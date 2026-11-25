//! Messages carried on the independently authorized PTY channel.

use serde::{Deserialize, Serialize};

pub const CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    pub fn valid(self) -> bool {
        (2..=512).contains(&self.cols) && (2..=256).contains(&self.rows)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Open { size: Size },
    Input,
    Resize { size: Size },
    Close,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Opened,
    Output,
    Ended,
    Unavailable,
}
