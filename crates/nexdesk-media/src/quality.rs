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
pub struct Quality {
    pub preset: Preset,
    pub fps: u16,
}

impl Quality {
    pub fn valid(self) -> bool {
        (10..=60).contains(&self.fps)
    }

    pub(crate) fn apply(self, sender: &RtpSender) -> Result<(), MediaError> {
        if !self.valid() {
            return Err(MediaError::InvalidFrame);
        }
        let (bitrate, degradation) = match self.preset {
            Preset::Auto => (None, DegradationPreference::Balanced),
            Preset::Clear => (Some(12_000_000), DegradationPreference::MaintainResolution),
            Preset::Smooth => (Some(6_000_000), DegradationPreference::MaintainFramerate),
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
