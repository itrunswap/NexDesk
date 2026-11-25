use serde::{Deserialize, Serialize};

#[cfg_attr(windows, path = "display_windows.rs")]
#[cfg_attr(target_os = "macos", path = "display_macos.rs")]
#[cfg_attr(target_os = "linux", path = "display_linux.rs")]
#[allow(unsafe_code)]
mod backend;

pub use backend::Monitor;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
}

impl DisplayMode {
    pub fn valid(self) -> bool {
        (320..=4096).contains(&self.width)
            && (200..=4096).contains(&self.height)
            && self.width * self.height <= 4096 * 2304
            && self.refresh_millihz <= 500_000
    }
}
