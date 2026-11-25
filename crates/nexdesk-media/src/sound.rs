use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicU64, Ordering::Relaxed},
};
use std::time::Duration;

use libwebrtc::{
    audio_frame::AudioFrame,
    audio_source::native::NativeAudioSource,
    audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions},
    audio_track::RtcAudioTrack,
};
use nexdesk_platform::{AUDIO_CHANNELS, AUDIO_RATE, AudioCapture, AudioOutput, AudioStatus};
use serde::Serialize;
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};
use tokio_stream::StreamExt;

pub(crate) async fn send(source: NativeAudioSource, capture: Arc<Mutex<AudioCapture>>) {
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut frame = AudioFrame::new(AUDIO_RATE, AUDIO_CHANNELS, AUDIO_RATE / 100);
    loop {
        clock.tick().await;
        let borrowed = capture.lock().ok().and_then(|device| device.next());
        let output = frame.data.to_mut();
        if let Some(samples) = borrowed {
            output.copy_from_slice(&samples);
        } else {
            output.fill(0);
        }
        if source.capture_frame(&frame).await.is_err() {
            return;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SoundStatus {
    pub decoded: u64,
    pub non_silent: u64,
    pub output: AudioStatus,
}

#[derive(Default)]
struct Counters {
    decoded: AtomicU64,
    non_silent: AtomicU64,
    output: Mutex<AudioStatus>,
}

pub struct ReceivedSound {
    counters: Arc<Counters>,
    task: JoinHandle<()>,
}

impl ReceivedSound {
    pub(crate) fn start(track: mpsc::Receiver<RtcAudioTrack>, gain: Arc<AtomicU8>) -> Self {
        let counters = Arc::new(Counters::default());
        let task = tokio::spawn(receive(track, gain, counters.clone()));
        Self { counters, task }
    }

    pub fn status(&self) -> SoundStatus {
        SoundStatus {
            decoded: self.counters.decoded.load(Relaxed),
            non_silent: self.counters.non_silent.load(Relaxed),
            output: self
                .counters
                .output
                .lock()
                .map(|status| *status)
                .unwrap_or_default(),
        }
    }
}

impl Drop for ReceivedSound {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(
    mut track: mpsc::Receiver<RtcAudioTrack>,
    gain: Arc<AtomicU8>,
    counters: Arc<Counters>,
) {
    let Some(track) = track.recv().await else {
        return;
    };
    let mut stream = NativeAudioStream::with_options(
        track,
        AUDIO_RATE as i32,
        AUDIO_CHANNELS as i32,
        NativeAudioStreamOptions {
            queue_size_frames: Some(6),
        },
    );
    let mut output = None;
    while let Some(frame) = stream.next().await {
        counters.decoded.fetch_add(1, Relaxed);
        if frame.data.iter().any(|&value| value.unsigned_abs() > 24) {
            counters.non_silent.fetch_add(1, Relaxed);
        }
        let volume = gain.load(Relaxed);
        if volume > 0 && output.is_none() {
            output = AudioOutput::start(volume).ok();
        }
        if let Some(output) = &output {
            output.volume(volume);
            if volume > 0 {
                output.submit(&frame.data);
            }
            if let Ok(mut status) = counters.output.lock() {
                *status = output.status();
            }
        }
    }
}
