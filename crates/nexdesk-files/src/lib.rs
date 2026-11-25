//! Directory-scoped file operations and transfer staging.
#![forbid(unsafe_code)]

mod root;
mod transfer;

pub use root::{Entry, EntryKind, Page, RelativePath, Root};
pub use transfer::{Incoming, Opened, Outgoing, Stamp, TransferStore};

use serde::{Deserialize, Serialize};
use std::io;

pub const BLOCK: usize = 12 * 1024;
pub const MAX_ITEMS: usize = 4096;
pub const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Conflict {
    KeepBoth,
    Replace,
    Skip,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileError {
    #[error("invalid path or request")]
    Invalid,
    #[error("path is outside the selected scope or unavailable")]
    Denied,
    #[error("the requested item no longer exists")]
    Missing,
    #[error("the source or confirmed range has changed")]
    Changed,
    #[error("the destination has an incompatible item")]
    Conflict,
    #[error("the transfer was canceled")]
    Canceled,
    #[error("the file connection could not be restored")]
    Disconnected,
    #[error("a directory or file limit was reached")]
    Limit,
    #[error("the destination disk is full")]
    Full,
    #[error("the file operation failed")]
    Io,
}

impl From<io::Error> for FileError {
    fn from(error: io::Error) -> Self {
        use io::ErrorKind;
        match error.kind() {
            ErrorKind::NotFound => Self::Missing,
            ErrorKind::AlreadyExists => Self::Conflict,
            ErrorKind::PermissionDenied | ErrorKind::NotADirectory => Self::Denied,
            ErrorKind::StorageFull => Self::Full,
            _ => Self::Io,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRequest {
    List {
        path: RelativePath,
        after: Option<String>,
    },
    Directory {
        id: u64,
        path: RelativePath,
        conflict: Conflict,
    },
    ReadOpen {
        path: RelativePath,
        expected: Option<Stamp>,
    },
    ReadBlock {
        offset: u64,
    },
    WriteOpen {
        id: u64,
        path: RelativePath,
        stamp: Stamp,
        conflict: Conflict,
    },
    WriteBlock {
        id: u64,
        offset: u64,
    },
    Commit {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileReply {
    Page { page: Page },
    Directory { name: Option<String> },
    Source { stamp: Stamp },
    Block { offset: u64 },
    Destination { status: Opened },
    Written { offset: u64 },
    Complete { name: Option<String> },
    Canceled,
}
