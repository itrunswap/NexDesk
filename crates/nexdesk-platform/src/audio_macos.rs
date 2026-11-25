use std::{
    collections::VecDeque,
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering::Relaxed},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, ProtocolObject},
    sel,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType,
};

use super::{AUDIO_CHANNELS, AUDIO_RATE, AUDIO_SAMPLES, State};

pub(super) const SUPPORTED: bool = true;
const FRAME_BYTES: usize = AUDIO_SAMPLES * 2;

pub(super) fn capture_supported() -> bool {
    AnyClass::get(c"SCShareableContent").is_some()
        && AnyClass::get(c"SCStreamConfiguration").is_some_and(|configuration| {
            configuration
                .instance_method(sel!(setCapturesAudio:))
                .is_some()
                && configuration
                    .instance_method(sel!(setExcludesCurrentProcessAudio:))
                    .is_some()
        })
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

#[repr(C)]
struct StreamFormat {
    sample_rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct Buffers {
    count: u32,
    items: [AudioBuffer; 2],
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetNumSamples(buffer: *const c_void) -> isize;
    fn CMSampleBufferGetFormatDescription(buffer: *const c_void) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(
        format: *const c_void,
    ) -> *const StreamFormat;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample: *const c_void,
        needed: *mut usize,
        list: *mut Buffers,
        length: usize,
        allocator: *const c_void,
        data_allocator: *const c_void,
        flags: u32,
        owner: *mut *const c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

struct BlockOwner(*const c_void);

impl Drop for BlockOwner {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: CoreMedia provided one retained block buffer.
            unsafe { CFRelease(self.0) };
        }
    }
}

#[derive(Default)]
struct CaptureQueue {
    samples: Mutex<VecDeque<i16>>,
    failed: AtomicBool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NexDeskSystemAudioReceiver"]
    #[ivars = CaptureQueue]
    struct CaptureReceiver;

    unsafe impl NSObjectProtocol for CaptureReceiver {}

    unsafe impl SCStreamOutput for CaptureReceiver {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _: &SCStream,
            buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Audio {
                ingest(buffer, self.ivars());
            }
        }
    }

    unsafe impl SCStreamDelegate for CaptureReceiver {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stream_didStopWithError(&self, _: &SCStream, _: &NSError) {
            self.ivars().failed.store(true, Relaxed);
        }
    }
);

// The shareable-content object is an immutable snapshot returned through the
// ScreenCaptureKit completion queue. One retained reference transfers ownership.
struct Content(Retained<SCShareableContent>);
unsafe impl Send for Content {}

pub(super) fn capture(state: &State, send: SyncSender<Vec<i16>>) {
    while !state.canceled.load(Relaxed) {
        autoreleasepool(|_| {
            let _ = capture_device(state, &send);
        });
        state.available.store(false, Relaxed);
        if !state.canceled.load(Relaxed) {
            thread::sleep(Duration::from_millis(400));
        }
    }
}

fn capture_device(state: &State, send: &SyncSender<Vec<i16>>) -> Option<()> {
    // SAFETY: This is a read-only TCC permission check.
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return None;
    }
    let (finish, ready) = mpsc::sync_channel(1);
    let content = RcBlock::new(move |content: *mut SCShareableContent, _: *mut NSError| {
        // SAFETY: A non-null completion argument is a valid object for the call.
        let snapshot = unsafe { Retained::retain(content) }.map(Content);
        let _ = finish.try_send(snapshot);
    });
    // SAFETY: ScreenCaptureKit retains the completion block until it returns.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&content) };
    let Content(content) = wait(state, &ready)??;
    // SAFETY: The locally retained shareable-content snapshot owns its display list.
    let displays = unsafe { content.displays() };
    if displays.count() == 0 {
        return None;
    }
    let display = displays.objectAtIndex(0);
    let excluded = NSArray::new();
    let sample_queue = DispatchQueue::new("dev.nexdesk.system-audio", DispatchQueueAttr::SERIAL);
    let receiver = CaptureReceiver::alloc().set_ivars(CaptureQueue::default());
    // SAFETY: NSObject initialization consumes the fresh subclass allocation.
    let receiver: Retained<CaptureReceiver> = unsafe { msg_send![super(receiver), init] };
    // SAFETY: All objects are retained until the stream has been stopped.
    let stream = unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &excluded,
        );
        let configuration = SCStreamConfiguration::new();
        configuration.setCapturesAudio(true);
        configuration.setSampleRate(AUDIO_RATE as isize);
        configuration.setChannelCount(AUDIO_CHANNELS as isize);
        configuration.setExcludesCurrentProcessAudio(true);
        configuration.setWidth(16);
        configuration.setHeight(16);
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &configuration,
            Some(ProtocolObject::from_ref(&*receiver)),
        )
    };
    // SAFETY: The delegate receiver and its serial callback queue remain retained through capture.
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*receiver),
                SCStreamOutputType::Audio,
                Some(&sample_queue),
            )
            .ok()?;
    }
    let (finish, ready) = mpsc::sync_channel(1);
    let completed = RcBlock::new(move |error: *mut NSError| {
        let _ = finish.try_send(error.is_null());
    });
    // SAFETY: The receiver has been registered and the completion owns its sender.
    unsafe { stream.startCaptureWithCompletionHandler(Some(&completed)) };
    if wait(state, &ready) == Some(true) {
        state.available.store(true, Relaxed);
        let queue = receiver.ivars();
        while !state.canceled.load(Relaxed) && !queue.failed.load(Relaxed) {
            if let Ok(mut samples) = queue.samples.lock() {
                while samples.len() >= AUDIO_SAMPLES {
                    let frame = samples.drain(..AUDIO_SAMPLES).collect();
                    state.frames.fetch_add(1, Relaxed);
                    if send.try_send(frame).is_err() {
                        state.dropped.fetch_add(1, Relaxed);
                    }
                }
            }
            thread::sleep(Duration::from_millis(4));
        }
    }
    // SAFETY: The retained stream and output are valid for both shutdown calls.
    unsafe {
        let _ = stream.removeStreamOutput_type_error(
            ProtocolObject::from_ref(&*receiver),
            SCStreamOutputType::Audio,
        );
        stream.stopCaptureWithCompletionHandler(None);
    }
    Some(())
}

fn wait<T>(state: &State, receive: &Receiver<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !state.canceled.load(Relaxed) {
        if let Ok(value) = receive.recv_timeout(Duration::from_millis(30)) {
            return Some(value);
        }
    }
    None
}

fn ingest(buffer: &CMSampleBuffer, queue: &CaptureQueue) {
    let sample = ptr::from_ref(buffer).cast::<c_void>();
    // SAFETY: The sample and its format description live through this callback.
    let (frames, format) = unsafe {
        let description = CMSampleBufferGetFormatDescription(sample);
        if description.is_null() {
            return;
        }
        (
            CMSampleBufferGetNumSamples(sample),
            CMAudioFormatDescriptionGetStreamBasicDescription(description).as_ref(),
        )
    };
    let Some(format) = format else {
        return;
    };
    if !(1..=4800).contains(&frames)
        || format.channels != AUDIO_CHANNELS
        || format.sample_rate != f64::from(AUDIO_RATE)
        || format.format != u32::from_be_bytes(*b"lpcm")
        || format.bits != 32
        || format.flags & 3 != 1
    {
        return;
    }
    let frames = frames as usize;
    let mut owner = BlockOwner(ptr::null());
    let mut buffers = Buffers::default();
    // SAFETY: The two-buffer list is sized and the returned block retains PCM until the end of the scope.
    if unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample,
            ptr::null_mut(),
            &mut buffers,
            size_of::<Buffers>(),
            ptr::null(),
            ptr::null(),
            1,
            &mut owner.0,
        )
    } != 0
    {
        return;
    }
    let samples = match buffers.count {
        1 if buffers.items[0].channels == 2 => audio_bytes(buffers.items[0], frames * 2, &owner)
            .map(|data| data.chunks_exact(4).map(sample_value).collect::<Vec<_>>()),
        2 if buffers.items[0].channels == 1 && buffers.items[1].channels == 1 => {
            audio_bytes(buffers.items[0], frames, &owner)
                .zip(audio_bytes(buffers.items[1], frames, &owner))
                .map(|(left, right)| {
                    left.chunks_exact(4)
                        .zip(right.chunks_exact(4))
                        .flat_map(|(left, right)| [sample_value(left), sample_value(right)])
                        .collect()
                })
        }
        _ => None,
    };
    if let Some(samples) = samples
        && let Ok(mut pending) = queue.samples.lock()
    {
        pending.extend(samples);
        let excess = pending.len().saturating_sub(AUDIO_SAMPLES * 6);
        pending.drain(..excess);
    }
}

fn audio_bytes(buffer: AudioBuffer, expected: usize, owner: &BlockOwner) -> Option<&[u8]> {
    if owner.0.is_null() || buffer.data.is_null() || (buffer.bytes as usize) < expected * 4 {
        return None;
    }
    // SAFETY: The AudioBufferList's retained owner keeps this checked PCM prefix alive.
    Some(unsafe { std::slice::from_raw_parts(buffer.data.cast(), expected * 4) })
}

fn sample_value(bytes: &[u8]) -> i16 {
    (f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).clamp(-1.0, 1.0) * 32767.0) as i16
}

type QueueHandle = *mut c_void;

#[repr(C)]
struct QueueBuffer {
    capacity: u32,
    data: *mut c_void,
    length: u32,
    user: *mut c_void,
    descriptions_capacity: u32,
    descriptions: *mut c_void,
    descriptions_count: u32,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioQueueNewOutput(
        format: *const StreamFormat,
        callback: unsafe extern "C" fn(*mut c_void, QueueHandle, *mut QueueBuffer),
        context: *mut c_void,
        loop_ref: *const c_void,
        mode: *const c_void,
        flags: u32,
        output: *mut QueueHandle,
    ) -> i32;
    fn AudioQueueAllocateBuffer(
        queue: QueueHandle,
        bytes: u32,
        output: *mut *mut QueueBuffer,
    ) -> i32;
    fn AudioQueueEnqueueBuffer(
        queue: QueueHandle,
        buffer: *mut QueueBuffer,
        count: u32,
        descriptions: *const c_void,
    ) -> i32;
    fn AudioQueueStart(queue: QueueHandle, time: *const c_void) -> i32;
    fn AudioQueueDispose(queue: QueueHandle, immediate: u8) -> i32;
}

unsafe extern "C" fn recycled(context: *mut c_void, _: QueueHandle, buffer: *mut QueueBuffer) {
    // SAFETY: AudioQueue holds this callback context until synchronous disposal.
    let sender = unsafe { &*context.cast::<SyncSender<usize>>() };
    let _ = sender.try_send(buffer as usize);
}

struct Playback {
    queue: NonNull<c_void>,
    _context: Box<SyncSender<usize>>,
    available: Receiver<usize>,
}

impl Playback {
    fn open() -> Option<Self> {
        let (free, available) = mpsc::sync_channel(4);
        let mut context = Box::new(free);
        let format = StreamFormat {
            sample_rate: f64::from(AUDIO_RATE),
            format: u32::from_be_bytes(*b"lpcm"),
            flags: 12,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels: AUDIO_CHANNELS,
            bits: 16,
            reserved: 0,
        };
        let mut handle = ptr::null_mut();
        // SAFETY: The queue copies the format and the callback context remains boxed until disposal.
        if unsafe {
            AudioQueueNewOutput(
                &format,
                recycled,
                ptr::from_mut(&mut *context).cast(),
                ptr::null(),
                ptr::null(),
                0,
                &mut handle,
            )
        } != 0
        {
            return None;
        }
        let playback = Self {
            queue: NonNull::new(handle)?,
            _context: context,
            available,
        };
        for _ in 0..4 {
            let mut buffer = ptr::null_mut();
            // SAFETY: The live queue owns all allocated buffers until disposal.
            if unsafe {
                AudioQueueAllocateBuffer(playback.queue.as_ptr(), FRAME_BYTES as u32, &mut buffer)
            } != 0
                || buffer.is_null()
            {
                return None;
            }
            playback._context.try_send(buffer as usize).ok()?;
        }
        Some(playback)
    }

    fn submit(&self, buffer: usize, bytes: &[u8; FRAME_BYTES]) -> bool {
        let buffer = buffer as *mut QueueBuffer;
        // SAFETY: Recycled addresses are exclusively owned by this producer until enqueued again.
        unsafe {
            let target = &mut *buffer;
            if target.data.is_null() || target.capacity < FRAME_BYTES as u32 {
                return false;
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), target.data.cast(), FRAME_BYTES);
            target.length = FRAME_BYTES as u32;
            AudioQueueEnqueueBuffer(self.queue.as_ptr(), buffer, 0, ptr::null()) == 0
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // SAFETY: Immediate disposal stops callbacks before the boxed context is dropped.
        unsafe { AudioQueueDispose(self.queue.as_ptr(), 1) };
    }
}

pub(super) fn play(state: &State, volume: &AtomicU8, receive: Receiver<Vec<i16>>) {
    while !state.canceled.load(Relaxed) {
        if let Some(playback) = Playback::open() {
            state.available.store(true, Relaxed);
            let mut started = false;
            let mut bytes = [0; FRAME_BYTES];
            while !state.canceled.load(Relaxed) {
                let Ok(buffer) = playback.available.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
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
                if !playback.submit(buffer, &bytes) {
                    break;
                }
                if !started {
                    // SAFETY: Playback owns the queue and has enqueued its first buffer.
                    if unsafe { AudioQueueStart(playback.queue.as_ptr(), ptr::null()) } != 0 {
                        break;
                    }
                    started = true;
                }
                if frame.is_some() {
                    state.frames.fetch_add(1, Relaxed);
                }
            }
        }
        state.available.store(false, Relaxed);
        if !state.canceled.load(Relaxed) {
            thread::sleep(Duration::from_millis(400));
        }
    }
}
