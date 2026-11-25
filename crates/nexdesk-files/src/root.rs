use std::{collections::BTreeMap, io, path::Path};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
#[cfg(windows)]
use cap_std::fs::MetadataExt;
use cap_std::{
    ambient_authority,
    fs::{Dir, File, Metadata, OpenOptions},
};
use serde::{Deserialize, Serialize};

use crate::{Conflict, FileError};

const PAGE_SIZE: usize = 24;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 960;
const MAX_DIRECTORY_ENTRIES: usize = 200_000;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RelativePath(pub Vec<String>);

impl RelativePath {
    pub fn child(&self, name: String) -> Result<Self, FileError> {
        let mut path = self.clone();
        path.0.push(name);
        path.validate()?;
        Ok(path)
    }

    pub fn validate(&self) -> Result<(), FileError> {
        if self.0.len() > MAX_DEPTH
            || self.0.iter().map(|name| name.len() + 1).sum::<usize>() > MAX_PATH_BYTES
            || self.0.iter().any(|name| !valid_name(name))
        {
            return Err(FileError::Invalid);
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or_default().trim_end();
    if lower.starts_with(".nexdesk-")
        || matches!(stem, "con" | "prn" | "aux" | "nul" | "conin$" | "conout$")
    {
        return false;
    }
    !["com", "lpt"].into_iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next: Option<String>,
}

/// The only ambient open happens when the local operator selects a directory.
pub struct Root(Dir);

impl Root {
    pub fn open(path: &Path) -> Result<Self, FileError> {
        if !path.is_absolute() {
            return Err(FileError::Invalid);
        }
        Ok(Self(Dir::open_ambient_dir(path, ambient_authority())?))
    }

    pub fn page(&self, path: &RelativePath, after: Option<&str>) -> Result<Page, FileError> {
        if after.is_some_and(|name| !valid_name(name)) {
            return Err(FileError::Invalid);
        }
        path.validate()?;
        let directory = self.directory(&path.0)?;
        let mut candidates = BTreeMap::new();
        for (index, item) in directory.entries()?.enumerate() {
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err(FileError::Limit);
            }
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if !valid_name(&name)
                || after.is_some_and(|cursor| name.as_str() <= cursor)
                || (candidates.len() > PAGE_SIZE
                    && candidates
                        .last_key_value()
                        .is_some_and(|(last, _)| &name > last))
            {
                continue;
            }
            let Ok(meta) = directory.symlink_metadata(&name) else {
                continue;
            };
            if redirected(&meta) {
                continue;
            }
            let kind = if meta.is_dir() {
                EntryKind::Directory
            } else if regular(&meta) {
                EntryKind::File
            } else {
                continue;
            };
            candidates.insert(
                name.clone(),
                Entry {
                    name,
                    kind,
                    size: meta.len(),
                },
            );
            if candidates.len() > PAGE_SIZE + 1 {
                candidates.pop_last();
            }
        }
        let next = if candidates.len() > PAGE_SIZE {
            candidates.pop_last();
            candidates.last_key_value().map(|(name, _)| name.clone())
        } else {
            None
        };
        Ok(Page {
            entries: candidates.into_values().collect(),
            next,
        })
    }

    pub fn create_directory(
        &self,
        path: &RelativePath,
        conflict: Conflict,
    ) -> Result<Option<String>, FileError> {
        let (parent, basename) = self.parent(path)?;
        for index in 1..=256 {
            let name = collision_name(basename, index, false)?;
            match parent.create_dir(&name) {
                Ok(()) => return Ok(Some(name)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match conflict {
                    Conflict::KeepBoth => {}
                    Conflict::Skip => return Ok(None),
                    Conflict::Replace => {
                        open_child(&parent, &name)?;
                        return Ok(Some(name));
                    }
                },
                Err(error) => return Err(error.into()),
            }
        }
        Err(FileError::Conflict)
    }

    pub(crate) fn source(&self, path: &RelativePath) -> Result<File, FileError> {
        let (parent, name) = self.parent(path)?;
        if !regular(&parent.symlink_metadata(name)?) {
            return Err(FileError::Denied);
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = parent.open_with(name, &options)?;
        if !regular(&file.metadata()?) {
            return Err(FileError::Denied);
        }
        Ok(file)
    }

    pub(crate) fn parent<'a>(&self, path: &'a RelativePath) -> Result<(Dir, &'a str), FileError> {
        path.validate()?;
        let (name, ancestors) = path.0.split_last().ok_or(FileError::Invalid)?;
        Ok((self.directory(ancestors)?, name))
    }

    fn directory(&self, names: &[String]) -> Result<Dir, FileError> {
        let mut directory = self.0.try_clone()?;
        for name in names {
            directory = open_child(&directory, name)?;
        }
        Ok(directory)
    }
}

fn open_child(parent: &Dir, name: &str) -> Result<Dir, FileError> {
    let directory = parent.open_dir_nofollow(name).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            FileError::Missing
        } else {
            FileError::Denied
        }
    })?;
    if redirected(&directory.dir_metadata()?) {
        return Err(FileError::Denied);
    }
    Ok(directory)
}

pub(crate) fn regular(meta: &Metadata) -> bool {
    meta.is_file() && !redirected(meta) && cap_fs_ext::MetadataExt::nlink(meta) == 1
}

fn redirected(meta: &Metadata) -> bool {
    #[cfg(windows)]
    if meta.file_attributes() & 0x400 != 0 {
        return true;
    }
    meta.is_symlink()
}

pub(crate) fn collision_name(name: &str, index: u16, file: bool) -> Result<String, FileError> {
    if index == 1 {
        return Ok(name.to_owned());
    }
    let (body, extension) = match name.rsplit_once('.') {
        Some((body, extension)) if file && !body.is_empty() => (body, format!(".{extension}")),
        _ => (name, String::new()),
    };
    let suffix = format!(" ({index}){extension}");
    let mut cut = body.len().min(255usize.saturating_sub(suffix.len()));
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let result = format!("{}{suffix}", &body[..cut]);
    if valid_name(&result) {
        Ok(result)
    } else {
        Err(FileError::Conflict)
    }
}
