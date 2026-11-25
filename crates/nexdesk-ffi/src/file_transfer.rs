use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering::Relaxed},
    },
    time::Duration,
};

use nexdesk_files::{
    BLOCK, Conflict, Entry, EntryKind, FileError, FileReply, FileRequest, Incoming, MAX_ITEMS,
    Opened, Outgoing, Page, RelativePath, Root, Stamp,
};
use nexdesk_session::collaboration::Capabilities;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::{
    collaboration::Context,
    rpc::{CallError, Rpc},
};

const RECONNECT: Duration = Duration::from_secs(22);
const REQUEST: Duration = Duration::from_secs(120);
const MAX_TASKS: usize = 12;

pub(crate) struct Files {
    local: Mutex<Option<(String, Arc<Root>)>>,
    jobs: Mutex<BTreeMap<u64, Arc<Job>>>,
    gate: Arc<Semaphore>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            local: Mutex::new(None),
            jobs: Mutex::new(BTreeMap::new()),
            gate: Arc::new(Semaphore::new(1)),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    Upload,
    Download,
}

impl Direction {
    fn permissions(self) -> Capabilities {
        match self {
            Self::Upload => Capabilities::FILE_WRITE,
            Self::Download => Capabilities::FILE_READ,
        }
    }

    fn is_download(self) -> bool {
        matches!(self, Self::Download)
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferSpec {
    direction: Direction,
    source: RelativePath,
    destination: RelativePath,
    items: Vec<String>,
    conflict: Conflict,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Control {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Queued,
    Scanning,
    Transferring,
    Paused,
    Reconnecting,
    Complete,
    Failed,
    Canceled,
}

impl Phase {
    fn terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Canceled)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Progress {
    id: u64,
    direction: Direction,
    label: String,
    phase: Phase,
    error: Option<FileError>,
    bytes: u64,
    size: u64,
    completed: usize,
    items: usize,
}

struct Job {
    progress: Mutex<Progress>,
    action: AtomicU8,
    spec: TransferSpec,
    root: Arc<Root>,
}

impl Job {
    fn update(&self, apply: impl FnOnce(&mut Progress)) {
        if let Ok(mut state) = self.progress.lock() {
            apply(&mut state);
        }
    }

    fn phase(&self, phase: Phase) {
        self.update(|state| state.phase = phase);
    }

    fn canceled(&self, context: &Context) -> bool {
        self.action.load(Relaxed) == 2 || context.ended.load(Relaxed)
    }

    async fn ready(&self, context: &Context, phase: Phase) -> Result<(), FileError> {
        while self.action.load(Relaxed) == 1 && !self.canceled(context) {
            self.phase(Phase::Paused);
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        if self.canceled(context) {
            return Err(FileError::Canceled);
        }
        self.phase(phase);
        Ok(())
    }
}

impl Files {
    pub fn retry(&self, context: Arc<Context>, id: u64) -> Result<u64, FileError> {
        let job = self
            .jobs
            .lock()
            .map_err(|_| FileError::Io)?
            .get(&id)
            .cloned()
            .ok_or(FileError::Missing)?;
        let phase = job.progress.lock().map_err(|_| FileError::Io)?.phase;
        if !matches!(phase, Phase::Failed | Phase::Canceled) {
            return Err(FileError::Denied);
        }
        if !Arc::ptr_eq(&job.root, &self.root()?) {
            return Err(FileError::Changed);
        }
        self.start(context, job.spec.clone())
    }

    pub async fn set_root(&self, path: String) -> Result<(), FileError> {
        if path.is_empty() || path.len() > 2048 {
            return Err(FileError::Invalid);
        }
        if self.busy() {
            return Err(FileError::Denied);
        }
        let selected = PathBuf::from(&path);
        let root = disk(move || Root::open(&selected)).await?;
        *self.local.lock().map_err(|_| FileError::Io)? = Some((path, Arc::new(root)));
        Ok(())
    }

    pub fn root_label(&self) -> Option<String> {
        self.local
            .lock()
            .ok()?
            .as_ref()
            .map(|(label, _)| label.clone())
    }

    fn root(&self) -> Result<Arc<Root>, FileError> {
        self.local
            .lock()
            .map_err(|_| FileError::Io)?
            .as_ref()
            .map(|(_, root)| root.clone())
            .ok_or(FileError::Denied)
    }

    pub async fn list(
        &self,
        context: &Context,
        remote: bool,
        path: RelativePath,
        after: Option<String>,
    ) -> Result<Page, FileError> {
        if remote {
            let link = context
                .link()
                .filter(|link| link.capabilities.any_files())
                .ok_or(FileError::Denied)?;
            let pipe = link.files.ok_or(FileError::Disconnected)?;
            page(context, &pipe, path, after)
                .await
                .map_err(TransferError::file)
        } else {
            let root = self.root()?;
            disk(move || root.page(&path, after.as_deref())).await
        }
    }

    pub fn snapshot(&self) -> Vec<Progress> {
        self.jobs
            .lock()
            .map(|jobs| {
                jobs.values()
                    .filter_map(|job| job.progress.lock().ok().map(|view| view.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn busy(&self) -> bool {
        self.snapshot().into_iter().any(|job| !job.phase.terminal())
    }

    pub fn cancel_all(&self) {
        if let Ok(jobs) = self.jobs.lock() {
            for job in jobs.values() {
                job.action.store(2, Relaxed);
            }
        }
    }

    pub fn control(&self, id: u64, control: Control) -> Result<(), FileError> {
        let jobs = self.jobs.lock().map_err(|_| FileError::Io)?;
        let job = jobs.get(&id).ok_or(FileError::Missing)?;
        if job.action.load(Relaxed) != 2 {
            job.action.store(
                match control {
                    Control::Pause => 1,
                    Control::Resume => 0,
                    Control::Cancel => 2,
                },
                Relaxed,
            );
        }
        Ok(())
    }

    pub fn start(&self, context: Arc<Context>, spec: TransferSpec) -> Result<u64, FileError> {
        spec.source.validate()?;
        spec.destination.validate()?;
        let first = spec.items.first().ok_or(FileError::Invalid)?;
        if spec.items.len() > 128
            || spec.items.iter().collect::<BTreeSet<_>>().len() != spec.items.len()
        {
            return Err(FileError::Invalid);
        }
        for item in &spec.items {
            spec.source.child(item.clone())?;
        }
        if !context
            .link()
            .is_some_and(|link| link.capabilities.includes(spec.direction.permissions()))
        {
            return Err(FileError::Denied);
        }
        let root = self.root()?;
        let mut jobs = self.jobs.lock().map_err(|_| FileError::Io)?;
        if jobs.len() >= MAX_TASKS {
            let oldest = jobs
                .iter()
                .find(|(_, job)| {
                    job.progress
                        .lock()
                        .is_ok_and(|status| status.phase.terminal())
                })
                .map(|(id, _)| *id)
                .ok_or(FileError::Limit)?;
            jobs.remove(&oldest);
        }
        let id = context.next();
        let job = Arc::new(Job {
            progress: Mutex::new(Progress {
                id,
                direction: spec.direction,
                label: first.clone(),
                phase: Phase::Queued,
                error: None,
                bytes: 0,
                size: 0,
                completed: 0,
                items: 0,
            }),
            action: AtomicU8::new(0),
            spec: spec.clone(),
            root: root.clone(),
        });
        jobs.insert(id, job.clone());
        let gate = self.gate.clone();
        tokio::spawn(async move {
            let queued = gate.acquire_owned().await;
            let result = if let Ok(_slot) = queued {
                Work {
                    context,
                    job: job.clone(),
                    root,
                    spec,
                }
                .run()
                .await
            } else {
                Err(FileError::Canceled)
            };
            job.update(|state| {
                state.phase = match result {
                    Ok(()) => Phase::Complete,
                    Err(FileError::Canceled) => Phase::Canceled,
                    Err(_) => Phase::Failed,
                };
                state.error = result.err();
            });
        });
        Ok(id)
    }
}

struct Work {
    context: Arc<Context>,
    job: Arc<Job>,
    root: Arc<Root>,
    spec: TransferSpec,
}

struct Item {
    source: RelativePath,
    entry: Entry,
    parent: Option<usize>,
}

impl Work {
    async fn run(&self) -> Result<(), FileError> {
        self.job.ready(&self.context, Phase::Scanning).await?;
        let plan = self.plan().await?;
        self.job.update(|status| {
            status.items = plan.len();
            status.size = plan
                .iter()
                .filter(|item| item.entry.kind == EntryKind::File)
                .map(|item| item.entry.size)
                .fold(0u64, u64::saturating_add);
        });
        let mut destinations: Vec<Option<RelativePath>> = Vec::with_capacity(plan.len());
        let mut completed_bytes = 0u64;
        for item in plan {
            self.job.ready(&self.context, Phase::Transferring).await?;
            let directory = match item.parent {
                Some(parent) => destinations.get(parent).and_then(Option::as_ref),
                None => Some(&self.spec.destination),
            };
            let destination = if let Some(directory) = directory {
                let path = directory.child(item.entry.name)?;
                if item.entry.kind == EntryKind::Directory {
                    let chosen = self.directory(path).await?;
                    chosen.map(|name| directory.child(name)).transpose()?
                } else {
                    let bytes = match self.spec.direction {
                        Direction::Upload => {
                            self.upload(item.source, path, completed_bytes).await?
                        }
                        Direction::Download => {
                            self.download(item.source, path, completed_bytes).await?
                        }
                    };
                    completed_bytes = completed_bytes.saturating_add(bytes);
                    None
                }
            } else {
                None
            };
            destinations.push(destination);
            self.job.update(|status| {
                status.completed += 1;
                status.bytes = completed_bytes;
            });
        }
        Ok(())
    }

    async fn plan(&self) -> Result<Vec<Item>, FileError> {
        let root = self.children(&self.spec.source).await?;
        let selected: BTreeSet<_> = self.spec.items.iter().collect();
        let mut stack: Vec<_> = root
            .into_iter()
            .filter(|entry| selected.contains(&entry.name))
            .rev()
            .map(|entry| (self.spec.source.clone(), entry, None))
            .collect();
        if stack.len() != selected.len() {
            return Err(FileError::Changed);
        }
        let mut plan = Vec::new();
        while let Some((path, entry, parent)) = stack.pop() {
            self.job.ready(&self.context, Phase::Scanning).await?;
            if plan.len() + stack.len() >= MAX_ITEMS {
                return Err(FileError::Limit);
            }
            let source = path.child(entry.name.clone())?;
            if entry.kind == EntryKind::Directory {
                let children = self.children(&source).await?;
                if children.len() + stack.len() + plan.len() >= MAX_ITEMS {
                    return Err(FileError::Limit);
                }
                stack.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|entry| (source.clone(), entry, Some(plan.len()))),
                );
            }
            plan.push(Item {
                source,
                entry,
                parent,
            });
        }
        Ok(plan)
    }

    async fn children(&self, path: &RelativePath) -> Result<Vec<Entry>, FileError> {
        let mut after = None;
        let mut entries = Vec::new();
        loop {
            self.job.ready(&self.context, Phase::Scanning).await?;
            let next = if self.spec.direction.is_download() {
                let request = FileRequest::List {
                    path: path.clone(),
                    after: after.clone(),
                };
                let (FileReply::Page { page }, _) = self.retry_call(&request).await? else {
                    return Err(FileError::Io);
                };
                page
            } else {
                let root = self.root.clone();
                let path = path.clone();
                let cursor = after.clone();
                disk(move || root.page(&path, cursor.as_deref())).await?
            };
            entries.extend(next.entries);
            if entries.len() > MAX_ITEMS {
                return Err(FileError::Limit);
            }
            match next.next {
                None => return Ok(entries),
                Some(cursor) if after.as_ref().is_none_or(|prior| &cursor > prior) => {
                    after = Some(cursor)
                }
                _ => return Err(FileError::Invalid),
            }
        }
    }

    async fn directory(&self, path: RelativePath) -> Result<Option<String>, FileError> {
        let conflict = self.spec.conflict;
        if self.spec.direction.is_download() {
            let root = self.root.clone();
            disk(move || root.create_directory(&path, conflict)).await
        } else {
            let (FileReply::Directory { name }, _) = self
                .retry_call(&FileRequest::Directory {
                    id: self.context.next(),
                    path,
                    conflict,
                })
                .await?
            else {
                return Err(FileError::Io);
            };
            Ok(name)
        }
    }

    async fn upload(
        &self,
        path: RelativePath,
        destination: RelativePath,
        base: u64,
    ) -> Result<u64, FileError> {
        let root = self.root.clone();
        let context = self.context.clone();
        let job = self.job.clone();
        let mut source = Some(
            disk(move || Outgoing::open(&root, &path, None, || job.canceled(&context))).await?,
        );
        let stamp = source.as_ref().ok_or(FileError::Io)?.stamp().clone();
        let id = self.context.next();
        let request = FileRequest::WriteOpen {
            id,
            path: destination,
            stamp: stamp.clone(),
            conflict: self.spec.conflict,
        };
        let mut result = self
            .upload_segments(id, &request, &stamp, &mut source, base)
            .await;
        if result.is_err() {
            if let Some(pipe) = self.context.link().and_then(|link| link.files) {
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    call(
                        &self.context,
                        &pipe,
                        &FileRequest::Cancel { id },
                        Vec::new(),
                    ),
                )
                .await;
            }
            if self.job.canceled(&self.context) {
                result = Err(FileError::Canceled);
            }
        }
        result
    }

    async fn upload_segments(
        &self,
        id: u64,
        request: &FileRequest,
        stamp: &Stamp,
        source: &mut Option<Outgoing>,
        base: u64,
    ) -> Result<u64, FileError> {
        loop {
            let pipe = self.pipe().await?;
            let result = self
                .upload_on(&pipe, id, request, stamp, source, base)
                .await;
            match result {
                Err(TransferError::Lost) => self.job.phase(Phase::Reconnecting),
                reply => return reply.map_err(TransferError::file),
            }
        }
    }

    async fn upload_on(
        &self,
        pipe: &Rpc,
        id: u64,
        request: &FileRequest,
        stamp: &Stamp,
        source: &mut Option<Outgoing>,
        base: u64,
    ) -> Result<u64, TransferError> {
        let (FileReply::Destination { status }, _) =
            call(&self.context, pipe, request, Vec::new()).await?
        else {
            return Err(FileError::Invalid.into());
        };
        let mut offset = match status {
            Opened::Complete { name } => return Ok(if name.is_some() { stamp.size } else { 0 }),
            Opened::Writing { offset } if offset <= stamp.size => offset,
            _ => return Err(FileError::Changed.into()),
        };
        while offset < stamp.size {
            self.job.ready(&self.context, Phase::Transferring).await?;
            let mut reader = source.take().ok_or(FileError::Io)?;
            let (reader, block) = disk(move || {
                let block = reader.block(offset)?;
                Ok((reader, block))
            })
            .await?;
            *source = Some(reader);
            let next = offset + block.len() as u64;
            let (FileReply::Written { offset: accepted }, _) = call(
                &self.context,
                pipe,
                &FileRequest::WriteBlock { id, offset },
                block,
            )
            .await?
            else {
                return Err(FileError::Invalid.into());
            };
            if next != accepted {
                return Err(FileError::Changed.into());
            }
            offset = accepted;
            self.progress_bytes(base + offset);
        }
        self.job.ready(&self.context, Phase::Transferring).await?;
        let (FileReply::Complete { name }, _) =
            call(&self.context, pipe, &FileRequest::Commit { id }, Vec::new()).await?
        else {
            return Err(FileError::Invalid.into());
        };
        Ok(if name.is_some() { stamp.size } else { 0 })
    }

    async fn download(
        &self,
        path: RelativePath,
        destination: RelativePath,
        base: u64,
    ) -> Result<u64, FileError> {
        let mut expected = None;
        let mut incoming: Option<Incoming> = None;
        loop {
            let pipe = self.pipe().await?;
            let result = self
                .download_on(
                    &pipe,
                    &path,
                    &destination,
                    base,
                    &mut expected,
                    &mut incoming,
                )
                .await;
            match result {
                Err(TransferError::Lost) => self.job.phase(Phase::Reconnecting),
                Err(error) => return Err(error.file()),
                Ok(size) => return Ok(size),
            }
        }
    }

    async fn download_on(
        &self,
        pipe: &Rpc,
        path: &RelativePath,
        destination: &RelativePath,
        base: u64,
        expected: &mut Option<Stamp>,
        incoming: &mut Option<Incoming>,
    ) -> Result<u64, TransferError> {
        let (FileReply::Source { stamp }, _) = call(
            &self.context,
            pipe,
            &FileRequest::ReadOpen {
                path: path.clone(),
                expected: expected.clone(),
            },
            Vec::new(),
        )
        .await?
        else {
            return Err(FileError::Invalid.into());
        };
        if expected.as_ref().is_some_and(|old| old != &stamp) {
            return Err(FileError::Changed.into());
        }
        *expected = Some(stamp.clone());
        if incoming.is_none() {
            let root = self.root.clone();
            let path = destination.clone();
            let spec = stamp.clone();
            let conflict = self.spec.conflict;
            *incoming = disk(move || Incoming::open(&root, &path, spec, conflict)).await?;
        }
        let Some(writer) = incoming.as_ref() else {
            return Ok(0);
        };
        let mut offset = writer.offset();
        while offset < stamp.size {
            self.job.ready(&self.context, Phase::Transferring).await?;
            let (FileReply::Block { offset: server }, block) = call(
                &self.context,
                pipe,
                &FileRequest::ReadBlock { offset },
                Vec::new(),
            )
            .await?
            else {
                return Err(FileError::Invalid.into());
            };
            if offset != server || block.is_empty() {
                return Err(FileError::Changed.into());
            }
            let mut writer = incoming.take().ok_or(FileError::Io)?;
            let (writer, written) = disk(move || {
                let written = writer.append(offset, &block)?;
                Ok((writer, written))
            })
            .await?;
            *incoming = Some(writer);
            offset = written;
            self.progress_bytes(base + offset);
        }
        self.job.ready(&self.context, Phase::Transferring).await?;
        let context = self.context.clone();
        let job = self.job.clone();
        let writer = incoming.take().ok_or(FileError::Io)?;
        let name = disk(move || writer.commit(|| job.canceled(&context))).await?;
        Ok(if name.is_some() { stamp.size } else { 0 })
    }

    fn progress_bytes(&self, bytes: u64) {
        self.job.update(|status| status.bytes = bytes);
    }

    async fn retry_call(&self, request: &FileRequest) -> Result<(FileReply, Vec<u8>), FileError> {
        loop {
            let pipe = self.pipe().await?;
            match call(&self.context, &pipe, request, Vec::new()).await {
                Err(TransferError::Lost) => self.job.phase(Phase::Reconnecting),
                result => return result.map_err(TransferError::file),
            }
        }
    }

    async fn pipe(&self) -> Result<Rpc, FileError> {
        let mut link = self.context.watch_link();
        tokio::time::timeout(RECONNECT, async {
            loop {
                if self.job.canceled(&self.context) {
                    return Err(FileError::Canceled);
                }
                let current = link.borrow_and_update().clone();
                if let Some(link) = current {
                    if !link
                        .capabilities
                        .includes(self.spec.direction.permissions())
                    {
                        return Err(FileError::Denied);
                    }
                    if let Some(files) = link.files.filter(|pipe| !pipe.closed()) {
                        return Ok(files);
                    }
                }
                self.job.phase(Phase::Reconnecting);
                tokio::select! {
                    _ = link.changed() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        })
        .await
        .unwrap_or(Err(FileError::Disconnected))
    }
}

#[derive(Debug)]
enum TransferError {
    Lost,
    File(FileError),
}

impl TransferError {
    fn file(self) -> FileError {
        match self {
            Self::Lost => FileError::Disconnected,
            Self::File(error) => error,
        }
    }
}

impl From<FileError> for TransferError {
    fn from(value: FileError) -> Self {
        Self::File(value)
    }
}

async fn page(
    context: &Context,
    pipe: &Rpc,
    path: RelativePath,
    after: Option<String>,
) -> Result<Page, TransferError> {
    let (FileReply::Page { page }, _) = call(
        context,
        pipe,
        &FileRequest::List { path, after },
        Vec::new(),
    )
    .await?
    else {
        return Err(FileError::Invalid.into());
    };
    Ok(page)
}

async fn call(
    context: &Context,
    pipe: &Rpc,
    request: &FileRequest,
    data: Vec<u8>,
) -> Result<(FileReply, Vec<u8>), TransferError> {
    let (result, block): (Result<FileReply, FileError>, _) = pipe
        .call(context.next(), data, request, REQUEST)
        .await
        .map_err(|error| match error {
            CallError::Lost => TransferError::Lost,
            CallError::Invalid => FileError::Invalid.into(),
        })?;
    let reply = result?;
    if block.len() > BLOCK || (!matches!(reply, FileReply::Block { .. }) && !block.is_empty()) {
        return Err(FileError::Invalid.into());
    }
    Ok((reply, block))
}

async fn disk<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, FileError> + Send + 'static,
) -> Result<T, FileError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| FileError::Io)?
}
