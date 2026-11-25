#![forbid(unsafe_code)]

#[cfg(feature = "native")]
mod lan;
pub mod pairing;

#[cfg(feature = "native")]
pub use lan::{
    ConnectedDesktop, ConnectedViewer, DesktopAnswer, DesktopOffer, DesktopRequest, SessionError,
    connect, connect_desktop, connect_routed, ice_options,
};
#[cfg(feature = "native")]
pub mod collaboration;
pub mod control;
#[cfg(feature = "native")]
pub mod controller;
pub mod envelope;
#[cfg(feature = "native")]
pub mod terminal;
