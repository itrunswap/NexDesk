//! Convert in the decoder's actual subsampling format. The generic WebRTC
//! converter first reduces NV12, I422 and I444 to I420.
use libwebrtc::video_frame::{BoxVideoFrame, VideoBufferType, VideoFormatType};

pub(crate) fn description(format: VideoBufferType) -> &'static str {
    match format {
        VideoBufferType::I444 => "I444 · 4:4:4 · BT.601 默认矩阵",
        VideoBufferType::I422 => "I422 · 4:2:2 · BT.601 默认矩阵",
        VideoBufferType::NV12 => "NV12 · 4:2:0 · BT.601 默认矩阵",
        VideoBufferType::I420 | VideoBufferType::I420A => "I420 · 4:2:0 · BT.601 默认矩阵",
        _ => "WebRTC 默认 SDR 转换",
    }
}

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
    pub(crate) fn nexdesk_planar_to_rgb(
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

pub(crate) fn convert(frame: &BoxVideoFrame, out: &mut [u8], stride: u32, rgba: bool) {
    const BT601_VIDEO: i32 = 2;
    let width = frame.buffer.width() as i32;
    let height = frame.buffer.height() as i32;
    assert!(stride >= width as u32 * 4 && out.len() >= stride as usize * height as usize);
    if let Some(image) = frame.buffer.as_nv12() {
        let (y, uv) = image.data();
        let (ys, uvs) = image.strides();
        // SAFETY: WebRTC owns both planes for the call; rows/columns match the decoded buffer.
        if unsafe {
            nexdesk_nv12_to_rgb(
                y.as_ptr(),
                ys as i32,
                uv.as_ptr(),
                uvs as i32,
                out.as_mut_ptr(),
                stride as i32,
                width,
                height,
                BT601_VIDEO,
                rgba,
            )
        } {
            return;
        }
    }
    let planes = if let Some(image) = frame.buffer.as_i444() {
        Some((444, image.data(), image.strides()))
    } else if let Some(image) = frame.buffer.as_i422() {
        Some((422, image.data(), image.strides()))
    } else {
        frame.buffer.as_i420a().map(|image| {
            let (y, u, v, _) = image.data();
            let (ys, us, vs, _) = image.strides();
            (420, (y, u, v), (ys, us, vs))
        })
    };
    if let Some((sampling, (y, u, v), (ys, us, vs))) = planes {
        // SAFETY: The selected libyuv routine reads the matching chroma layout
        // and uses the reported independent strides. Desktop output is opaque.
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
                BT601_VIDEO,
                rgba,
                sampling,
            )
        };
    } else {
        let format = if rgba {
            VideoFormatType::ABGR
        } else {
            VideoFormatType::ARGB
        };
        frame.buffer.to_argb(format, out, stride, width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libwebrtc::video_frame::{I444Buffer, VideoBuffer, VideoFrame, VideoRotation};

    #[test]
    fn planar_444_retains_one_pixel_color_stripes() {
        let mut image = I444Buffer::new(4, 2);
        let (_, us, vs) = image.strides();
        let (y, u, v) = image.data_mut();
        y.fill(128);
        for row in 0..2 {
            u[row * us as usize..][..4].copy_from_slice(&[96, 160, 96, 160]);
            v[row * vs as usize..][..4].copy_from_slice(&[160, 96, 160, 96]);
        }
        let frame = VideoFrame::new(
            VideoRotation::VideoRotation0,
            Box::new(image) as Box<dyn VideoBuffer>,
        );
        let mut pixels = [0; 32];
        convert(&frame, &mut pixels, 16, true);
        assert!(pixels[0] > pixels[4] + 80 && pixels[6] > pixels[2] + 100);
        assert_eq!(&pixels[..8], &pixels[8..16]);
    }
}
