use std::{
    ops::Deref,
    sync::{Arc, Mutex},
    time::Instant,
};

use libwebrtc::video_frame::BoxVideoFrame;

use crate::{MediaError, capture::valid_size};

// Keep one idle allocation per viewer. Published frames and FFI leases retain
// exclusive ownership of their storage until their last reader drops it.
#[derive(Clone, Default)]
pub(crate) struct PixelPool(Arc<Mutex<Option<Vec<u8>>>>);

impl PixelPool {
    fn take(&self, len: usize) -> RgbaBytes {
        let reusable = self.0.lock().ok().and_then(|mut slot| slot.take());
        let bytes = reusable
            .filter(|bytes| bytes.len() == len)
            .unwrap_or_else(|| vec![0; len]);
        RgbaBytes {
            bytes,
            pool: self.clone(),
        }
    }
}

struct RgbaBytes {
    bytes: Vec<u8>,
    pool: PixelPool,
}

impl Drop for RgbaBytes {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.pool.0.lock() {
            *slot = Some(std::mem::take(&mut self.bytes));
        }
    }
}

pub struct FramePixels {
    #[cfg(not(target_os = "macos"))]
    bytes: RgbaBytes,
    #[cfg(target_os = "macos")]
    native: crate::mac_pixels::DecodedFrame,
    #[cfg(target_os = "macos")]
    portable: std::sync::OnceLock<RgbaBytes>,
    #[cfg(target_os = "macos")]
    pool: PixelPool,
}

impl Deref for FramePixels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        #[cfg(target_os = "macos")]
        {
            &self
                .portable
                .get_or_init(|| {
                    let stride = self.native.width() * 4;
                    let mut storage = self.pool.take((stride * self.native.height()) as usize);
                    self.native.convert(&mut storage.bytes, stride, true);
                    storage
                })
                .bytes
        }
        #[cfg(not(target_os = "macos"))]
        {
            &self.bytes.bytes
        }
    }
}

pub struct PixelFrame {
    #[cfg(not(target_os = "macos"))]
    color: &'static str,
    pub rgba: FramePixels,
    pub width: u32,
    pub height: u32,
    pub decoded_at: Instant,
}

impl PixelFrame {
    pub(crate) fn decode(
        frame: BoxVideoFrame,
        pool: &PixelPool,
        #[cfg(target_os = "macos")] desktop_sdr: bool,
    ) -> Result<Self, MediaError> {
        let decoded_at = Instant::now();
        let width = frame.buffer.width();
        let height = frame.buffer.height();
        if !valid_size(width, height) {
            return Err(MediaError::InvalidFrame);
        }
        #[cfg(not(target_os = "macos"))]
        let rgba = {
            let stride = width * 4;
            let mut bytes = pool.take((stride * height) as usize);
            crate::sdr::convert(&frame, &mut bytes.bytes, stride, true);
            FramePixels { bytes }
        };
        #[cfg(target_os = "macos")]
        let rgba = FramePixels {
            native: crate::mac_pixels::DecodedFrame::new(frame, desktop_sdr),
            portable: std::sync::OnceLock::new(),
            pool: pool.clone(),
        };
        Ok(Self {
            #[cfg(not(target_os = "macos"))]
            color: crate::sdr::description(frame.buffer.buffer_type()),
            rgba,
            width,
            height,
            decoded_at,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn copy_bgra(&self, destination: &mut [u8], stride: u32) -> bool {
        if stride < self.width * 4
            || stride > i32::MAX as u32
            || (stride as usize)
                .checked_mul(self.height as usize)
                .is_none_or(|bytes| bytes > destination.len())
        {
            return false;
        }
        self.rgba.native.convert(destination, stride, false);
        true
    }

    pub fn color_conversion(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        {
            self.rgba.native.color_description()
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.color
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn nexdesk_nv12_to_rgb(
            y: *const u8,
            ys: i32,
            uv: *const u8,
            uvs: i32,
            out: *mut u8,
            stride: i32,
            width: i32,
            height: i32,
            color: i32,
            rgba: bool,
        ) -> bool;
        fn nexdesk_planar_to_rgb(
            y: *const u8,
            ys: i32,
            u: *const u8,
            us: i32,
            v: *const u8,
            vs: i32,
            out: *mut u8,
            stride: i32,
            width: i32,
            height: i32,
            color: i32,
            rgba: bool,
            sampling: i32,
        );
    }

    fn color_chip(y: u8, u: u8, v: u8, color: i32, rgba: bool) -> [u8; 3] {
        let mut rgb = [0x55u8; 24];
        let mut planar = rgb;
        // SAFETY: Both conversions read a 2x2 image with one chroma pair,
        // writing two 8-byte rows in separate 12-byte-stride destinations.
        unsafe {
            assert!(nexdesk_nv12_to_rgb(
                [y; 4].as_ptr(),
                2,
                [u, v].as_ptr(),
                2,
                rgb.as_mut_ptr(),
                12,
                2,
                2,
                color,
                rgba
            ));
            nexdesk_planar_to_rgb(
                [y; 4].as_ptr(),
                2,
                &u,
                1,
                &v,
                1,
                planar.as_mut_ptr(),
                12,
                2,
                2,
                color,
                rgba,
                420,
            );
        }
        assert_eq!(rgb, planar);
        assert_eq!(rgb[3], 255);
        assert_eq!(&rgb[8..12], &[0x55; 4]);
        assert_eq!(&rgb[0..8], &rgb[12..20]);
        [rgb[0], rgb[1], rgb[2]]
    }

    #[test]
    fn grey_ramp_survives_full_and_video_range() {
        for color in 2..=7 {
            for level in [
                0u8, 8, 16, 24, 32, 48, 64, 96, 128, 160, 192, 216, 224, 235, 240, 248, 255,
            ] {
                let y = if color % 2 == 1 {
                    level
                } else {
                    (16 + (u16::from(level) * 219 + 127) / 255) as u8
                };
                assert!(
                    color_chip(y, 128, 128, color, true)
                        .iter()
                        .all(|&v| v.abs_diff(level) <= 2),
                    "matrix {color} grey {level}"
                );
            }
        }
    }

    #[test]
    fn color_matrices_match_sdr_equations_and_texture_channel_order() {
        for (color, kr, kb) in [(2, 0.299, 0.114), (4, 0.2126, 0.0722), (6, 0.2627, 0.0593)] {
            for full in [false, true] {
                let y = if full { 128.0 } else { 112.0 * 255.0 / 219.0 };
                let gain = if full { 1.0 } else { 255.0 / 224.0 };
                let red = y + 2.0 * (1.0 - kr) * 33.0 * gain;
                let blue = y - 2.0 * (1.0 - kb) * 28.0 * gain;
                let green = (y - kr * red - kb * blue) / (1.0 - kr - kb);
                let expected = [red, green, blue].map(|v: f64| v.round() as u8);
                let [r, g, b] = color_chip(128, 100, 161, color + i32::from(full), true);
                assert!(r.abs_diff(expected[0]) <= 2 && g.abs_diff(expected[1]) <= 2);
                // libyuv's fixed-point SIMD tables may cap the blue U multiplier
                // at 2.0. Both unlimited and capped SDK builds are supported.
                let capped_blue = (y - 28.0 * (2.0 * (1.0 - kb) * gain).min(2.0)).round() as u8;
                assert!(
                    (expected[2].saturating_sub(1)..=capped_blue.saturating_add(1)).contains(&b)
                );
                assert_eq!(
                    color_chip(128, 100, 161, color + i32::from(full), false),
                    [b, g, r]
                );
            }
        }
    }

    #[test]
    fn a_live_frame_cannot_be_recycled() {
        let pool = PixelPool::default();
        let mut first = pool.take(100);
        first.bytes.fill(2);
        let second = pool.take(100);
        let address = second.bytes.as_ptr();
        assert_ne!(address, first.bytes.as_ptr());
        drop(second);
        let mut reused = pool.take(100);
        assert_eq!(reused.bytes.as_ptr(), address);
        reused.bytes.fill(3);
        assert!(first.bytes.iter().all(|&byte| byte == 2));
    }
}
