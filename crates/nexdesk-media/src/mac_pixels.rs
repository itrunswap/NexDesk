//! Interpret VideoToolbox's actual CVPixelBuffer before WebRTC flattens it to I420.
use std::{ffi::c_void, ptr::NonNull, slice};

use crate::sdr::nexdesk_planar_to_rgb;
use crate::{FrameSize, text_detail::TextDetail};
use libwebrtc::{
    native::yuv_helper,
    video_frame::{BoxVideoFrame, native::NativeBuffer},
};

unsafe extern "C" {
    fn nexdesk_cv_capture_pool(width: i32, height: i32) -> *mut c_void;
    fn nexdesk_cv_capture_pool_release(pool: *mut c_void);
    fn nexdesk_cv_capture_begin(pool: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetBaseAddressOfPlane(image: *mut c_void, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(image: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferUnlockBaseAddress(image: *mut c_void, flags: u64) -> i32;
    fn CFRelease(value: *const c_void);
    fn nexdesk_cv_color(image: *mut c_void, desktop_sdr: bool) -> i32;
    fn nexdesk_cv_to_rgb(
        image: *mut c_void,
        color: i32,
        rgba: bool,
        out: *mut u8,
        stride: i32,
        width: i32,
        height: i32,
    ) -> bool;
}

#[derive(Default)]
pub(crate) struct CapturePool {
    pool: Option<NonNull<c_void>>,
    dimensions: Option<FrameSize>,
}

// The capture callback exclusively owns this pool. CoreVideo retains outstanding images.
unsafe impl Send for CapturePool {}

impl Drop for CapturePool {
    fn drop(&mut self) {
        if let Some(pool) = self.pool {
            // SAFETY: This is the single ownership reference from nexdesk_cv_capture_pool.
            unsafe { nexdesk_cv_capture_pool_release(pool.as_ptr()) };
        }
    }
}

struct LockedCapture(NonNull<c_void>);

impl Drop for LockedCapture {
    fn drop(&mut self) {
        // SAFETY: capture_begin transferred one locked, owned pixel buffer.
        unsafe {
            CVPixelBufferUnlockBaseAddress(self.0.as_ptr(), 0);
            CFRelease(self.0.as_ptr());
        }
    }
}

impl CapturePool {
    pub(crate) fn convert(
        &mut self,
        bgra: &[u8],
        stride: u32,
        size: FrameSize,
        detail: Option<&mut TextDetail>,
    ) -> Option<NativeBuffer> {
        if self.dimensions != Some(size) {
            // SAFETY: The pool validates its dimensions and returns a new owned reference.
            let pool = NonNull::new(unsafe {
                nexdesk_cv_capture_pool(size.width as i32, size.height as i32)
            })?;
            *self = Self {
                pool: Some(pool),
                dimensions: Some(size),
            };
        }
        // SAFETY: The pool remains alive. The returned buffer is exclusively locked for writing.
        let locked = LockedCapture(NonNull::new(unsafe {
            nexdesk_cv_capture_begin(self.pool?.as_ptr())
        })?);
        let cv = locked.0.as_ptr();
        // SAFETY: The pool provides distinct writable 420v Y and UV planes at size.
        unsafe {
            let plane = |n: usize, rows: usize| {
                let stride = CVPixelBufferGetBytesPerRowOfPlane(cv, n);
                let ptr = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(cv, n))?;
                Some((
                    slice::from_raw_parts_mut(ptr.as_ptr(), stride.checked_mul(rows)?),
                    u32::try_from(stride).ok()?,
                ))
            };
            let (y, ys) = plane(0, size.height as usize)?;
            let (uv, uvs) = plane(1, size.height.div_ceil(2) as usize)?;
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
            CVPixelBufferUnlockBaseAddress(cv, 0);
            std::mem::forget(locked);
            // The SDK consumes the +1 reference when wrapping the CVPixelBuffer.
            Some(NativeBuffer::from_cv_pixel_buffer(cv))
        }
    }
}

pub(crate) struct DecodedFrame {
    frame: BoxVideoFrame,
    color: i32,
}

impl DecodedFrame {
    pub(crate) fn new(frame: BoxVideoFrame, desktop_sdr: bool) -> Self {
        let image = frame
            .buffer
            .as_native()
            .map_or(std::ptr::null_mut(), |native| native.get_cv_pixel_buffer());
        // SAFETY: The WebRTC frame owns this borrowed CVPixelBuffer. Null is accepted.
        let color = unsafe { nexdesk_cv_color(image, desktop_sdr) };
        Self { frame, color }
    }

    pub(crate) fn width(&self) -> u32 {
        self.frame.buffer.width()
    }
    pub(crate) fn height(&self) -> u32 {
        self.frame.buffer.height()
    }
    pub(crate) fn color_description(&self) -> &'static str {
        match self.color {
            2 => "NV12 · BT.601 · 有限范围",
            3 => "NV12 · BT.601 · 全范围",
            4 => "NV12 · BT.709 · 有限范围",
            5 => "NV12 · BT.709 · 全范围",
            6 => "NV12 · BT.2020 · 有限范围",
            7 => "NV12 · BT.2020 · 全范围",
            _ => crate::sdr::description(self.frame.buffer.buffer_type()),
        }
    }

    pub(crate) fn convert(&self, out: &mut [u8], stride: u32, rgba: bool) {
        let width = self.width() as i32;
        let height = self.height() as i32;
        assert!(stride >= width as u32 * 4 && out.len() >= stride as usize * height as usize);
        if let Some(native) = self.frame.buffer.as_native().filter(|_| self.color != 0) {
            // SAFETY: The immutable source owns the borrowed image; the exclusive destination
            // covers every row. CoreVideo locks and unlocks the source within the call.
            if unsafe {
                nexdesk_cv_to_rgb(
                    native.get_cv_pixel_buffer(),
                    self.color,
                    rgba,
                    out.as_mut_ptr(),
                    stride as i32,
                    width,
                    height,
                )
            } {
                return;
            }
            let image = self.frame.buffer.to_i420();
            let (y, u, v) = image.data();
            let (ys, us, vs) = image.strides();
            // SAFETY: WebRTC's cropped I420 planes live until the call returns. Their
            // dimensions match the logical RTC frame, including ceil-divided chroma.
            unsafe {
                nexdesk_planar_to_rgb(
                    y.as_ptr(),
                    ys as i32,
                    u.as_ptr(),
                    us as i32,
                    v.as_ptr(),
                    vs as i32,
                    out.as_mut_ptr(),
                    stride as i32,
                    width,
                    height,
                    self.color,
                    rgba,
                    420,
                )
            };
        } else {
            crate::sdr::convert(&self.frame, out, stride, rgba);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFrame, pixels::PixelPool};
    use libwebrtc::video_frame::{VideoBuffer, VideoFrame, VideoRotation, native::NativeBuffer};

    unsafe extern "C" {
        fn CVPixelBufferCreate(
            allocator: *const c_void,
            width: usize,
            height: usize,
            format: u32,
            attributes: *const c_void,
            image: *mut *mut c_void,
        ) -> i32;
        fn CVPixelBufferLockBaseAddress(image: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(image: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(image: *mut c_void, plane: usize) -> *mut u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(image: *mut c_void, plane: usize) -> usize;
        fn CVBufferSetAttachment(
            image: *mut c_void,
            key: *const c_void,
            value: *const c_void,
            mode: u32,
        );
        static kCVImageBufferYCbCrMatrixKey: *const c_void;
        static kCVImageBufferYCbCrMatrix_ITU_R_709_2: *const c_void;
        static kCVImageBufferYCbCrMatrix_ITU_R_2020: *const c_void;
    }

    #[test]
    fn apple_texture_and_portable_ffi_agree_on_real_cv_buffers() {
        for (fourcc, y, expected) in [(b"420f", 32, 32u8), (b"420v", 44, 33)] {
            let mut cv = std::ptr::null_mut();
            // SAFETY: CoreVideo allocates a 4x2 bi-planar buffer. Write its Y/UV rows
            // while locked, then transfer the owned buffer to WebRTC's native wrapper.
            let native = unsafe {
                assert_eq!(
                    CVPixelBufferCreate(
                        std::ptr::null(),
                        4,
                        2,
                        u32::from_be_bytes(*fourcc),
                        std::ptr::null(),
                        &mut cv
                    ),
                    0
                );
                let range = i32::from(fourcc == b"420f");
                CVBufferSetAttachment(
                    cv,
                    kCVImageBufferYCbCrMatrixKey,
                    kCVImageBufferYCbCrMatrix_ITU_R_2020,
                    1,
                );
                assert_eq!(nexdesk_cv_color(cv, true), 6 + range);
                CVBufferSetAttachment(
                    cv,
                    kCVImageBufferYCbCrMatrixKey,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                    1,
                );
                assert_eq!(nexdesk_cv_color(cv, true), 2 + range);
                assert_eq!(nexdesk_cv_color(cv, false), 4 + range);
                assert_eq!(CVPixelBufferLockBaseAddress(cv, 0), 0);
                let luma = CVPixelBufferGetBaseAddressOfPlane(cv, 0);
                let stride = CVPixelBufferGetBytesPerRowOfPlane(cv, 0);
                for row in 0..2 {
                    luma.add(row * stride).write_bytes(y, 4);
                }
                CVPixelBufferGetBaseAddressOfPlane(cv, 1)
                    .copy_from_nonoverlapping([128, 128, 100, 161].as_ptr(), 4);
                assert_eq!(CVPixelBufferUnlockBaseAddress(cv, 0), 0);
                NativeBuffer::from_cv_pixel_buffer(cv)
            };
            let frame = VideoFrame::new(
                VideoRotation::VideoRotation0,
                Box::new(native) as Box<dyn VideoBuffer>,
            );
            let pixels = PixelFrame::decode(frame, &PixelPool::default(), false).unwrap();
            assert!(pixels.color_conversion().contains("709"));
            assert_eq!(
                pixels.color_conversion().contains("全范围"),
                fourcc == b"420f"
            );
            let mut bgra = [99; 48];
            assert!(pixels.copy_bgra(&mut bgra, 24));
            assert!(bgra[0].abs_diff(expected) <= 1);
            assert_eq!(&bgra[16..24], &[99; 8]);
            let [r, g, b, a] = pixels.rgba[8..12] else {
                unreachable!()
            };
            assert_eq!(&bgra[8..12], &[b, g, r, a]);
        }
    }
}
