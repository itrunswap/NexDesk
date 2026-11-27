use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU8, Ordering::Relaxed},
    },
    time::Duration,
};

use libwebrtc::{
    MediaType,
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    audio_track::RtcAudioTrack,
    media_stream_track::MediaStreamTrack,
    peer_connection::{
        AnswerOptions, IceGatheringState, OfferOptions, PeerConnection, PeerConnectionState,
    },
    peer_connection_factory::{
        ContinualGatheringPolicy, IceServer, IceTransportsType, PeerConnectionFactory,
        RtcConfiguration, native::PeerConnectionFactoryExt,
    },
    rtp_parameters::RtpCodecCapability,
    rtp_sender::{RtpSender, VideoEncoderBackend},
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
    stats::{IceCandidateType, QualityLimitationReason, RtcStats},
    video_source::{VideoResolution, native::NativeVideoSource},
    video_stream::native::{NativeVideoStream, NativeVideoStreamOptions},
    video_track::RtcVideoTrack,
};
use nexdesk_platform::{AUDIO_CHANNELS, AUDIO_RATE, AudioCapture, AudioStatus};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, mpsc},
    task::JoinHandle,
    time::{MissedTickBehavior, timeout},
};
use tokio_stream::StreamExt;

use crate::{
    CaptureStatus, CaptureTarget, ControlPipe, FrameSize, MediaError, PixelFrame, Quality,
    ReceivedSound,
    capture::{CaptureSettings, CaptureTask, FrameSink, PROBE_HEIGHT, PROBE_WIDTH},
    pixels::PixelPool,
    sound,
};

pub const DESCRIPTION_LIMIT: usize = 48 * 1024;
const ICE_DEADLINE: Duration = Duration::from_secs(5);
const MAX_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Codec {
    H264 = 1,
    Vp8 = 2,
    Av1 = 3,
}

impl Codec {
    pub fn from_value(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::H264,
            2 => Self::Vp8,
            3 => Self::Av1,
            _ => return None,
        })
    }

    fn mime(self) -> &'static str {
        match self {
            Self::H264 => "video/H264",
            Self::Vp8 => "video/VP8",
            Self::Av1 => "video/AV1",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EncodingPreference {
    pub codec: Option<Codec>,
    pub software: bool,
}

pub struct DesktopMediaSettings {
    pub capture: CaptureTarget,
    pub extended: bool,
    pub audio: bool,
    pub encoding: Option<EncodingPreference>,
}

#[derive(Clone, Copy, Debug)]
pub struct MediaConfig {
    pub capture: CaptureTarget,
    pub codec: Codec,
    pub fps: u16,
    pub audio_probe: bool,
    #[cfg(target_os = "windows")]
    pub prefer_mf_h264: bool,
}

#[derive(Clone, Default)]
pub enum IceOptions {
    #[default]
    Lan,
    Routed {
        servers: Vec<IceServer>,
        relay_only: bool,
    },
}

impl IceOptions {
    fn routed(&self) -> bool {
        matches!(self, Self::Routed { .. })
    }
}

pub struct HostMedia {
    cfg: MediaConfig,
    factory: PeerConnectionFactory,
    codecs: Vec<RtpCodecCapability>,
    active: AsyncMutex<Weak<Feed>>,
}

struct Feed {
    video: RtcVideoTrack,
    audio: Option<RtcAudioTrack>,
    capture: Mutex<CaptureTask>,
    sink: FrameSink,
    settings: Arc<CaptureSettings>,
    audio_task: Option<JoinHandle<()>>,
    system_sound: Option<Arc<Mutex<AudioCapture>>>,
    #[cfg(target_os = "windows")]
    pre_encoded: bool,
}

impl Drop for Feed {
    fn drop(&mut self) {
        if let Some(task) = &self.audio_task {
            task.abort();
        }
    }
}

pub struct HostPeer {
    peer: Peer,
    feed: Arc<Feed>,
    sender: RtpSender,
    applied_quality: Mutex<Option<(Quality, Option<u64>)>>,
    preferred_codec: Codec,
    negotiated_codec: Codec,
}

pub struct DesktopPipes {
    pub control: ControlPipe,
    pub collaboration: Option<ControlPipe>,
    pub files: Option<ControlPipe>,
    pub shell: Option<ControlPipe>,
}

impl DesktopPipes {
    fn new(peer: &PeerConnection, extended: bool, shell: bool) -> Result<Self, MediaError> {
        Ok(Self {
            control: ControlPipe::new(peer)?,
            collaboration: extended
                .then(|| ControlPipe::collaboration(peer))
                .transpose()?,
            files: extended.then(|| ControlPipe::files(peer)).transpose()?,
            shell: shell.then(|| ControlPipe::shell(peer)).transpose()?,
        })
    }
}

impl HostMedia {
    pub fn new(cfg: MediaConfig) -> Result<Self, MediaError> {
        if !(1..=60).contains(&cfg.fps) {
            return Err(MediaError::InvalidFrame);
        }
        let factory = PeerConnectionFactory::default();
        let codecs = factory.get_rtp_sender_capabilities(MediaType::Video).codecs;
        if !codecs
            .iter()
            .any(|codec| codec.mime_type.eq_ignore_ascii_case(cfg.codec.mime()))
        {
            return Err(MediaError::CodecUnavailable);
        }
        Ok(Self {
            cfg,
            factory,
            codecs,
            active: AsyncMutex::new(Weak::new()),
        })
    }

    pub async fn answer(&self, offer: &str) -> Result<(HostPeer, String), MediaError> {
        let (peer, answer, _) = self
            .answer_session(offer, None, IceOptions::Lan, false)
            .await?;
        Ok((peer, answer))
    }

    pub async fn answer_desktop(
        &self,
        offer: &str,
        settings: DesktopMediaSettings,
    ) -> Result<(HostPeer, String, DesktopPipes), MediaError> {
        let (peer, answer, pipe) = self
            .answer_session(offer, Some(settings), IceOptions::Lan, false)
            .await?;
        Ok((peer, answer, pipe.ok_or(MediaError::WebRtc)?))
    }

    pub async fn answer_routed(
        &self,
        offer: &str,
        settings: DesktopMediaSettings,
        ice: IceOptions,
        shell: bool,
    ) -> Result<(HostPeer, String, DesktopPipes), MediaError> {
        let (peer, answer, pipe) = self
            .answer_session(offer, Some(settings), ice, shell)
            .await?;
        Ok((peer, answer, pipe.ok_or(MediaError::WebRtc)?))
    }

    async fn answer_session(
        &self,
        offer: &str,
        settings: Option<DesktopMediaSettings>,
        ice: IceOptions,
        shell: bool,
    ) -> Result<(HostPeer, String, Option<DesktopPipes>), MediaError> {
        // Limit the offered video payload types without changing authenticated ICE or DTLS.
        let desktop = settings.is_some();
        let extended = settings.as_ref().is_some_and(|settings| settings.extended);
        let encoding = settings.as_ref().and_then(|settings| settings.encoding);
        let preferred = encoding
            .and_then(|encoding| encoding.codec)
            .unwrap_or(self.cfg.codec);
        let (chosen, codecs, offer) = self.select_codec(offer, preferred, encoding.is_some())?;
        let routed = ice.routed();
        let offer = checked_description(&offer, SdpType::Offer, desktop, routed)?;
        let peer = Peer::new(&self.factory, ice)?;
        let pipe = desktop
            .then(|| DesktopPipes::new(&peer.connection, extended, shell))
            .transpose()?;
        peer.connection.set_remote_description(offer).await?;
        let feed = if let Some(settings) = settings {
            self.create_feed(
                settings.capture,
                extended,
                settings.audio,
                chosen,
                encoding.is_none(),
            )
            .await?
        } else {
            self.feed().await?
        };
        let sender = peer
            .connection
            .add_track(MediaStreamTrack::Video(feed.video.clone()), &["desktop"])?;
        if let Some(encoding) = encoding {
            sender.set_video_encoder_backend(if encoding.software {
                VideoEncoderBackend::Software
            } else {
                VideoEncoderBackend::Hardware
            });
        } else {
            #[cfg(target_os = "windows")]
            if feed.pre_encoded {
                sender.set_video_encoder_backend(VideoEncoderBackend::PreEncoded);
            }
        }
        for transceiver in peer.connection.transceivers() {
            if matches!(
                transceiver.sender().track(),
                Some(MediaStreamTrack::Video(_))
            ) {
                transceiver.set_codec_preferences(codecs.clone())?;
            }
        }
        if let Some(track) = &feed.audio {
            peer.connection
                .add_track(MediaStreamTrack::Audio(track.clone()), &["desktop"])?;
        }
        let description = peer
            .connection
            .create_answer(AnswerOptions::default())
            .await?;
        let answer = gather(&peer, description, routed).await?;
        Ok((
            HostPeer {
                peer,
                feed,
                sender,
                applied_quality: Mutex::new(None),
                preferred_codec: preferred,
                negotiated_codec: chosen,
            },
            answer,
            pipe,
        ))
    }

    fn select_codec(
        &self,
        offer: &str,
        preferred: Codec,
        allow_fallback: bool,
    ) -> Result<(Codec, Vec<RtpCodecCapability>, String), MediaError> {
        let fallback = [Codec::H264, Codec::Vp8]
            .into_iter()
            .filter(|codec| allow_fallback && *codec != preferred);
        for codec in std::iter::once(preferred).chain(fallback) {
            let mut supported: Vec<_> = self
                .codecs
                .iter()
                .filter(|entry| entry.mime_type.eq_ignore_ascii_case(codec.mime()))
                .cloned()
                .collect();
            if supported.is_empty() {
                continue;
            }
            supported.extend(
                self.codecs
                    .iter()
                    .filter(|entry| entry.mime_type.eq_ignore_ascii_case("video/rtx"))
                    .cloned(),
            );
            match restrict_video_offer(offer, codec) {
                Ok(sdp) => return Ok((codec, supported, sdp)),
                Err(MediaError::CodecUnavailable) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(MediaError::CodecUnavailable)
    }

    async fn feed(&self) -> Result<Arc<Feed>, MediaError> {
        let mut active = self.active.lock().await;
        if let Some(feed) = active.upgrade() {
            return Ok(feed);
        }
        let feed = self
            .create_feed(self.cfg.capture, false, false, self.cfg.codec, true)
            .await?;
        *active = Arc::downgrade(&feed);
        Ok(feed)
    }

    async fn create_feed(
        &self,
        target: CaptureTarget,
        extended: bool,
        audio: bool,
        codec: Codec,
        legacy_encoder: bool,
    ) -> Result<Arc<Feed>, MediaError> {
        let (source, sink) = self.video_source(codec, legacy_encoder).await;
        #[cfg(target_os = "windows")]
        let pre_encoded = matches!(sink, FrameSink::Hardware(_));
        let video = self.factory.create_video_track("desktop", source);
        let settings = Arc::new(CaptureSettings::new(self.cfg.fps));
        let capture = CaptureTask::spawn(target, sink.clone(), settings.clone())?;
        let system_sound = if audio {
            AudioCapture::start()
                .ok()
                .map(|capture| Arc::new(Mutex::new(capture)))
        } else {
            None
        };
        let (audio, audio_task) = if let Some(capture) = &system_sound {
            let source = NativeAudioSource::new(
                AudioSourceOptions::default(),
                AUDIO_RATE,
                AUDIO_CHANNELS,
                40,
            );
            let track = self
                .factory
                .create_audio_track("system-audio", source.clone());
            (
                Some(track),
                Some(tokio::spawn(sound::send(source, capture.clone()))),
            )
        } else if !extended && self.cfg.audio_probe {
            let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 40);
            let track = self
                .factory
                .create_audio_track("probe-tone", source.clone());
            (Some(track), Some(tokio::spawn(audio_probe(source))))
        } else {
            (None, None)
        };
        Ok(Arc::new(Feed {
            video,
            audio,
            capture: Mutex::new(capture),
            sink,
            settings,
            audio_task,
            system_sound,
            #[cfg(target_os = "windows")]
            pre_encoded,
        }))
    }

    async fn video_source(
        &self,
        _codec: Codec,
        _legacy_encoder: bool,
    ) -> (NativeVideoSource, FrameSink) {
        #[cfg(target_os = "windows")]
        if _legacy_encoder && self.cfg.prefer_mf_h264 && _codec == Codec::H264 {
            let source = crate::windows_h264::source();
            let upstream = source.clone();
            let fps = self.cfg.fps;
            if let Ok(Ok(sink)) = tokio::task::spawn_blocking(move || {
                crate::windows_h264::HardwareSink::start(upstream, fps)
            })
            .await
            {
                return (source, FrameSink::Hardware(sink));
            }
        }
        let source = NativeVideoSource::new(
            VideoResolution {
                width: PROBE_WIDTH,
                height: PROBE_HEIGHT,
            },
            true,
        );
        (source.clone(), FrameSink::Raw(source))
    }
}

impl HostPeer {
    pub fn sound(&self) -> AudioStatus {
        self.feed
            .system_sound
            .as_ref()
            .and_then(|device| device.lock().ok())
            .map(|device| device.status())
            .unwrap_or_default()
    }

    pub fn fps(&self) -> u16 {
        self.feed.settings.fps.load(Relaxed)
    }

    pub fn set_quality(&self, quality: Quality) -> Result<(), MediaError> {
        let mut applied = self
            .applied_quality
            .lock()
            .map_err(|_| MediaError::WebRtc)?;
        let bitrate = quality.bitrate_limit_for(self.status().source_size());
        quality.apply(&self.sender, bitrate)?;
        self.feed.settings.set(quality);
        *applied = Some((quality, bitrate));
        Ok(())
    }

    pub fn refresh_quality(&self, capture: CaptureStatus) {
        let Some(size) = capture.source_size() else {
            return;
        };
        let Ok(mut applied) = self.applied_quality.lock() else {
            return;
        };
        let Some((quality, previous)) = applied.as_mut() else {
            return;
        };
        let bitrate = quality.bitrate_limit_for(Some(size));
        if *previous != bitrate && quality.apply(&self.sender, bitrate).is_ok() {
            *previous = bitrate;
        }
    }

    pub fn stop_capture(&self) {
        if let Ok(task) = self.feed.capture.lock() {
            task.stop();
        }
    }
    pub fn status(&self) -> CaptureStatus {
        self.feed.capture.lock().map_or(
            CaptureStatus {
                unavailable: true,
                ..CaptureStatus::default()
            },
            |task| task.status(),
        )
    }

    pub fn switch_source(&self, target: CaptureTarget) -> Result<(), MediaError> {
        let mut current = self
            .feed
            .capture
            .lock()
            .map_err(|_| MediaError::CaptureUnavailable)?;
        let next = CaptureTask::spawn(target, self.feed.sink.clone(), self.feed.settings.clone())?;
        *current = next;
        Ok(())
    }

    pub fn has_ended(&self) -> bool {
        matches!(
            self.peer.connection.connection_state(),
            PeerConnectionState::Failed
                | PeerConnectionState::Closed
                | PeerConnectionState::Disconnected
        )
    }

    pub async fn stats(&self) -> Result<MediaStats, MediaError> {
        let mut stats = self.peer.stats(true).await?;
        let capture = self.status();
        stats.source_size = capture.source_size();
        stats.bitrate_ceiling_bps = self
            .applied_quality
            .lock()
            .ok()
            .and_then(|value| (*value)?.1);
        stats.capture_size = capture.frame_size;
        stats.capture_work_ms = capture
            .frame_size
            .map(|_| capture.last_frame_work_us as f64 / 1000.0);
        if capture.frame_size.is_some() {
            stats.prepare_ms = Some(capture.preparation_us as f64 / 1000.0);
            stats.submit_ms = Some(capture.submission_us as f64 / 1000.0);
        }
        stats.requested_codec = Some(self.preferred_codec);
        stats.negotiated_codec = Some(self.negotiated_codec);
        Ok(stats)
    }
}

async fn audio_probe(source: NativeAudioSource) {
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut phase = 0.0f32;
    let mut frame = AudioFrame::new(48_000, 1, 480);
    let increment = std::f32::consts::TAU * 440.0 / 48_000.0;
    loop {
        clock.tick().await;
        for sample in frame.data.to_mut() {
            *sample = (phase.sin() * 900.0) as i16;
            phase = (phase + increment) % std::f32::consts::TAU;
        }
        if source.capture_frame(&frame).await.is_err() {
            return;
        }
    }
}

struct Peer {
    connection: PeerConnection,
    relay_only: bool,
}

impl Peer {
    fn new(factory: &PeerConnectionFactory, ice: IceOptions) -> Result<Self, MediaError> {
        let mut cfg = RtcConfiguration::default();
        cfg.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
        let relay_only = matches!(
            &ice,
            IceOptions::Routed {
                relay_only: true,
                ..
            }
        );
        if let IceOptions::Routed { servers, .. } = ice {
            cfg.ice_servers = servers;
            if relay_only {
                cfg.ice_transport_type = IceTransportsType::Relay;
            }
        }
        Ok(Self {
            connection: factory.create_peer_connection(cfg)?,
            relay_only,
        })
    }

    async fn stats(&self, sending: bool) -> Result<MediaStats, MediaError> {
        let entries = self.connection.get_stats().await?;
        Ok(MediaStats::from_rtc(entries, sending, self.relay_only))
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.connection.close();
    }
}

pub struct ControllerOffer {
    peer: Peer,
    video: mpsc::Receiver<RtcVideoTrack>,
    audio: Option<mpsc::Receiver<RtcAudioTrack>>,
    sdp: String,
    pipes: Option<DesktopPipes>,
    routed: bool,
}

impl ControllerOffer {
    pub async fn new() -> Result<Self, MediaError> {
        Self::with_profile(false, false, IceOptions::Lan, false).await
    }

    pub async fn desktop() -> Result<Self, MediaError> {
        Self::with_profile(true, false, IceOptions::Lan, false).await
    }

    pub async fn collaboration() -> Result<Self, MediaError> {
        Self::with_profile(true, true, IceOptions::Lan, false).await
    }

    pub async fn routed(ice: IceOptions, shell: bool) -> Result<Self, MediaError> {
        Self::with_profile(true, true, ice, shell).await
    }

    async fn with_profile(
        desktop: bool,
        extended: bool,
        ice: IceOptions,
        shell: bool,
    ) -> Result<Self, MediaError> {
        let routed = ice.routed();
        let peer = Peer::new(&PeerConnectionFactory::default(), ice)?;
        let pipes = desktop
            .then(|| DesktopPipes::new(&peer.connection, extended, shell))
            .transpose()?;
        let (track_sender, video) = mpsc::channel(1);
        let (audio_sender, audio) = mpsc::channel(1);
        peer.connection
            .on_track(Some(Box::new(move |event| match event.track {
                MediaStreamTrack::Video(track) => {
                    let _ = track_sender.try_send(track);
                }
                MediaStreamTrack::Audio(track) if extended => {
                    let _ = audio_sender.try_send(track);
                }
                _ => {}
            })));
        for media in [MediaType::Video, MediaType::Audio] {
            peer.connection.add_transceiver_for_media(
                media,
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::RecvOnly,
                    stream_ids: Vec::new(),
                    send_encodings: Vec::new(),
                },
            )?;
        }
        let description = peer
            .connection
            .create_offer(OfferOptions {
                ice_restart: false,
                offer_to_receive_video: None,
                offer_to_receive_audio: None,
            })
            .await?;
        let sdp = gather(&peer, description, routed).await?;
        Ok(Self {
            peer,
            video,
            audio: extended.then_some(audio),
            sdp,
            pipes,
            routed,
        })
    }

    pub fn sdp(&self) -> &str {
        &self.sdp
    }

    pub async fn apply_answer(mut self, sdp: &str) -> Result<View, MediaError> {
        let answer = checked_description(sdp, SdpType::Answer, self.pipes.is_some(), self.routed)?;
        self.peer.connection.set_remote_description(answer).await?;
        let track = timeout(Duration::from_secs(8), self.video.recv())
            .await
            .map_err(|_| MediaError::VideoTimeout)?
            .ok_or(MediaError::VideoTimeout)?;
        let video = NativeVideoStream::with_options(
            track,
            NativeVideoStreamOptions {
                queue_size_frames: Some(1),
            },
        );
        Ok(View {
            pixels: PixelPool::default(),
            color_conversion: None,
            peer: self.peer,
            video,
            audio: self.audio.take(),
            pipes: self.pipes.take(),
        })
    }
}

pub struct View {
    pixels: PixelPool,
    color_conversion: Option<&'static str>,
    peer: Peer,
    video: NativeVideoStream,
    audio: Option<mpsc::Receiver<RtcAudioTrack>>,
    pipes: Option<DesktopPipes>,
}

impl View {
    pub fn listen_sound(&mut self, volume: Arc<AtomicU8>) -> Option<ReceivedSound> {
        self.audio
            .take()
            .map(|track| ReceivedSound::start(track, volume))
    }

    pub fn take_control(&mut self) -> Option<ControlPipe> {
        self.take_pipes().map(|pipes| pipes.control)
    }

    pub fn take_pipes(&mut self) -> Option<DesktopPipes> {
        self.pipes.take()
    }
    pub async fn next_frame(&mut self) -> Result<Option<PixelFrame>, MediaError> {
        let Some(frame) = self.video.next().await else {
            return Ok(None);
        };
        let pixels = PixelFrame::decode(frame, &self.pixels)?;
        self.color_conversion = Some(pixels.color_conversion());
        Ok(Some(pixels))
    }

    pub async fn stats(&self) -> Result<MediaStats, MediaError> {
        let stats = self.peer.stats(false).await?;
        let stats = MediaStats {
            color_conversion: self.color_conversion.map(str::to_owned),
            ..stats
        };
        Ok(stats)
    }

    pub fn has_ended(&self) -> bool {
        matches!(
            self.peer.connection.connection_state(),
            PeerConnectionState::Failed
                | PeerConnectionState::Closed
                | PeerConnectionState::Disconnected
        )
    }
}

type GatheredCandidates = HashMap<String, Vec<String>>;

async fn gather(
    peer: &Peer,
    description: SessionDescription,
    routed: bool,
) -> Result<String, MediaError> {
    let sdp = description.to_string();
    let relay_only = peer.relay_only;
    let peer = &peer.connection;
    let candidates = Arc::new(Mutex::new(GatheredCandidates::new()));
    let received = Arc::clone(&candidates);
    peer.on_ice_candidate(Some(Box::new(move |candidate| {
        if let Ok(mut received) = received.lock() {
            let count = received.values().map(Vec::len).sum::<usize>();
            let line = candidate.candidate();
            if count < MAX_CANDIDATES && valid_candidate(&line, routed, relay_only) {
                received.entry(candidate.sdp_mid()).or_default().push(line);
            }
        }
    })));
    let done = Arc::new(Notify::new());
    let signal = Arc::clone(&done);
    peer.on_ice_gathering_state_change(Some(Box::new(move |state| {
        if state == IceGatheringState::Complete {
            signal.notify_one();
        }
    })));
    peer.set_local_description(description).await?;
    if peer.ice_gathering_state() != IceGatheringState::Complete {
        let result = timeout(ICE_DEADLINE, done.notified()).await;
        if result.is_err() && !routed {
            return Err(MediaError::IceTimeout);
        }
    }
    peer.on_ice_candidate(None);
    peer.on_ice_gathering_state_change(None);
    let received = candidates.lock().map_err(|_| MediaError::WebRtc)?;
    if received.is_empty() {
        return Err(MediaError::IceTimeout);
    }
    Ok(with_candidates(&sdp, &received))
}

fn with_candidates(sdp: &str, candidates: &GatheredCandidates) -> String {
    let mut gathered = String::with_capacity(sdp.len() + 1024);
    for line in sdp.lines() {
        gathered.push_str(line);
        gathered.push_str("\r\n");
        if let Some(entries) = line
            .strip_prefix("a=mid:")
            .and_then(|mid| candidates.get(mid))
        {
            for candidate in entries {
                gathered.push_str("a=");
                gathered.push_str(candidate);
                gathered.push_str("\r\n");
            }
        }
    }
    gathered
}

fn checked_description(
    sdp: &str,
    kind: SdpType,
    desktop: bool,
    routed: bool,
) -> Result<SessionDescription, MediaError> {
    if sdp.is_empty() || sdp.len() > DESCRIPTION_LIMIT {
        return Err(MediaError::InvalidDescription);
    }
    let mut fingerprint_seen = false;
    let mut candidates = 0;
    let mut video = 0;
    let mut audio = 0;
    let mut data = 0;
    let mut accepted = String::with_capacity(sdp.len());
    for line in sdp.lines() {
        if let Some(media) = line.strip_prefix("m=") {
            match media.split_ascii_whitespace().next() {
                Some("video") => video += 1,
                Some("audio") => audio += 1,
                Some("application")
                    if desktop && media.ends_with(" UDP/DTLS/SCTP webrtc-datachannel") =>
                {
                    data += 1
                }
                _ => return Err(MediaError::InvalidDescription),
            }
        }
        if let Some(fingerprint) = line.strip_prefix("a=fingerprint:") {
            let Some(hex) = fingerprint.strip_prefix("sha-256 ") else {
                return Err(MediaError::InvalidDescription);
            };
            if hex.split(':').count() != 32
                || !hex.split(':').all(|octet| {
                    octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            {
                return Err(MediaError::InvalidDescription);
            }
            fingerprint_seen = true;
        }
        if let Some(candidate) = line.strip_prefix("a=")
            && candidate.starts_with("candidate:")
        {
            if !valid_candidate(candidate, routed, false) {
                continue;
            }
            candidates += 1;
        }
        accepted.push_str(line);
        accepted.push_str("\r\n");
    }
    if !fingerprint_seen
        || candidates == 0
        || candidates > MAX_CANDIDATES
        || video != 1
        || audio > 1
        || data != usize::from(desktop)
    {
        return Err(MediaError::InvalidDescription);
    }
    SessionDescription::parse(&accepted, kind).map_err(|_| MediaError::InvalidDescription)
}

fn restrict_video_offer(sdp: &str, codec: Codec) -> Result<String, MediaError> {
    if sdp.len() > DESCRIPTION_LIMIT {
        return Err(MediaError::InvalidDescription);
    }
    let lines = sdp.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.starts_with("m=video "))
        .ok_or(MediaError::InvalidDescription)?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("m="))
        .map_or(lines.len(), |offset| start + 1 + offset);
    let (_, encoding) = codec
        .mime()
        .split_once('/')
        .ok_or(MediaError::CodecUnavailable)?;
    let media = lines[start].split_ascii_whitespace().collect::<Vec<_>>();
    if media.len() < 4 {
        return Err(MediaError::InvalidDescription);
    }
    let mut primary = HashSet::new();
    let mut rtx = HashSet::new();
    for line in &lines[start + 1..end] {
        if let Some((payload, description)) = line
            .strip_prefix("a=rtpmap:")
            .and_then(|entry| entry.split_once(' '))
            && media[3..].contains(&payload)
            && let Some((name, _clock)) = description.split_once('/')
        {
            if name.eq_ignore_ascii_case(encoding) {
                primary.insert(payload);
            }
            if name.eq_ignore_ascii_case("rtx") {
                rtx.insert(payload);
            }
        }
    }
    if primary.is_empty() {
        return Err(MediaError::CodecUnavailable);
    }
    let mut allowed = primary.clone();
    for line in &lines[start + 1..end] {
        if let Some((pt, parameters)) = line.strip_prefix("a=fmtp:").and_then(|s| s.split_once(' '))
            && rtx.contains(pt)
        {
            let mut apt = parameters
                .split(';')
                .filter_map(|part| part.trim().split_once('='))
                .filter(|(key, _)| key.trim().eq_ignore_ascii_case("apt"));
            if let Some((_, target)) = apt.next()
                && primary.contains(target.trim())
                && apt.next().is_none()
            {
                allowed.insert(pt);
            }
        }
    }
    let formats = media[3..]
        .iter()
        .copied()
        .filter(|pt| allowed.contains(pt))
        .collect::<Vec<_>>();

    let mut output = lines[..start].join("\r\n");
    if !output.is_empty() {
        output.push_str("\r\n");
    }
    output.push_str(&media[..3].join(" "));
    output.push(' ');
    output.push_str(&formats.join(" "));
    output.push_str("\r\n");
    for line in &lines[start + 1..end] {
        let payload = ["a=rtpmap:", "a=fmtp:", "a=rtcp-fb:"]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .and_then(|entry| entry.split_ascii_whitespace().next());
        if payload.is_none_or(|pt| pt == "*" || allowed.contains(pt)) {
            output.push_str(line);
            output.push_str("\r\n");
        }
    }
    for line in &lines[end..] {
        output.push_str(line);
        output.push_str("\r\n");
    }
    Ok(output)
}

fn valid_candidate(line: &str, routed: bool, relay_only: bool) -> bool {
    if line.len() > 512 || line.contains(['\r', '\n', '\0']) || !line.starts_with("candidate:") {
        return false;
    }
    let mut fields = line.split_ascii_whitespace();
    let Some(transport) = fields.nth(2) else {
        return false;
    };
    let Some(address) = fields.nth(1) else {
        return false;
    };
    let Some(port) = fields.next().and_then(|port| port.parse::<u16>().ok()) else {
        return false;
    };
    if port == 0 || fields.next() != Some("typ") {
        return false;
    }
    let candidate_type = fields.next();
    if relay_only && candidate_type != Some("relay")
        || !(transport.eq_ignore_ascii_case("udp")
            || routed && transport.eq_ignore_ascii_case("tcp"))
        || !(candidate_type == Some("host")
            || routed && matches!(candidate_type, Some("relay" | "srflx" | "prflx")))
    {
        return false;
    }
    match address.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(ip) if routed => {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && !matches!(ip, IpAddr::V4(v4) if v4.is_broadcast())
        }
        Ok(IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => address.strip_suffix(".local").is_some_and(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }),
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MediaStats {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_frames: u32,
    pub video_bytes: u64,
    pub dropped_frames: u32,
    pub lost_packets: i64,
    pub retransmitted_packets: Option<u64>,
    pub average_jitter_ms: Option<f64>,
    pub audio_packets: u64,
    pub codec_implementation: Option<String>,
    pub power_efficient: Option<bool>,
    pub average_codec_ms: Option<f64>,
    pub ice: Option<IcePathStats>,
    pub source_size: Option<FrameSize>,
    pub capture_size: Option<FrameSize>,
    pub frame_size: Option<FrameSize>,
    pub capture_work_ms: Option<f64>,
    pub prepare_ms: Option<f64>,
    pub submit_ms: Option<f64>,
    pub color_conversion: Option<String>,
    pub target_bitrate_bps: Option<u64>,
    pub bitrate_ceiling_bps: Option<u64>,
    pub limitation: Option<VideoLimitation>,
    pub requested_codec: Option<Codec>,
    pub negotiated_codec: Option<Codec>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoLimitation {
    None,
    Cpu,
    Bandwidth,
    Other,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IcePathStats {
    pub protocol: String,
    pub local_scope: String,
    pub remote_scope: String,
    pub relayed: bool,
    pub rtt_ms: f64,
}

impl MediaStats {
    fn from_rtc(entries: Vec<RtcStats>, sending: bool, relay_only: bool) -> Self {
        let codecs: HashMap<&str, &str> = entries
            .iter()
            .filter_map(|entry| match entry {
                RtcStats::Codec(codec) => {
                    Some((codec.rtc.id.as_str(), codec.codec.mime_type.as_str()))
                }
                _ => None,
            })
            .collect();
        let codec = |id: &str| codecs.get(id).map(|name| (*name).to_owned());
        let mut result = Self {
            ice: ice_path(&entries, relay_only),
            ..Self::default()
        };
        for entry in &entries {
            match entry {
                RtcStats::OutboundRtp(stats) if sending => {
                    if stats.stream.kind == "video" {
                        result.video_codec = codec(&stats.stream.codec_id);
                        result.video_frames = stats.outbound.frames_encoded;
                        result.retransmitted_packets =
                            Some(stats.outbound.retransmitted_packets_sent);
                        result.video_bytes = stats.sent.bytes_sent;
                        result.frame_size =
                            measured_size(stats.outbound.frame_width, stats.outbound.frame_height);
                        result.target_bitrate_bps = (stats.outbound.target_bitrate > 0.0)
                            .then_some(stats.outbound.target_bitrate as u64);
                        if result.frame_size.is_some() {
                            result.limitation =
                                Some(match stats.outbound.quality_limitation_reason {
                                    QualityLimitationReason::None => VideoLimitation::None,
                                    QualityLimitationReason::Cpu => VideoLimitation::Cpu,
                                    QualityLimitationReason::Bandwidth => {
                                        VideoLimitation::Bandwidth
                                    }
                                    QualityLimitationReason::Other => VideoLimitation::Other,
                                });
                        }
                        result.set_video_path(
                            &stats.outbound.encoder_implementation,
                            stats.outbound.power_efficient_encoder,
                            stats.outbound.total_encode_time,
                        );
                    } else if stats.stream.kind == "audio" {
                        result.audio_codec = codec(&stats.stream.codec_id);
                        result.audio_packets = stats.sent.packets_sent;
                    }
                }
                RtcStats::InboundRtp(stats) if !sending => {
                    if stats.stream.kind == "video" {
                        result.video_codec = codec(&stats.stream.codec_id);
                        result.video_frames = stats.inbound.frames_decoded;
                        result.retransmitted_packets =
                            Some(stats.inbound.retransmitted_packets_received);
                        result.video_bytes = stats.inbound.bytes_received;
                        result.frame_size =
                            measured_size(stats.inbound.frame_width, stats.inbound.frame_height);
                        result.dropped_frames = stats.inbound.frames_dropped;
                        result.lost_packets = stats.received.packets_lost;
                        if stats.inbound.jitter_buffer_emitted_count > 0 {
                            result.average_jitter_ms = Some(
                                stats.inbound.jitter_buffer_delay * 1000.0
                                    / stats.inbound.jitter_buffer_emitted_count as f64,
                            );
                        }
                        result.set_video_path(
                            &stats.inbound.decoder_implementation,
                            stats.inbound.power_efficient_decoder,
                            stats.inbound.total_decode_time,
                        );
                    } else if stats.stream.kind == "audio" {
                        result.audio_codec = codec(&stats.stream.codec_id);
                        result.audio_packets = stats.received.packets_received;
                    }
                }
                _ => {}
            }
        }
        result
    }

    fn set_video_path(&mut self, implementation: &str, efficient: bool, elapsed: f64) {
        if !implementation.is_empty() {
            self.codec_implementation = Some(implementation.to_owned());
            self.power_efficient = Some(efficient);
        }
        if self.video_frames > 0 {
            self.average_codec_ms = Some(elapsed * 1000.0 / f64::from(self.video_frames));
        }
    }
}

fn measured_size(width: u32, height: u32) -> Option<FrameSize> {
    (width > 0 && height > 0).then_some(FrameSize { width, height })
}

fn ice_path(entries: &[RtcStats], relay_only: bool) -> Option<IcePathStats> {
    let id = entries.iter().find_map(|entry| match entry {
        RtcStats::Transport(stats) if !stats.transport.selected_candidate_pair_id.is_empty() => {
            Some(stats.transport.selected_candidate_pair_id.as_str())
        }
        _ => None,
    })?;
    let pair = entries.iter().find_map(|entry| match entry {
        RtcStats::CandidatePair(stats) if stats.rtc.id == id => Some(&stats.candidate_pair),
        _ => None,
    })?;
    let local = entries.iter().find_map(|entry| match entry {
        RtcStats::LocalCandidate(stats) if stats.rtc.id == pair.local_candidate_id => {
            Some(&stats.local_candidate)
        }
        _ => None,
    })?;
    let remote = entries.iter().find_map(|entry| match entry {
        RtcStats::RemoteCandidate(stats) if stats.rtc.id == pair.remote_candidate_id => {
            Some(&stats.remote_candidate)
        }
        _ => None,
    })?;
    Some(IcePathStats {
        protocol: local.protocol.clone(),
        local_scope: address_scope(&local.address).to_owned(),
        remote_scope: address_scope(&remote.address).to_owned(),
        relayed: relay_only
            || [local.candidate_type, remote.candidate_type]
                .contains(&Some(IceCandidateType::Relay)),
        rtt_ms: pair.current_round_trip_time * 1000.0,
    })
}

fn address_scope(address: &str) -> &'static str {
    match address.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(ip) if ip.is_loopback() => "loopback",
        Ok(IpAddr::V4(ip)) if ip.is_private() || ip.is_link_local() => "private",
        Ok(IpAddr::V6(ip)) if ip.is_unique_local() || ip.is_unicast_link_local() => "private",
        Ok(_) => "other",
        Err(_) if address.ends_with(".local") => "local-name",
        Err(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_only_signals_only_turn_candidates() {
        let host = "candidate:1 1 udp 2122260223 192.168.1.10 50102 typ host";
        let relay = "candidate:2 1 udp 41754367 192.0.2.1 49160 typ relay";
        assert!(valid_candidate(host, true, false));
        assert!(!valid_candidate(host, true, true));
        assert!(valid_candidate(relay, true, true));
        assert!(!valid_candidate(relay, false, false));
    }

    #[test]
    fn strict_codec_keeps_dtls_and_other_tracks() -> Result<(), MediaError> {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96 97 102\r\n",
            "a=fingerprint:sha-256 test-marker\r\n",
            "a=rtpmap:96 VP8/90000\r\n",
            "a=rtpmap:97 rtx/90000\r\n",
            "a=fmtp:97 apt=96\r\n",
            "a=rtpmap:102 H264/90000\r\n",
            "a=fmtp:102 packetization-mode=1\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
            "a=rtpmap:111 opus/48000/2\r\n",
        );
        let restricted = restrict_video_offer(offer, Codec::H264)?;
        assert!(restricted.contains("m=video 9 UDP/TLS/RTP/SAVPF 102\r\n"));
        assert!(restricted.contains("a=fingerprint:sha-256 test-marker\r\n"));
        assert!(restricted.contains("a=fmtp:102 packetization-mode=1\r\n"));
        assert!(restricted.contains("a=rtpmap:111 opus/48000/2\r\n"));
        assert!(!restricted.contains("VP8"));
        assert!(!restricted.contains("apt=96"));
        Ok(())
    }

    #[test]
    fn retransmissions_follow_the_selected_payload_in_offer_order() -> Result<(), MediaError> {
        let offer = concat!(
            "m=video 9 UDP/TLS/RTP/SAVPF 97 115 116 114 111\r\n",
            "a=fmtp:115 rtx-time=300;apt=114\r\na=rtpmap:115 rtx/90000\r\n",
            "a=rtpmap:116 rtx/90000\r\na=fmtp:116 apt=115\r\n",
            "a=rtpmap:97 VP8/90000\r\na=rtpmap:114 H264/90000\r\n",
            "a=rtpmap:111 rtx/90000\r\na=fmtp:111 apt=97\r\n",
            "a=rtcp-fb:114 nack pli\r\na=rtcp-fb:* transport-cc\r\n"
        );
        let answer = restrict_video_offer(offer, Codec::H264)?;
        assert!(answer.starts_with("m=video 9 UDP/TLS/RTP/SAVPF 115 114\r\n"));
        assert!(answer.contains("a=fmtp:115 rtx-time=300;apt=114"));
        assert!(answer.contains("a=rtcp-fb:114 nack pli"));
        assert!(answer.contains("a=rtcp-fb:* transport-cc"));
        assert!(!answer.contains("apt=115") && !answer.contains("apt=97"));
        Ok(())
    }
}
