//! Bounds and checks the desktop capturer's native buffer and source selection.

use std::slice;

use cxx::UniquePtr;
use libwebrtc::{
    native::yuv_helper,
    video_frame::{NV12Buffer, VideoFrame, VideoRotation},
};
use webrtc_sys::desktop_capturer::{
    DesktopCapturerCallback, DesktopCapturerCallbackWrapper,
    ffi::{DesktopCapturer, DesktopCapturerOptions, DesktopFrame, Source, new_desktop_capturer},
};

pub(crate) use webrtc_sys::desktop_capturer::{CaptureError, ffi::SourceType};

use crate::{CaptureRegion, MediaError, capture::valid_size};

type CapturedFrame = (VideoFrame<NV12Buffer>, CaptureRegion);

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
                FrameCallback(callback),
            ))));
        Ok(())
    }

    pub(crate) fn capture_frame(&self) {
        self.capturer.capture_frame();
    }
}

struct FrameCallback<F>(F);

impl<F> DesktopCapturerCallback for FrameCallback<F>
where
    F: FnMut(Result<CapturedFrame, CaptureError>) + Send,
{
    fn on_capture_result(&mut self, result: Result<UniquePtr<DesktopFrame>, CaptureError>) {
        (self.0)(result.and_then(|frame| {
            frame
                .as_ref()
                .and_then(to_nv12)
                .ok_or(CaptureError::Temporary)
        }));
    }
}

fn to_nv12(frame: &DesktopFrame) -> Option<CapturedFrame> {
    let width = u32::try_from(frame.width()).ok()? & !1;
    let height = u32::try_from(frame.height()).ok()? & !1;
    let stride = u32::try_from(frame.stride()).ok()?;
    let pointer = frame.data();
    if !valid_size(width, height) || stride < width * 4 || stride > 65_536 || pointer.is_null() {
        return None;
    }
    let length = (stride as usize).checked_mul(height as usize)?;
    // SAFETY: DesktopFrame owns at least stride bytes per row until this callback
    // releases it. Dimensions, stride and length are checked above.
    let pixels = unsafe { slice::from_raw_parts(pointer, length) };
    let mut image = NV12Buffer::new(width, height);
    let (stride_y, stride_uv) = image.strides();
    let (y, uv) = image.data_mut();
    // Libyuv's little-endian ARGB input matches the capturer's BGRA bytes.
    yuv_helper::argb_to_nv12(
        pixels,
        stride,
        y,
        stride_y,
        uv,
        stride_uv,
        width as i32,
        height as i32,
    );
    let region = CaptureRegion {
        left: frame.left(),
        top: frame.top(),
        width,
        height,
    };
    Some((
        VideoFrame::new(VideoRotation::VideoRotation0, image),
        region,
    ))
}
