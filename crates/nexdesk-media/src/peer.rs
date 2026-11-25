use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU8, AtomicU16, Ordering::Relaxed},
    },
    time::{Duration, Instant},
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
    rtp_sender::RtpSender,
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
    stats::{IceCandidateType, RtcStats},
    video_frame::{BoxVideoFrame, VideoFormatType},
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
    CaptureStatus, CaptureTarget, ControlPipe, MediaError, Quality, ReceivedSound,
    capture::{CaptureTask, FrameSink, PROBE_HEIGHT, PROBE_WIDTH, valid_size},
    sound,
};

pub const DESCRIPTION_LIMIT: usize = 48 * 1024;
const ICE_DEADLINE: Duration = Duration::from_secs(5);
const MAX_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Codec {
    H264,
    Vp8,
}

impl Codec {
    fn mime(self) -> &'static str {
        match self {
            Self::H264 => "video/H264",
            Self::Vp8 => "video/VP8",
        }
    }
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
    fps: Arc<AtomicU16>,
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
        let codecs = factory
            .get_rtp_sender_capabilities(MediaType::Video)
            .codecs
            .into_iter()
            .filter(|candidate| candidate.mime_type.eq_ignore_ascii_case(cfg.codec.mime()))
            .collect::<Vec<_>>();
        if codecs.is_empty() {
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
            .answer_session(offer, None, false, false, IceOptions::Lan, false)
            .await?;
        Ok((peer, answer))
    }

    pub async fn answer_desktop(
        &self,
        offer: &str,
        target: CaptureTarget,
        extended: bool,
        audio: bool,
    ) -> Result<(HostPeer, String, DesktopPipes), MediaError> {
        let (peer, answer, pipe) = self
            .answer_session(offer, Some(target), extended, audio, IceOptions::Lan, false)
            .await?;
        Ok((peer, answer, pipe.ok_or(MediaError::WebRtc)?))
    }

    pub async fn answer_routed(
        &self,
        offer: &str,
        target: CaptureTarget,
        audio: bool,
        ice: IceOptions,
        shell: bool,
    ) -> Result<(HostPeer, String, DesktopPipes), MediaError> {
        let (peer, answer, pipe) = self
            .answer_session(offer, Some(target), true, audio, ice, shell)
            .await?;
        Ok((peer, answer, pipe.ok_or(MediaError::WebRtc)?))
    }

    async fn answer_session(
        &self,
        offer: &str,
        target: Option<CaptureTarget>,
        extended: bool,
        audio: bool,
        ice: IceOptions,
        shell: bool,
    ) -> Result<(HostPeer, String, Option<DesktopPipes>), MediaError> {
        // Limit the offered video payload types without changing authenticated ICE or DTLS.
        let desktop = target.is_some();
        let offer = restrict_video_offer(offer, self.cfg.codec)?;
        let routed = ice.routed();
        let offer = checked_description(&offer, SdpType::Offer, desktop, routed)?;
        let peer = Peer::new(&self.factory, ice)?;
        let pipe = desktop
            .then(|| DesktopPipes::new(&peer.0, extended, shell))
            .transpose()?;
        peer.0.set_remote_description(offer).await?;
        let feed = if let Some(target) = target {
            self.create_feed(target, extended, audio).await?
        } else {
            self.feed().await?
        };
        let sender = peer
            .0
            .add_track(MediaStreamTrack::Video(feed.video.clone()), &["desktop"])?;
        #[cfg(target_os = "windows")]
        if feed.pre_encoded {
            sender
                .set_video_encoder_backend(libwebrtc::rtp_sender::VideoEncoderBackend::PreEncoded);
        }
        for transceiver in peer.0.transceivers() {
            if matches!(
                transceiver.sender().track(),
                Some(MediaStreamTrack::Video(_))
            ) {
                transceiver.set_codec_preferences(self.codecs.clone())?;
            }
        }
        if let Some(track) = &feed.audio {
            peer.0
                .add_track(MediaStreamTrack::Audio(track.clone()), &["desktop"])?;
        }
        let description = peer.0.create_answer(AnswerOptions::default()).await?;
        let answer = gather(&peer.0, description, routed).await?;
        Ok((HostPeer { peer, feed, sender }, answer, pipe))
    }

    async fn feed(&self) -> Result<Arc<Feed>, MediaError> {
        let mut active = self.active.lock().await;
        if let Some(feed) = active.upgrade() {
            return Ok(feed);
        }
        let feed = self.create_feed(self.cfg.capture, false, false).await?;
        *active = Arc::downgrade(&feed);
        Ok(feed)
    }

    async fn create_feed(
        &self,
        target: CaptureTarget,
        extended: bool,
        audio: bool,
    ) -> Result<Arc<Feed>, MediaError> {
        let (source, sink) = self.video_source().await;
        #[cfg(target_os = "windows")]
        let pre_encoded = matches!(sink, FrameSink::Hardware(_));
        let video = self.factory.create_video_track("desktop", source);
        let fps = Arc::new(AtomicU16::new(self.cfg.fps));
        let capture = CaptureTask::spawn(target, sink.clone(), fps.clone())?;
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
            fps,
            audio_task,
            system_sound,
            #[cfg(target_os = "windows")]
            pre_encoded,
        }))
    }

    async fn video_source(&self) -> (NativeVideoSource, FrameSink) {
        #[cfg(target_os = "windows")]
        if self.cfg.prefer_mf_h264 && self.cfg.codec == Codec::H264 {
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
        self.feed.fps.load(Relaxed)
    }

    pub fn set_quality(&self, quality: Quality) -> Result<(), MediaError> {
        quality.apply(&self.sender)?;
        self.feed.fps.store(quality.fps, Relaxed);
        Ok(())
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
        let next = CaptureTask::spawn(target, self.feed.sink.clone(), self.feed.fps.clone())?;
        *current = next;
        Ok(())
    }

    pub fn has_ended(&self) -> bool {
        matches!(
            self.peer.0.connection_state(),
            PeerConnectionState::Failed
                | PeerConnectionState::Closed
                | PeerConnectionState::Disconnected
        )
    }

    pub async fn stats(&self) -> Result<MediaStats, MediaError> {
        Ok(MediaStats::from_rtc(self.peer.0.get_stats().await?, true))
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

struct Peer(PeerConnection);

impl Peer {
    fn new(factory: &PeerConnectionFactory, ice: IceOptions) -> Result<Self, MediaError> {
        let mut cfg = RtcConfiguration::default();
        cfg.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
        if let IceOptions::Routed {
            servers,
            relay_only,
        } = ice
        {
            cfg.ice_servers = servers;
            if relay_only {
                cfg.ice_transport_type = IceTransportsType::Relay;
            }
        }
        Ok(Self(factory.create_peer_connection(cfg)?))
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.0.close();
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
            .then(|| DesktopPipes::new(&peer.0, extended, shell))
            .transpose()?;
        let (track_sender, video) = mpsc::channel(1);
        let (audio_sender, audio) = mpsc::channel(1);
        peer.0
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
            peer.0.add_transceiver_for_media(
                media,
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::RecvOnly,
                    stream_ids: Vec::new(),
                    send_encodings: Vec::new(),
                },
            )?;
        }
        let description = peer
            .0
            .create_offer(OfferOptions {
                ice_restart: false,
                offer_to_receive_video: None,
                offer_to_receive_audio: None,
            })
            .await?;
        let sdp = gather(&peer.0, description, routed).await?;
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
        self.peer.0.set_remote_description(answer).await?;
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
            peer: self.peer,
            video,
            audio: self.audio.take(),
            pipes: self.pipes.take(),
        })
    }
}

pub struct View {
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
        to_pixel_frame(frame).map(Some)
    }

    pub async fn stats(&self) -> Result<MediaStats, MediaError> {
        Ok(MediaStats::from_rtc(self.peer.0.get_stats().await?, false))
    }

    pub fn has_ended(&self) -> bool {
        matches!(
            self.peer.0.connection_state(),
            PeerConnectionState::Failed
                | PeerConnectionState::Closed
                | PeerConnectionState::Disconnected
        )
    }
}

pub struct PixelFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub decoded_at: Instant,
}

fn to_pixel_frame(frame: BoxVideoFrame) -> Result<PixelFrame, MediaError> {
    let decoded_at = Instant::now();
    let width = frame.buffer.width();
    let height = frame.buffer.height();
    if !valid_size(width, height) {
        return Err(MediaError::InvalidFrame);
    }
    let stride = width * 4;
    let mut rgba = vec![0; (stride * height) as usize];
    // ABGR in libyuv notation has RGBA byte order on our little-endian desktop targets.
    frame.buffer.to_argb(
        VideoFormatType::ABGR,
        &mut rgba,
        stride,
        width as i32,
        height as i32,
    );
    Ok(PixelFrame {
        rgba,
        width,
        height,
        decoded_at,
    })
}

type GatheredCandidates = HashMap<String, Vec<String>>;

async fn gather(
    peer: &PeerConnection,
    description: SessionDescription,
    routed: bool,
) -> Result<String, MediaError> {
    let sdp = description.to_string();
    let candidates = Arc::new(Mutex::new(GatheredCandidates::new()));
    let received = Arc::clone(&candidates);
    peer.on_ice_candidate(Some(Box::new(move |candidate| {
        if let Ok(mut received) = received.lock() {
            let count = received.values().map(Vec::len).sum::<usize>();
            let line = candidate.candidate();
            if count < MAX_CANDIDATES && valid_candidate(&line, routed) {
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
            if !valid_candidate(candidate, routed) {
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
    let mut allowed = HashSet::new();
    for line in &lines[start + 1..end] {
        if let Some((payload, description)) = line
            .strip_prefix("a=rtpmap:")
            .and_then(|entry| entry.split_once(' '))
            && description
                .split('/')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(encoding))
        {
            allowed.insert(payload);
        }
    }
    let media = lines[start].split_ascii_whitespace().collect::<Vec<_>>();
    if media.len() < 4 {
        return Err(MediaError::InvalidDescription);
    }
    let formats = media[3..]
        .iter()
        .copied()
        .filter(|pt| allowed.contains(pt))
        .collect::<Vec<_>>();
    if formats.is_empty() {
        return Err(MediaError::CodecUnavailable);
    }

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

fn valid_candidate(line: &str, routed: bool) -> bool {
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
    if !(transport.eq_ignore_ascii_case("udp") || routed && transport.eq_ignore_ascii_case("tcp"))
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
pub struct MediaStats {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_frames: u32,
    pub video_bytes: u64,
    pub dropped_frames: u32,
    pub lost_packets: i64,
    pub average_jitter_ms: Option<f64>,
    pub audio_packets: u64,
    pub codec_implementation: Option<String>,
    pub power_efficient: Option<bool>,
    pub average_codec_ms: Option<f64>,
    pub ice: Option<IcePathStats>,
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
    fn from_rtc(entries: Vec<RtcStats>, sending: bool) -> Self {
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
            ice: ice_path(&entries),
            ..Self::default()
        };
        for entry in &entries {
            match entry {
                RtcStats::OutboundRtp(stats) if sending => {
                    if stats.stream.kind == "video" {
                        result.video_codec = codec(&stats.stream.codec_id);
                        result.video_frames = stats.outbound.frames_encoded;
                        result.video_bytes = stats.sent.bytes_sent;
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
                        result.video_bytes = stats.inbound.bytes_received;
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

fn ice_path(entries: &[RtcStats]) -> Option<IcePathStats> {
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
        relayed: [local.candidate_type, remote.candidate_type]
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
}
