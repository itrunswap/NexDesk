//! The Media Foundation encoder is created, used and destroyed on one worker.

use std::{
    ffi::c_void,
    ptr::NonNull,
    slice,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

use libwebrtc::{
    video_frame::{
        EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame, VideoBuffer, VideoFrame,
    },
    video_source::{VideoResolution, native::NativeVideoSource},
};

use crate::MediaError;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

pub(crate) fn source() -> NativeVideoSource {
    NativeVideoSource::new(
        VideoResolution {
            width: WIDTH,
            height: HEIGHT,
        },
        true,
    )
}

#[derive(Clone)]
pub(crate) struct HardwareSink {
    frames: SyncSender<Vec<u8>>,
    stats: HardwareStats,
}

#[derive(Clone, Default)]
pub(crate) struct HardwareStats {
    encoded: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
}

impl HardwareStats {
    pub(crate) fn read(&self) -> (u64, bool) {
        (self.encoded.load(Relaxed), self.failed.load(Relaxed))
    }
}

impl HardwareSink {
    pub(crate) fn start(source: NativeVideoSource, fps: u16) -> Result<Self, MediaError> {
        let (frames, incoming) = mpsc::sync_channel::<Vec<u8>>(1);
        let (initialized, ready) = mpsc::sync_channel(0);
        let stats = HardwareStats::default();
        let shared = stats.clone();
        thread::Builder::new()
            .name("nexdesk-mf-h264".into())
            .spawn(move || {
                let Some(mut encoder) = Encoder::new(fps) else {
                    let _ = initialized.send(false);
                    return;
                };
                if initialized.send(true).is_err() {
                    return;
                }
                let mut since_key = fps;
                while let Ok(frame) = incoming.recv() {
                    let key_requested = source.take_keyframe_request() || since_key >= fps;
                    match encoder.encode(&frame, key_requested) {
                        Ok(Some(AccessUnit { payload, is_key })) => {
                            if is_key {
                                since_key = 0;
                            }
                            if source.capture_encoded_frame(&EncodedVideoFrame {
                                codec: EncodedVideoCodec::H264,
                                payload,
                                timestamp_us: 0,
                                frame_type: if is_key {
                                    EncodedFrameType::Key
                                } else {
                                    EncodedFrameType::Delta
                                },
                                resolution: VideoResolution {
                                    width: WIDTH,
                                    height: HEIGHT,
                                },
                                frame_metadata: None,
                            }) {
                                shared.encoded.fetch_add(1, Relaxed);
                            }
                        }
                        Ok(None) => {}
                        Err(()) => {
                            shared.failed.store(true, Relaxed);
                            break;
                        }
                    }
                    since_key = since_key.saturating_add(1);
                }
            })
            .map_err(MediaError::CaptureWorker)?;
        if ready.recv_timeout(Duration::from_secs(3)) != Ok(true) {
            return Err(MediaError::CodecUnavailable);
        }
        Ok(Self { frames, stats })
    }

    pub(crate) fn stats(&self) -> HardwareStats {
        self.stats.clone()
    }

    pub(crate) fn submit<T: AsRef<dyn VideoBuffer>>(&self, frame: &VideoFrame<T>) -> bool {
        let mut image = frame.buffer.as_ref().to_i420();
        let input_width = image.width();
        let input_height = image.height();
        if input_width == 0 || input_height == 0 {
            return false;
        }
        let (scaled_width, scaled_height) = if input_width * HEIGHT >= input_height * WIDTH {
            (WIDTH, ((input_height * WIDTH / input_width) & !1).max(2))
        } else {
            (((input_width * HEIGHT / input_height) & !1).max(2), HEIGHT)
        };
        if (input_width, input_height) != (scaled_width, scaled_height) {
            image = image.scale(scaled_width as i32, scaled_height as i32);
        }
        let (y, u, v) = image.data();
        let (stride_y, stride_u, stride_v) = image.strides();
        let width = scaled_width as usize;
        let height = scaled_height as usize;
        let canvas_stride = WIDTH as usize;
        let luma_size = (WIDTH * HEIGHT) as usize;
        // Even offsets keep chroma aligned and letterbox the image without stretching it.
        let left = ((WIDTH - scaled_width) as usize / 2) & !1;
        let top = ((HEIGHT - scaled_height) as usize / 2) & !1;
        let mut nv12 = vec![128; luma_size * 3 / 2];
        let (target_y, target_uv) = nv12.split_at_mut(luma_size);
        target_y.fill(16);
        let input_rows = y.chunks_exact(stride_y as usize).take(height);
        let output_rows = target_y.chunks_exact_mut(canvas_stride).skip(top);
        for (input, output) in input_rows.zip(output_rows) {
            output[left..left + width].copy_from_slice(&input[..width]);
        }
        let u_rows = u.chunks_exact(stride_u as usize);
        let v_rows = v.chunks_exact(stride_v as usize);
        let uv_rows = target_uv.chunks_exact_mut(canvas_stride).skip(top / 2);
        for ((u_row, v_row), output) in u_rows.zip(v_rows).take(height / 2).zip(uv_rows) {
            for ((&u, &v), pixel) in u_row
                .iter()
                .zip(v_row)
                .zip(output[left..left + width].chunks_exact_mut(2))
            {
                pixel.copy_from_slice(&[u, v]);
            }
        }
        self.frames.try_send(nv12).is_ok()
    }
}

#[repr(C)]
struct Output {
    bytes: *const u8,
    size: usize,
    key_frame: u32,
}

unsafe extern "C" {
    fn nexdesk_mf_h264_open(fps: u32) -> *mut c_void;
    fn nexdesk_mf_h264_encode(
        handle: *mut c_void,
        bytes: *const u8,
        size: usize,
        request_key_frame: u32,
        output: *mut Output,
    ) -> i32;
    fn nexdesk_mf_h264_close(handle: *mut c_void);
}

struct Encoder(NonNull<c_void>);

struct AccessUnit<'a> {
    payload: &'a [u8],
    is_key: bool,
}

impl Encoder {
    fn new(fps: u16) -> Option<Self> {
        // SAFETY: No borrowed inputs; a non-null return transfers one thread-affine encoder.
        NonNull::new(unsafe { nexdesk_mf_h264_open(u32::from(fps)) }).map(Self)
    }

    fn encode(&mut self, pixels: &[u8], key_frame: bool) -> Result<Option<AccessUnit<'_>>, ()> {
        let mut output = Output {
            bytes: std::ptr::null(),
            size: 0,
            key_frame: 0,
        };
        // SAFETY: The handle lives on this worker; the input slice and output struct live through the call.
        let status = unsafe {
            nexdesk_mf_h264_encode(
                self.0.as_ptr(),
                pixels.as_ptr(),
                pixels.len(),
                u32::from(key_frame),
                &mut output,
            )
        };
        match status {
            0 => Ok(None),
            1 if !output.bytes.is_null() && (4..=MAX_OUTPUT).contains(&output.size) => {
                // SAFETY: Native storage remains valid until the next mutable call on this encoder.
                let payload = unsafe { slice::from_raw_parts(output.bytes, output.size) };
                if !payload.starts_with(&[0, 0, 1]) && !payload.starts_with(&[0, 0, 0, 1]) {
                    return Err(());
                }
                let idr = payload
                    .windows(4)
                    .any(|nal| nal[..3] == [0, 0, 1] && nal[3] & 0x1f == 5);
                Ok(Some(AccessUnit {
                    payload,
                    is_key: output.key_frame != 0 || idr,
                }))
            }
            _ => Err(()),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: The worker exclusively owns the handle and closes it once on its creation thread.
        unsafe { nexdesk_mf_h264_close(self.0.as_ptr()) };
    }
}
