use libwebrtc::{rtp_parameters::DegradationPreference, rtp_sender::RtpSender};
use serde::{Deserialize, Serialize};

use crate::MediaError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    #[default]
    Auto,
    Clear,
    Smooth,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ResolutionLimit {
    Hd = 1,
    FullHd = 2,
    Qhd = 3,
    Uhd = 4,
}

impl ResolutionLimit {
    pub fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Hd),
            2 => Some(Self::FullHd),
            3 => Some(Self::Qhd),
            4 => Some(Self::Uhd),
            _ => None,
        }
    }

    pub fn size(self) -> FrameSize {
        let (width, height) = match self {
            Self::Hd => (1280, 720),
            Self::FullHd => (1920, 1080),
            Self::Qhd => (2560, 1440),
            Self::Uhd => (3840, 2160),
        };
        FrameSize { width, height }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
}

impl FrameSize {
    pub(crate) fn fit(self, bounds: Self) -> Self {
        let Self { width, height } = self;
        let (max_width, max_height) = if height > width {
            (bounds.height, bounds.width)
        } else {
            (bounds.width, bounds.height)
        };
        if width <= max_width && height <= max_height {
            return self;
        }
        let (width, height) = if width * max_height > height * max_width {
            (max_width, height * max_width / width)
        } else {
            (width * max_height / height, max_height)
        };
        Self {
            width: (width & !1).max(2),
            height: (height & !1).max(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Quality {
    pub preset: Preset,
    pub fps: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ResolutionLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mbps: Option<u16>,
}

impl Quality {
    pub fn new(preset: Preset, fps: u16) -> Self {
        Self {
            preset,
            fps,
            resolution: None,
            max_mbps: None,
        }
    }

    pub fn valid(self) -> bool {
        (10..=60).contains(&self.fps) && self.max_mbps.is_none_or(|rate| (1..=80).contains(&rate))
    }

    pub fn bitrate_limit_for(self, source: Option<FrameSize>) -> Option<u64> {
        if let Some(mbps) = self.max_mbps {
            return Some(u64::from(mbps) * 1_000_000);
        }
        if self.preset == Preset::Auto {
            return None;
        }
        let bounds = self.resolution.map(ResolutionLimit::size);
        let output = match (source, bounds) {
            (Some(size), Some(cap)) => size.fit(cap),
            (Some(size), None) => size,
            (None, Some(cap)) => cap,
            (None, None) => ResolutionLimit::Uhd.size(),
        };
        let pixels = u64::from(output.width) * u64::from(output.height);
        let (smooth, clear) = match pixels {
            ..=921_600 => (6, 8),
            921_601..=2_073_600 => (10, 16),
            2_073_601..=3_686_400 => (16, 24),
            _ => (32, 48),
        };
        let base_mbps = if self.preset == Preset::Clear {
            clear
        } else {
            smooth
        };
        // 30 FPS uses the tier's base. More frames share prediction work; at
        // low FPS reserve enough bits per update for fine edges and text.
        Some(base_mbps * 1_000_000 * (30 + u64::from(self.fps)) / 60)
    }

    pub(crate) fn apply(self, sender: &RtpSender, bitrate: Option<u64>) -> Result<(), MediaError> {
        if !self.valid() {
            return Err(MediaError::InvalidFrame);
        }
        let degradation = match self.preset {
            Preset::Auto => DegradationPreference::Balanced,
            Preset::Clear => DegradationPreference::MaintainResolution,
            Preset::Smooth => DegradationPreference::MaintainFramerate,
        };
        let mut params = sender.parameters();
        if params.encodings.is_empty() {
            return Err(MediaError::WebRtc);
        }
        for encoding in &mut params.encodings {
            encoding.max_bitrate = bitrate;
            encoding.max_framerate = Some(f64::from(self.fps));
        }
        params.set_degradation_preference(degradation);
        sender.set_parameters(params)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_cap_preserves_orientation_and_never_upscales() {
        let fhd = ResolutionLimit::FullHd.size();
        let qhd = ResolutionLimit::Qhd.size();
        let uhd = ResolutionLimit::Uhd.size();
        assert_eq!(uhd.fit(qhd), qhd);
        assert_eq!(fhd.fit(uhd), fhd);
        assert_eq!(
            FrameSize {
                width: 2160,
                height: 3840
            }
            .fit(qhd),
            FrameSize {
                width: 1440,
                height: 2560
            }
        );
        assert_eq!(
            FrameSize {
                width: 3440,
                height: 1440
            }
            .fit(qhd),
            FrameSize {
                width: 2560,
                height: 1070
            }
        );
    }

    #[test]
    fn rate_follows_transmitted_pixels_and_user_caps() {
        let clear = Quality::new(Preset::Clear, 30);
        let fhd = ResolutionLimit::FullHd.size();
        let uhd = ResolutionLimit::Uhd.size();
        assert_eq!(clear.bitrate_limit_for(Some(uhd)), Some(48_000_000));
        assert_eq!(clear.bitrate_limit_for(Some(fhd)), Some(16_000_000));
        let qhd = Quality {
            resolution: Some(ResolutionLimit::Qhd),
            ..clear
        };
        assert_eq!(qhd.bitrate_limit_for(Some(uhd)), Some(24_000_000));
        assert_eq!(qhd.bitrate_limit_for(Some(fhd)), Some(16_000_000));
        assert_eq!(
            Quality::new(Preset::Smooth, 60).bitrate_limit_for(Some(uhd)),
            Some(48_000_000)
        );
        assert_eq!(
            Quality::new(Preset::Auto, 30).bitrate_limit_for(Some(uhd)),
            None
        );
        assert_eq!(
            Quality {
                max_mbps: Some(8),
                ..clear
            }
            .bitrate_limit_for(None),
            Some(8_000_000)
        );
        assert!(
            !Quality {
                max_mbps: Some(81),
                ..clear
            }
            .valid()
        );
    }
}
