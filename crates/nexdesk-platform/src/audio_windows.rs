use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering::Relaxed},
        mpsc::{Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat,
};

use super::{AUDIO_CHANNELS, AUDIO_RATE, AUDIO_SAMPLES, State};

pub(super) const SUPPORTED: bool = true;
const POLL: Duration = Duration::from_millis(4);
const RETRY: Duration = Duration::from_millis(400);

pub(super) fn capture(state: &State, send: SyncSender<Vec<i16>>) {
    with_default_device(state, Direction::Capture, |client, device| {
        let capture = client.get_audiocaptureclient()?;
        let mut bytes = VecDeque::with_capacity(AUDIO_SAMPLES * 6);
        client.start_stream()?;
        state.available.store(true, Relaxed);
        while !state.canceled.load(Relaxed) && device.current()? {
            for _ in 0..8 {
                if capture.get_next_packet_size()?.unwrap_or(0) == 0 {
                    break;
                }
                capture.read_from_device_to_deque(&mut bytes)?;
            }
            while bytes.len() >= AUDIO_SAMPLES * 2 {
                let frame = (0..AUDIO_SAMPLES)
                    .map(|_| {
                        i16::from_le_bytes([
                            bytes.pop_front().unwrap_or(0),
                            bytes.pop_front().unwrap_or(0),
                        ])
                    })
                    .collect();
                state.frames.fetch_add(1, Relaxed);
                if send.try_send(frame).is_err() {
                    state.dropped.fetch_add(1, Relaxed);
                }
            }
            thread::sleep(POLL);
        }
        client.stop_stream()
    });
}

pub(super) fn play(state: &State, volume: &AtomicU8, receive: Receiver<Vec<i16>>) {
    with_default_device(state, Direction::Render, |client, device| {
        let render = client.get_audiorenderclient()?;
        let mut queued = VecDeque::with_capacity(AUDIO_SAMPLES * 6);
        let mut bytes = Vec::with_capacity(AUDIO_SAMPLES * 2);
        client.start_stream()?;
        state.available.store(true, Relaxed);
        while !state.canceled.load(Relaxed) && device.current()? {
            let frames = client
                .get_available_space_in_frames()?
                .min(AUDIO_RATE / 100);
            let samples = (frames * AUDIO_CHANNELS) as usize;
            if samples > 0 {
                while queued.len() < AUDIO_SAMPLES * 4 {
                    let Ok(chunk) = receive.try_recv() else {
                        break;
                    };
                    queued.extend(chunk);
                }
                let gain = i32::from(volume.load(Relaxed));
                let had_audio = !queued.is_empty();
                bytes.clear();
                for _ in 0..samples {
                    let value = (i32::from(queued.pop_front().unwrap_or(0)) * gain / 100) as i16;
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                render.write_to_device(&bytes, None)?;
                if had_audio {
                    state.frames.fetch_add(1, Relaxed);
                }
            }
            thread::sleep(POLL);
        }
        client.stop_stream()
    });
}

struct Com;

impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

struct Endpoint<'a> {
    enumeration: &'a DeviceEnumerator,
    id: String,
    check_after: Instant,
}

impl Endpoint<'_> {
    fn current(&mut self) -> Result<bool, WasapiError> {
        let now = Instant::now();
        if now < self.check_after {
            return Ok(true);
        }
        self.check_after = now + Duration::from_secs(1);
        Ok(self
            .enumeration
            .get_default_device(Direction::Render)?
            .get_id()?
            == self.id)
    }
}

fn with_default_device(
    state: &State,
    direction: Direction,
    mut run: impl FnMut(AudioClient, &mut Endpoint<'_>) -> Result<(), WasapiError>,
) {
    if wasapi::initialize_mta().is_err() {
        return;
    }
    let _com = Com;
    let wave = WaveFormat::new(
        16,
        16,
        SampleType::Int,
        AUDIO_RATE as usize,
        AUDIO_CHANNELS as usize,
        None,
    );
    let mode = StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: 300_000,
    };
    while !state.canceled.load(Relaxed) {
        let _ = (|| {
            let enumeration = DeviceEnumerator::new()?;
            let endpoint = enumeration.get_default_device(Direction::Render)?;
            let id = endpoint.get_id()?;
            let mut client = endpoint.get_iaudioclient()?;
            client.initialize_client(&wave, direction, mode, None)?;
            run(
                client,
                &mut Endpoint {
                    enumeration: &enumeration,
                    id,
                    check_after: Instant::now(),
                },
            )
        })();
        state.available.store(false, Relaxed);
        if !state.canceled.load(Relaxed) {
            thread::sleep(RETRY);
        }
    }
}
