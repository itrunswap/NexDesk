//! Bounds and checks the desktop capturer's native buffer and source selection.

use std::{slice, sync::Arc, time::Instant};

use cxx::UniquePtr;
#[cfg(target_os = "macos")]
use libwebrtc::video_frame::native::NativeBuffer as DesktopBuffer;
use libwebrtc::video_frame::{VideoFrame, VideoRotation};
#[cfg(not(target_os = "macos"))]
use libwebrtc::{native::yuv_helper, video_frame::NV12Buffer as DesktopBuffer};
use webrtc_sys::desktop_capturer::{
    DesktopCapturerCallback, DesktopCapturerCallbackWrapper,
    ffi::{DesktopCapturer, DesktopCapturerOptions, DesktopFrame, Source, new_desktop_capturer},
};

pub(crate) use webrtc_sys::desktop_capturer::{CaptureError, ffi::SourceType};

use crate::text_detail::TextDetail;
use crate::{CaptureRegion, FrameSize, MediaError, capture::CaptureSettings};

pub(crate) struct CapturedFrame {
    pub(crate) frame: VideoFrame<DesktopBuffer>,
    pub(crate) region: CaptureRegion,
    pub(crate) started: Instant,
    pub(crate) text_detail: bool,
}

pub(crate) struct DesktopCapture {
    capturer: UniquePtr<DesktopCapturer>,
    #[cfg(target_os = "macos")]
    explicit_screen: bool,
}

impl DesktopCapture {
    pub(crate) fn new(source_type: SourceType, system_picker: bool) -> Result<Self, MediaError> {
        #[cfg(target_os = "macos")]
        let explicit_screen = source_type == SourceType::Screen && !system_picker;
        let capturer = new_desktop_capturer(DesktopCapturerOptions {
            source_type,
            include_cursor: true,
            allow_sck_system_picker: system_picker,
        });
        if capturer.is_null() {
            return Err(MediaError::CaptureUnavailable);
        }
        Ok(Self {
            capturer,
            #[cfg(target_os = "macos")]
            explicit_screen,
        })
    }

    pub(crate) fn sources(&self) -> Result<Vec<Source>, MediaError> {
        let sources = self.capturer.get_source_list();
        #[cfg(target_os = "macos")]
        if self.explicit_screen && sources.is_empty() {
            // WebRTC's ScreenCaptureKit capturer accepts display IDs but leaves
            // its source list empty when the system picker is disabled.
            let displays =
                nexdesk_platform::capture_display_ids().ok_or(MediaError::CaptureUnavailable)?;
            return Ok(displays
                .into_iter()
                .map(|id| Source {
                    id: u64::from(id),
                    title: String::new(),
                    display_id: i64::from(id),
                })
                .collect());
        }
        Ok(sources)
    }

    pub(crate) fn start(
        &mut self,
        source: Option<u64>,
        settings: Arc<CaptureSettings>,
        callback: impl FnMut(Result<CapturedFrame, CaptureError>) + Send + 'static,
    ) -> Result<(), MediaError> {
        if let Some(source) = source
            && (!self.sources()?.iter().any(|entry| entry.id == source)
                || !self.capturer.select_source(source))
        {
            return Err(MediaError::CaptureUnavailable);
        }
        self.capturer
            .pin_mut()
            .start(Box::new(DesktopCapturerCallbackWrapper::new(Box::new(
                FrameCallback {
                    callback,
                    settings,
                    scaled_bgra: Vec::new(),
                    pixels: DesktopPixels::default(),
                },
            ))));
        Ok(())
    }

    pub(crate) fn capture_frame(&self) {
        self.capturer.capture_frame();
    }
}

struct FrameCallback<F> {
    callback: F,
    settings: Arc<CaptureSettings>,
    scaled_bgra: Vec<u8>,
    pixels: DesktopPixels,
}

impl<F> DesktopCapturerCallback for FrameCallback<F>
where
    F: FnMut(Result<CapturedFrame, CaptureError>) + Send,
{
    fn on_capture_result(&mut self, result: Result<UniquePtr<DesktopFrame>, CaptureError>) {
        (self.callback)(result.and_then(|frame| {
            frame
                .as_ref()
                .and_then(|frame| {
                    to_nv12(
                        frame,
                        &self.settings,
                        &mut self.scaled_bgra,
                        &mut self.pixels,
                    )
                })
                .ok_or(CaptureError::Temporary)
        }));
    }
}

unsafe extern "C" {
    fn ARGBScale(
        src: *const u8,
        stride: i32,
        width: i32,
        height: i32,
        dst: *mut u8,
        dst_stride: i32,
        dst_width: i32,
        dst_height: i32,
        filter: i32,
    ) -> i32;
}

fn to_nv12(
    frame: &DesktopFrame,
    settings: &CaptureSettings,
    scratch: &mut Vec<u8>,
    converter: &mut DesktopPixels,
) -> Option<CapturedFrame> {
    let started = Instant::now();
    let width = u32::try_from(frame.width()).ok()? & !1;
    let height = u32::try_from(frame.height()).ok()? & !1;
    let stride = u32::try_from(frame.stride()).ok()?;
    let pointer = frame.data();
    if !(2..=8192).contains(&width)
        || !(2..=8192).contains(&height)
        || width * height > 8192 * 4320
        || stride < width * 4
        || stride > 65_536
        || pointer.is_null()
    {
        return None;
    }
    let length = (stride as usize).checked_mul(height as usize)?;
    // SAFETY: DesktopFrame owns at least stride bytes per row until this callback
    // releases it. Dimensions, stride and length are checked above.
    let pixels = unsafe { slice::from_raw_parts(pointer, length) };
    let source = FrameSize { width, height };
    let output = settings.output_size(source);
    let (pixels, stride) = if source == output {
        (pixels, stride)
    } else {
        let out_stride = output.width * 4;
        scratch.resize((out_stride * output.height) as usize, 0);
        // SAFETY: Source and destination are disjoint, validated BGRA planes.
        // Box filtering precedes chroma subsampling so scaled text retains color edges.
        let result = unsafe {
            ARGBScale(
                pixels.as_ptr(),
                stride as i32,
                width as i32,
                height as i32,
                scratch.as_mut_ptr(),
                out_stride as i32,
                output.width as i32,
                output.height as i32,
                3,
            )
        };
        if result != 0 {
            return None;
        }
        (scratch.as_slice(), out_stride)
    };
    let text_detail = settings
        .text_detail
        .load(std::sync::atomic::Ordering::Relaxed);
    let region = CaptureRegion {
        left: frame.left(),
        top: frame.top(),
        width,
        height,
    };
    Some(CapturedFrame {
        frame: converter.frame(pixels, stride, output, text_detail)?,
        region,
        started,
        text_detail,
    })
}

#[derive(Default)]
pub(crate) struct DesktopPixels {
    detail: TextDetail,
    #[cfg(target_os = "macos")]
    pool: crate::mac_pixels::CapturePool,
}

impl DesktopPixels {
    pub(crate) fn frame(
        &mut self,
        bgra: &[u8],
        stride: u32,
        size: FrameSize,
        enhance: bool,
    ) -> Option<VideoFrame<DesktopBuffer>> {
        let detail = enhance.then_some(&mut self.detail);
        #[cfg(target_os = "macos")]
        let image = self.pool.convert(bgra, stride, size, detail)?;
        #[cfg(not(target_os = "macos"))]
        let image = {
            let mut image = DesktopBuffer::new(size.width, size.height);
            let (ys, uvs) = image.strides();
            let (y, uv) = image.data_mut();
            yuv_helper::argb_to_nv12(
                bgra,
                stride,
                y,
                ys,
                uv,
                uvs,
                size.width as i32,
                size.height as i32,
            );
            if let Some(detail) = detail {
                detail.apply(y, ys as usize, size.width as usize, size.height as usize);
            }
            image
        };
        Some(VideoFrame::new(VideoRotation::VideoRotation0, image))
    }
}
