use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{self, Read, Seek, SeekFrom, Write},
};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    fs::{Dir, File, OpenOptions},
    time::SystemTime,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BLOCK, Conflict, FileError, FileReply, FileRequest, MAX_FILE_SIZE, MAX_ITEMS, RelativePath,
    Root,
    root::{collision_name, regular},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Stamp {
    pub size: u64,
    pub sha256: [u8; 32],
}

pub struct Outgoing {
    file: File,
    stamp: Stamp,
    modified: SystemTime,
}

impl Outgoing {
    pub fn open(
        root: &Root,
        path: &RelativePath,
        expected: Option<&Stamp>,
        canceled: impl Fn() -> bool,
    ) -> Result<Self, FileError> {
        let mut file = root.source(path)?;
        let before = file.metadata()?;
        if before.len() > MAX_FILE_SIZE {
            return Err(FileError::Limit);
        }
        let modified = before.modified()?;
        let stamp = Stamp {
            size: before.len(),
            sha256: hash(&mut file, before.len(), canceled)?,
        };
        let source = Self {
            file,
            stamp,
            modified,
        };
        if expected.is_some_and(|previous| previous != &source.stamp) {
            return Err(FileError::Changed);
        }
        source.check()?;
        Ok(source)
    }

    pub fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    pub fn block(&mut self, offset: u64) -> Result<Vec<u8>, FileError> {
        self.check()?;
        let left = self
            .stamp
            .size
            .checked_sub(offset)
            .ok_or(FileError::Invalid)?;
        let mut bytes = vec![0; left.min(BLOCK as u64) as usize];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        self.check()?;
        Ok(bytes)
    }

    fn check(&self) -> Result<(), FileError> {
        let meta = self.file.metadata()?;
        if !regular(&meta) || meta.len() != self.stamp.size || meta.modified()? != self.modified {
            return Err(FileError::Changed);
        }
        Ok(())
    }
}

pub struct Incoming {
    parent: Dir,
    target: String,
    temp: String,
    file: Option<File>,
    stamp: Stamp,
    offset: u64,
    conflict: Conflict,
}

impl Incoming {
    pub fn open(
        root: &Root,
        path: &RelativePath,
        stamp: Stamp,
        conflict: Conflict,
    ) -> Result<Option<Self>, FileError> {
        if stamp.size > MAX_FILE_SIZE {
            return Err(FileError::Limit);
        }
        let (parent, target) = root.parent(path)?;
        match parent.symlink_metadata(target) {
            Ok(_) if conflict == Conflict::Skip => return Ok(None),
            Ok(existing) if conflict == Conflict::Replace && !regular(&existing) => {
                return Err(FileError::Conflict);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut random = [0; 16];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| FileError::Io)?;
        let mut temp = String::from(".nexdesk-");
        for byte in random {
            write!(temp, "{byte:02x}").map_err(|_| FileError::Io)?;
        }
        temp.push_str(".part");
        let mut options = OpenOptions::new();
        options
            .create_new(true)
            .read(true)
            .write(true)
            .follow(FollowSymlinks::No);
        let file = parent.open_with(&temp, &options)?;
        Ok(Some(Self {
            parent,
            target: target.to_owned(),
            temp,
            file: Some(file),
            stamp,
            offset: 0,
            conflict,
        }))
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn append(&mut self, offset: u64, bytes: &[u8]) -> Result<u64, FileError> {
        if offset != self.offset
            || !(1..=BLOCK).contains(&bytes.len())
            || bytes.len() as u64 > self.stamp.size - self.offset
        {
            return Err(FileError::Changed);
        }
        let file = self.file.as_mut().ok_or(FileError::Io)?;
        file.seek(SeekFrom::Start(offset))?;
        if let Err(error) = file.write_all(bytes) {
            file.set_len(offset)?;
            return Err(error.into());
        }
        self.offset += bytes.len() as u64;
        Ok(self.offset)
    }

    pub fn commit(mut self, canceled: impl Fn() -> bool) -> Result<Option<String>, FileError> {
        let mut file = self.file.take().ok_or(FileError::Io)?;
        if self.offset != self.stamp.size
            || file.metadata()?.len() != self.offset
            || hash(&mut file, self.offset, &canceled)? != self.stamp.sha256
        {
            return Err(FileError::Changed);
        }
        file.sync_all()?;
        if canceled() {
            return Err(FileError::Canceled);
        }
        let meta = file.metadata()?;
        let staged = self.parent.symlink_metadata(&self.temp)?;
        if !regular(&staged)
            || cap_fs_ext::MetadataExt::ino(&meta) != cap_fs_ext::MetadataExt::ino(&staged)
            || cap_fs_ext::MetadataExt::dev(&meta) != cap_fs_ext::MetadataExt::dev(&staged)
        {
            return Err(FileError::Changed);
        }
        drop(file);
        if self.conflict == Conflict::Replace {
            match self.parent.symlink_metadata(&self.target) {
                Ok(meta) if !regular(&meta) => return Err(FileError::Conflict),
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
            self.parent.rename(&self.temp, &self.parent, &self.target)?;
            self.temp.clear();
            return Ok(Some(self.target.clone()));
        }
        for index in 1..=256 {
            let name = collision_name(&self.target, index, true)?;
            match self.parent.hard_link(&self.temp, &self.parent, &name) {
                Ok(()) => return Ok(Some(name)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if self.conflict == Conflict::Skip {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(FileError::Conflict)
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.temp.is_empty() {
            let _ = self.parent.remove_file(&self.temp);
        }
    }
}

fn hash(file: &mut File, size: u64, canceled: impl Fn() -> bool) -> Result<[u8; 32], FileError> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 128 * 1024];
    let mut left = size;
    file.seek(SeekFrom::Start(0))?;
    while left > 0 {
        if canceled() {
            return Err(FileError::Canceled);
        }
        let bytes = file.read(&mut buffer[..left.min(128 * 1024) as usize])?;
        if bytes == 0 {
            return Err(FileError::Changed);
        }
        digest.update(&buffer[..bytes]);
        left -= bytes as u64;
    }
    let mut trailing = [0];
    if file.read(&mut trailing)? != 0 {
        return Err(FileError::Changed);
    }
    Ok(digest.finalize().into())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Opened {
    Writing { offset: u64 },
    Complete { name: Option<String> },
}

#[derive(Eq, PartialEq)]
struct Destination {
    id: u64,
    path: RelativePath,
    stamp: Stamp,
    conflict: Conflict,
}

struct CreatedDirectory {
    target: RelativePath,
    conflict: Conflict,
    name: Option<String>,
}

/// This state belongs to a single approved receiver, including its short reconnect window.
#[derive(Default)]
pub struct TransferStore {
    reader: Option<Outgoing>,
    writer: Option<(Destination, Incoming)>,
    committed: Option<(Destination, Option<String>)>,
    directories: BTreeMap<u64, CreatedDirectory>,
}

impl TransferStore {
    pub fn execute(
        &mut self,
        root: &Root,
        request: FileRequest,
        bytes: &[u8],
        canceled: impl Fn() -> bool,
    ) -> Result<(FileReply, Vec<u8>), FileError> {
        if canceled() {
            return Err(FileError::Canceled);
        }
        if !matches!(request, FileRequest::WriteBlock { .. }) && !bytes.is_empty() {
            return Err(FileError::Invalid);
        }
        let reply = match request {
            FileRequest::List { path, after } => FileReply::Page {
                page: root.page(&path, after.as_deref())?,
            },
            FileRequest::Directory { id, path, conflict } => FileReply::Directory {
                name: self.directory(root, id, path, conflict)?,
            },
            FileRequest::ReadOpen { path, expected } => {
                let reader = Outgoing::open(root, &path, expected.as_ref(), canceled)?;
                let stamp = reader.stamp().clone();
                self.reader = Some(reader);
                FileReply::Source { stamp }
            }
            FileRequest::ReadBlock { offset } => {
                let block = self
                    .reader
                    .as_mut()
                    .ok_or(FileError::Invalid)?
                    .block(offset)?;
                return Ok((FileReply::Block { offset }, block));
            }
            FileRequest::WriteOpen {
                id,
                path,
                stamp,
                conflict,
            } => {
                let target = Destination {
                    id,
                    path,
                    stamp,
                    conflict,
                };
                FileReply::Destination {
                    status: self.write_open(root, target)?,
                }
            }
            FileRequest::WriteBlock { id, offset } => {
                let (_, writer) = self
                    .writer
                    .as_mut()
                    .filter(|(target, _)| target.id == id)
                    .ok_or(FileError::Invalid)?;
                FileReply::Written {
                    offset: writer.append(offset, bytes)?,
                }
            }
            FileRequest::Commit { id } => self.commit(id, canceled)?,
            FileRequest::Cancel { id } => {
                if self
                    .writer
                    .as_ref()
                    .is_some_and(|(target, _)| target.id == id)
                {
                    self.writer = None;
                }
                self.reader = None;
                FileReply::Canceled
            }
        };
        Ok((reply, Vec::new()))
    }

    fn directory(
        &mut self,
        root: &Root,
        id: u64,
        path: RelativePath,
        conflict: Conflict,
    ) -> Result<Option<String>, FileError> {
        if let Some(created) = self.directories.get(&id) {
            if created.target != path || created.conflict != conflict {
                return Err(FileError::Changed);
            }
            return Ok(created.name.clone());
        }
        if self.directories.len() >= MAX_ITEMS {
            self.directories.pop_first();
        }
        let name = root.create_directory(&path, conflict)?;
        self.directories.insert(
            id,
            CreatedDirectory {
                target: path,
                conflict,
                name: name.clone(),
            },
        );
        Ok(name)
    }

    fn write_open(&mut self, root: &Root, request: Destination) -> Result<Opened, FileError> {
        if let Some((target, name)) = &self.committed
            && target.id == request.id
        {
            if target != &request {
                return Err(FileError::Changed);
            }
            return Ok(Opened::Complete { name: name.clone() });
        }
        if let Some((target, writer)) = &self.writer {
            if target != &request {
                return Err(FileError::Changed);
            }
            return Ok(Opened::Writing {
                offset: writer.offset(),
            });
        }
        match Incoming::open(root, &request.path, request.stamp.clone(), request.conflict)? {
            Some(writer) => {
                self.writer = Some((request, writer));
                Ok(Opened::Writing { offset: 0 })
            }
            None => {
                self.committed = Some((request, None));
                Ok(Opened::Complete { name: None })
            }
        }
    }

    fn commit(&mut self, id: u64, canceled: impl Fn() -> bool) -> Result<FileReply, FileError> {
        if let Some((target, name)) = &self.committed
            && target.id == id
        {
            return Ok(FileReply::Complete { name: name.clone() });
        }
        if self
            .writer
            .as_ref()
            .is_none_or(|(target, _)| target.id != id)
        {
            return Err(FileError::Invalid);
        }
        let (target, writer) = self.writer.take().ok_or(FileError::Invalid)?;
        let name = writer.commit(canceled)?;
        self.committed = Some((target, name.clone()));
        Ok(FileReply::Complete { name })
    }
}
