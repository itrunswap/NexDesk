use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed},
    mpsc::{self, Receiver, SyncSender},
};

use serde::{Deserialize, Serialize};

use crate::InputError;

#[cfg_attr(windows, path = "audio_windows.rs")]
#[cfg_attr(target_os = "linux", path = "audio_linux.rs")]
#[cfg_attr(target_os = "macos", path = "audio_macos.rs")]
#[cfg_attr(target_os = "macos", allow(unsafe_code))]
mod backend;

pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;
pub const AUDIO_SAMPLES: usize = (AUDIO_RATE * AUDIO_CHANNELS / 100) as usize;
const QUEUE: usize = 6;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct AudioStatus {
    pub available: bool,
    pub frames: u64,
    pub dropped: u64,
}

#[derive(Default)]
struct State {
    canceled: AtomicBool,
    available: AtomicBool,
    frames: AtomicU64,
    dropped: AtomicU64,
}

impl State {
    fn status(&self) -> AudioStatus {
        AudioStatus {
            available: self.available.load(Relaxed),
            frames: self.frames.load(Relaxed),
            dropped: self.dropped.load(Relaxed),
        }
    }
}

pub struct AudioCapture {
    state: Arc<State>,
    receive: Receiver<Vec<i16>>,
}

impl AudioCapture {
    pub fn supported() -> bool {
        #[cfg(not(target_os = "macos"))]
        {
            backend::SUPPORTED
        }
        #[cfg(target_os = "macos")]
        {
            backend::capture_supported()
        }
    }

    pub fn start() -> Result<Self, InputError> {
        if !Self::supported() {
            return Err(InputError::Unsupported);
        }
        let (send, receive) = mpsc::sync_channel(QUEUE);
        let state = Arc::new(State::default());
        let worker = state.clone();
        std::thread::Builder::new()
            .name("nexdesk-system-audio".into())
            .spawn(move || backend::capture(&worker, send))
            .map_err(|_| InputError::Unavailable)?;
        Ok(Self { state, receive })
    }

    pub fn next(&self) -> Option<Vec<i16>> {
        self.receive.try_recv().ok()
    }

    pub fn status(&self) -> AudioStatus {
        self.state.status()
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.state.canceled.store(true, Relaxed);
    }
}

pub struct AudioOutput {
    state: Arc<State>,
    volume: Arc<AtomicU8>,
    send: SyncSender<Vec<i16>>,
}

impl AudioOutput {
    pub fn supported() -> bool {
        backend::SUPPORTED
    }

    pub fn start(volume: u8) -> Result<Self, InputError> {
        if !Self::supported() {
            return Err(InputError::Unsupported);
        }
        let (send, receive) = mpsc::sync_channel(QUEUE);
        let state = Arc::new(State::default());
        let volume = Arc::new(AtomicU8::new(volume.min(100)));
        let worker = state.clone();
        let gain = volume.clone();
        std::thread::Builder::new()
            .name("nexdesk-audio-output".into())
            .spawn(move || backend::play(&worker, &gain, receive))
            .map_err(|_| InputError::Unavailable)?;
        Ok(Self {
            state,
            volume,
            send,
        })
    }

    pub fn volume(&self, volume: u8) {
        self.volume.store(volume.min(100), Relaxed);
    }

    pub fn submit(&self, samples: &[i16]) {
        if samples.len() != AUDIO_SAMPLES || self.send.try_send(samples.to_vec()).is_err() {
            self.state.dropped.fetch_add(1, Relaxed);
        }
    }

    pub fn status(&self) -> AudioStatus {
        self.state.status()
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.state.canceled.store(true, Relaxed);
    }
}
