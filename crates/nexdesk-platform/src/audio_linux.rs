use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        atomic::{AtomicU8, Ordering::Relaxed},
        mpsc::{Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use libpulse_binding::{
    callbacks::ListResult,
    context::{Context, FlagSet as ContextFlags, State as ContextState},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    sample::{Format, Spec},
    stream::{FlagSet, PeekResult, SeekMode, State as StreamState, Stream},
};

use super::{AUDIO_CHANNELS, AUDIO_RATE, AUDIO_SAMPLES, State};

pub(super) const SUPPORTED: bool = true;
const BYTES: usize = AUDIO_SAMPLES * 2;
const POLL: Duration = Duration::from_millis(4);

pub(super) fn capture(state: &State, send: SyncSender<Vec<i16>>) {
    repeat(state, |mut pulse| {
        let monitor = pulse.monitor(state)?;
        let mut stream = pulse.stream("NexDesk system audio")?;
        stream
            .connect_record(
                Some(&monitor),
                Some(&attributes()),
                FlagSet::ADJUST_LATENCY | FlagSet::DONT_MOVE,
            )
            .ok()?;
        pulse.ready(state, &stream)?;
        if stream.get_device_name().as_deref() != Some(&monitor) {
            return None;
        }
        state.available.store(true, Relaxed);
        let mut buffer = VecDeque::with_capacity(BYTES * 4);
        let mut check = Instant::now();
        while pulse.running(state, &stream) {
            for _ in 0..8 {
                match stream.peek().ok()? {
                    PeekResult::Empty => break,
                    PeekResult::Hole(length) => {
                        buffer.resize(buffer.len() + length.min(BYTES * 3), 0)
                    }
                    PeekResult::Data(data) => buffer.extend(data.iter().take(BYTES * 3).copied()),
                }
                stream.discard().ok()?;
            }
            while buffer.len() >= BYTES {
                let frame = (0..AUDIO_SAMPLES)
                    .map(|_| {
                        i16::from_le_bytes([
                            buffer.pop_front().unwrap_or(0),
                            buffer.pop_front().unwrap_or(0),
                        ])
                    })
                    .collect();
                state.frames.fetch_add(1, Relaxed);
                if send.try_send(frame).is_err() {
                    state.dropped.fetch_add(1, Relaxed);
                }
            }
            if check.elapsed() >= Duration::from_secs(2) {
                check = Instant::now();
                if pulse.monitor(state).as_deref() != Some(&monitor) {
                    break;
                }
            }
            thread::sleep(POLL);
        }
        Some(())
    });
}

pub(super) fn play(state: &State, volume: &AtomicU8, receive: Receiver<Vec<i16>>) {
    repeat(state, |mut pulse| {
        let mut stream = pulse.stream("NexDesk remote audio")?;
        stream
            .connect_playback(
                None,
                Some(&attributes()),
                FlagSet::ADJUST_LATENCY,
                None,
                None,
            )
            .ok()?;
        pulse.ready(state, &stream)?;
        state.available.store(true, Relaxed);
        let mut bytes = [0; BYTES];
        while pulse.running(state, &stream) {
            for _ in 0..4 {
                if stream.writable_size()? < BYTES {
                    break;
                }
                let gain = i32::from(volume.load(Relaxed));
                let frame = receive.try_recv().ok();
                if let Some(frame) = &frame {
                    for (chunk, &sample) in bytes.chunks_exact_mut(2).zip(frame) {
                        chunk.copy_from_slice(
                            &((i32::from(sample) * gain / 100) as i16).to_le_bytes(),
                        );
                    }
                } else {
                    bytes.fill(0);
                }
                stream.write_copy(&bytes, 0, SeekMode::Relative).ok()?;
                if frame.is_some() {
                    state.frames.fetch_add(1, Relaxed);
                }
            }
            thread::sleep(POLL);
        }
        Some(())
    });
}

fn repeat(state: &State, mut run: impl FnMut(Pulse) -> Option<()>) {
    while !state.canceled.load(Relaxed) {
        let _ = Pulse::connect(state).and_then(&mut run);
        state.available.store(false, Relaxed);
        if !state.canceled.load(Relaxed) {
            thread::sleep(Duration::from_millis(400));
        }
    }
}

fn attributes() -> BufferAttr {
    BufferAttr {
        maxlength: (BYTES * 8) as u32,
        tlength: (BYTES * 3) as u32,
        prebuf: (BYTES * 2) as u32,
        minreq: BYTES as u32,
        fragsize: BYTES as u32,
    }
}

struct Pulse {
    context: Context,
    mainloop: Mainloop,
}

impl Pulse {
    fn connect(state: &State) -> Option<Self> {
        let mainloop = Mainloop::new()?;
        let mut context = Context::new(&mainloop, "NexDesk")?;
        context
            .connect(None, ContextFlags::NOAUTOSPAWN, None)
            .ok()?;
        let mut pulse = Self { context, mainloop };
        pulse.wait(state, |pulse| match pulse.context.get_state() {
            ContextState::Ready => Some(true),
            ContextState::Failed | ContextState::Terminated => Some(false),
            _ => None,
        })?;
        Some(pulse)
    }

    fn stream(&mut self, name: &str) -> Option<Stream> {
        Stream::new(
            &mut self.context,
            name,
            &Spec {
                format: Format::S16le,
                channels: AUDIO_CHANNELS as u8,
                rate: AUDIO_RATE,
            },
            None,
        )
    }

    fn ready(&mut self, state: &State, stream: &Stream) -> Option<()> {
        self.wait(state, |_| match stream.get_state() {
            StreamState::Ready => Some(true),
            StreamState::Failed | StreamState::Terminated => Some(false),
            _ => None,
        })
    }

    fn monitor(&mut self, state: &State) -> Option<String> {
        let current = Rc::new(RefCell::new(None));
        let destination = current.clone();
        let _operation = self.context.introspect().get_server_info(move |result| {
            *destination.borrow_mut() =
                Some(result.default_sink_name.as_deref().map(str::to_owned));
        });
        self.wait(state, |_| current.borrow().as_ref().map(Option::is_some))?;
        let sink = current.borrow_mut().take().flatten()?;
        let current = Rc::new(RefCell::new(None));
        let destination = current.clone();
        self.context
            .introspect()
            .get_sink_info_by_name(&sink, move |info| match info {
                ListResult::Item(sink) => {
                    *destination.borrow_mut() =
                        Some(sink.monitor_source_name.as_deref().map(str::to_owned));
                }
                ListResult::End | ListResult::Error if destination.borrow().is_none() => {
                    *destination.borrow_mut() = Some(None);
                }
                _ => {}
            });
        self.wait(state, |_| current.borrow().as_ref().map(Option::is_some))?;
        current.borrow_mut().take().flatten()
    }

    fn wait(&mut self, state: &State, mut ready: impl FnMut(&Self) -> Option<bool>) -> Option<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && self.pump(state) {
            if let Some(done) = ready(self) {
                return done.then_some(());
            }
            thread::sleep(POLL);
        }
        None
    }

    fn running(&mut self, state: &State, stream: &Stream) -> bool {
        self.pump(state)
            && self.context.get_state() == ContextState::Ready
            && stream.get_state() == StreamState::Ready
    }

    fn pump(&mut self, state: &State) -> bool {
        !state.canceled.load(Relaxed)
            && matches!(self.mainloop.iterate(false), IterateResult::Success(_))
    }
}
