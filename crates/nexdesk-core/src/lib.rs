#![forbid(unsafe_code)]

mod target;

pub use target::{DEFAULT_LAN_PORT, DeviceId, LanEndpoint, TargetError, local_listener_ip};

pub const CONTROL_PACKET_LIMIT: usize = 16 * 1024;
