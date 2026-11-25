//! Bounded, versioned requests for desktop collaboration and file data channels.

use nexdesk_media::{MediaStats, Quality};
use nexdesk_platform::{AudioStatus, DisplayMode};
use serde::{Deserialize, Serialize};

pub use crate::envelope::{decode, encode};

pub const TEXT_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Capabilities(u32);

impl Capabilities {
    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const AUDIO: Self = Self(1);
    pub const CLIPBOARD: Self = Self(2);
    pub const FILE_READ: Self = Self(4);
    pub const FILE_WRITE: Self = Self(8);
    pub const DISPLAY: Self = Self(16);
    pub const TEXT: Self = Self(32);
    pub const SHELL: Self = Self(64);
    pub const ALL: Self = Self(127);

    pub fn from_bits(value: u32) -> Option<Self> {
        (value & !Self::ALL.0 == 0).then_some(Self(value))
    }

    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
    pub fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
    pub fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub fn includes(self, other: Self) -> bool {
        self.intersection(other) == other
    }
    pub fn any_files(self) -> bool {
        self.0 & (Self::FILE_READ.0 | Self::FILE_WRITE.0) != 0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Quality { quality: Quality },
    Diagnostics,
    ClipboardRead,
    ClipboardWrite { text: String },
    Text { generation: u64, text: String },
    Modes { display: String },
    Apply { display: String, mode: DisplayMode },
    Confirm { token: u64 },
    Rollback { token: u64 },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Ok,
    Quality {
        quality: Quality,
    },
    Diagnostics {
        media: MediaStats,
        audio: AudioStatus,
    },
    Clipboard {
        text: String,
    },
    Modes {
        display: String,
        current: DisplayMode,
        choices: Vec<DisplayMode>,
    },
    Preview {
        token: u64,
        display: String,
        mode: DisplayMode,
        expires_ms: u32,
    },
    Confirmed {
        display: String,
        mode: DisplayMode,
    },
    Restored {
        current: DisplayMode,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    Denied,
    Unavailable,
    Invalid,
    Changed,
    Busy,
    Expired,
    Disconnected,
    Storage,
}

pub fn valid_text(text: &str) -> bool {
    text.len() <= TEXT_LIMIT
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
}
