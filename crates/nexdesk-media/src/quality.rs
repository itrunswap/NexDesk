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

    pub fn bitrate_limit(self) -> Option<u64> {
        use ResolutionLimit::{FullHd, Hd, Qhd, Uhd};

        let mbps = self.max_mbps.or(match (self.preset, self.resolution) {
            (Preset::Auto, _) => None,
            (Preset::Clear | Preset::Smooth, Some(Hd)) => Some(6),
            (Preset::Clear, Some(FullHd)) => Some(10),
            (Preset::Smooth, Some(FullHd)) => Some(14),
            (Preset::Clear, Some(Qhd)) => Some(16),
            (Preset::Smooth, Some(Qhd)) => Some(24),
            (Preset::Clear, Some(Uhd)) => Some(32),
            (Preset::Smooth, Some(Uhd)) => Some(48),
            (Preset::Clear, None) => Some(12),
            (Preset::Smooth, None) => Some(6),
        });
        mbps.map(|value| u64::from(value) * 1_000_000)
    }

    pub(crate) fn apply(self, sender: &RtpSender) -> Result<(), MediaError> {
        if !self.valid() {
            return Err(MediaError::InvalidFrame);
        }
        let degradation = match self.preset {
            Preset::Auto => DegradationPreference::Balanced,
            Preset::Clear => DegradationPreference::MaintainResolution,
            Preset::Smooth => DegradationPreference::MaintainFramerate,
        };
        let bitrate = self.bitrate_limit();
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
    fn quality_keeps_legacy_limits_and_allows_bounded_overrides() {
        let legacy = Quality::new(Preset::Smooth, 30);
        assert_eq!(legacy.bitrate_limit(), Some(6_000_000));
        let high = Quality {
            resolution: Some(ResolutionLimit::Uhd),
            ..legacy
        };
        assert!(high.valid());
        assert_eq!(high.bitrate_limit(), Some(48_000_000));
        assert_eq!(
            Quality {
                max_mbps: Some(24),
                ..high
            }
            .bitrate_limit(),
            Some(24_000_000)
        );
        assert!(
            !Quality {
                max_mbps: Some(81),
                ..high
            }
            .valid()
        );
    }
}
