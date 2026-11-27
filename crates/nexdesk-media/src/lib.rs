#![deny(unsafe_code)]

mod capture;
mod channel;
#[allow(unsafe_code)]
mod desktop_capture;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mac_pixels;
mod peer;
mod pixels;
mod quality;
#[allow(unsafe_code)]
mod sdr;
mod sound;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows_h264;

pub use capture::{
    CaptureKind, CaptureRegion, CaptureSourceInfo, CaptureStatus, CaptureTarget, capture_sources,
};
pub use channel::{CONTROL_PACKET_LIMIT, ControlPipe};
pub use libwebrtc::peer_connection_factory::IceServer;
pub use peer::{
    Codec, ControllerOffer, DesktopMediaSettings, DesktopPipes, EncodingPreference, HostMedia,
    HostPeer, IceOptions, IcePathStats, MediaConfig, MediaStats, VideoLimitation, View,
};
pub use pixels::{FramePixels, PixelFrame};
pub use quality::{FrameSize, Preset, Quality, ResolutionLimit};
pub use sound::{ReceivedSound, SoundStatus};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("capture source is unavailable or permission was not granted")]
    CaptureUnavailable,
    #[error("could not start the capture worker")]
    CaptureWorker(#[source] std::io::Error),
    #[error("description is outside the M1 media profile")]
    InvalidDescription,
    #[error("ICE did not produce a usable LAN description before the deadline")]
    IceTimeout,
    #[error("no video track arrived before the deadline")]
    VideoTimeout,
    #[error("unsupported or oversized video frame")]
    InvalidFrame,
    #[error("the requested video codec is unavailable")]
    CodecUnavailable,
    #[error("native WebRTC operation failed")]
    WebRtc,
}

impl From<libwebrtc::RtcError> for MediaError {
    fn from(_: libwebrtc::RtcError) -> Self {
        Self::WebRtc
    }
}
