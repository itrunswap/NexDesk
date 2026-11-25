use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use nexdesk_files::{FileError, RelativePath};
use nexdesk_media::{DesktopPipes, MediaStats, Quality, SoundStatus, View};
use nexdesk_session::{
    collaboration::{Capabilities, Reply, Request},
    pairing::HostIdentity,
};
use nexdesk_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{
    desktop::{viewer_context, write_json},
    file_transfer::{Control, Files, TransferSpec},
    rpc::{CallError, Rpc},
};

const REQUEST_LIMIT: usize = 12 * 1024;
const RESULT_LIMIT: usize = 64;
const OUTPUT_LIMIT: usize = 32 * 1024;

pub(crate) struct Context {
    ids: AtomicU64,
    submit: mpsc::Sender<(u64, Operation)>,
    transport: watch::Sender<Option<Link>>,
    results: Mutex<BTreeMap<u64, Value>>,
    info: Mutex<Info>,
    pub files: Files,
    pub volume: Arc<AtomicU8>,
    pub ended: AtomicBool,
    pub upgrade: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct Link {
    pub capabilities: Capabilities,
    pub files: Option<Rpc>,
    remote: Rpc,
    identity: HostIdentity,
}

#[derive(Default, Serialize)]
struct Info {
    capabilities: Option<Capabilities>,
    quality: Option<Quality>,
    media: Option<MediaStats>,
    frames_per_second: Option<f64>,
    kilobits_per_second: Option<f64>,
    sound: SoundStatus,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Remote {
        request: Request,
        #[serde(default)]
        remember: bool,
    },
    LocalRoot {
        path: String,
    },
    List {
        remote: bool,
        path: RelativePath,
        after: Option<String>,
    },
    Transfer {
        transfer: TransferSpec,
    },
    Task {
        task: u64,
        control: Control,
    },
    Retry {
        task: u64,
    },
    Volume {
        level: u8,
    },
    RequestControl,
}

impl Context {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<(u64, Operation)>) {
        let (submit, commands) = mpsc::channel(24);
        (
            Arc::new(Self {
                ids: AtomicU64::new(1),
                submit,
                transport: watch::channel(None).0,
                results: Mutex::new(BTreeMap::new()),
                info: Mutex::new(Info::default()),
                files: Files::default(),
                volume: Arc::new(AtomicU8::new(0)),
                ended: AtomicBool::new(false),
                upgrade: AtomicBool::new(false),
            }),
            commands,
        )
    }

    pub fn next(&self) -> u64 {
        self.ids.fetch_add(1, Relaxed)
    }

    pub fn link(&self) -> Option<Link> {
        self.transport.borrow().clone()
    }

    pub fn watch_link(&self) -> watch::Receiver<Option<Link>> {
        self.transport.subscribe()
    }

    pub fn start(self: &Arc<Self>, mut commands: mpsc::Receiver<(u64, Operation)>) -> CommandLoop {
        let context = self.clone();
        let task = tokio::spawn(async move {
            while let Some((id, operation)) = commands.recv().await {
                let reply = context.operation(id, operation).await;
                if let Ok(mut completed) = context.results.lock() {
                    if completed.len() >= RESULT_LIMIT {
                        completed.pop_first();
                    }
                    completed.insert(id, reply);
                }
            }
        });
        CommandLoop {
            context: self.clone(),
            task,
        }
    }

    pub fn attach(
        self: &Arc<Self>,
        mut pipes: DesktopPipes,
        capabilities: Option<Capabilities>,
        identity: HostIdentity,
    ) -> (nexdesk_media::ControlPipe, Option<Attachment>) {
        let attachment =
            capabilities
                .zip(pipes.collaboration.take())
                .map(|(capabilities, pipe)| {
                    let (remote, mut tasks) = {
                        let (client, task) = Rpc::new(pipe);
                        (client, vec![task])
                    };
                    let files = pipes.files.take().map(|pipe| {
                        let (client, worker) = Rpc::new(pipe);
                        tasks.push(worker);
                        client
                    });
                    let previous = self.info.lock().ok().and_then(|info| info.quality);
                    if let Some(quality) = previous {
                        let client = remote.clone();
                        let serial = self.next();
                        tasks.push(tokio::spawn(async move {
                            let _ = client
                                .call::<_, Result<Reply, nexdesk_session::collaboration::Problem>>(
                                    serial,
                                    Vec::new(),
                                    &Request::Quality { quality },
                                    Duration::from_secs(5),
                                )
                                .await;
                        }));
                    }
                    self.transport.send_replace(Some(Link {
                        capabilities,
                        files,
                        remote,
                        identity,
                    }));
                    if let Ok(mut info) = self.info.lock() {
                        info.capabilities = Some(capabilities);
                    }
                    Attachment {
                        context: self.clone(),
                        tasks,
                        previous: None,
                    }
                });
        (pipes.control, attachment)
    }

    async fn operation(self: &Arc<Self>, id: u64, operation: Operation) -> Value {
        let reply = match operation {
            Operation::Remote { request, remember } => {
                return self.remote(id, request, remember).await;
            }
            Operation::Volume { level } if level <= 100 => {
                self.volume.store(level, Relaxed);
                Ok(Value::Null)
            }
            Operation::Volume { .. } => Err(FileError::Invalid),
            Operation::LocalRoot { path } => self.files.set_root(path).await.map(|_| Value::Null),
            Operation::List {
                remote,
                path,
                after,
            } => self
                .files
                .list(self, remote, path, after)
                .await
                .map(|page| json!(page)),
            Operation::Transfer { transfer } => self
                .files
                .start(self.clone(), transfer)
                .map(|task| json!({"task":task})),
            Operation::Task { task, control } => {
                self.files.control(task, control).map(|_| Value::Null)
            }
            Operation::Retry { task } => self
                .files
                .retry(self.clone(), task)
                .map(|task| json!({"task":task})),
            Operation::RequestControl => {
                if self.link().is_none() || self.files.busy() {
                    return json!({"error":"busy"});
                }
                self.upgrade.store(true, Relaxed);
                Ok(Value::Null)
            }
        };
        match reply {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error}),
        }
    }

    async fn remote(&self, id: u64, request: Request, remember: bool) -> Value {
        let Some(link) = self.link() else {
            return json!({"error":"disconnected"});
        };
        let response = link
            .remote
            .call::<_, Result<Reply, nexdesk_session::collaboration::Problem>>(
                id,
                Vec::new(),
                &request,
                Duration::from_secs(8),
            )
            .await;
        match response {
            Ok((Ok(reply), body)) if body.is_empty() => {
                match &reply {
                    Reply::Quality { quality } => {
                        if let Ok(mut info) = self.info.lock() {
                            info.quality = Some(*quality);
                        }
                    }
                    Reply::Confirmed { display, mode } if remember => {
                        let (display, mode) = (display.clone(), *mode);
                        let result = tokio::task::spawn_blocking(move || {
                            Store::open()?.confirm_display_mode(link.identity, &display, mode)
                        })
                        .await;
                        if !matches!(result, Ok(Ok(()))) {
                            return json!({"ok":reply, "warning":"storage"});
                        }
                    }
                    Reply::Modes {
                        display, choices, ..
                    } => {
                        let display = display.clone();
                        let remembered = tokio::task::spawn_blocking(move || {
                            Store::open()
                                .ok()?
                                .display_mode(link.identity, &display)
                                .ok()
                                .flatten()
                        })
                        .await
                        .ok()
                        .flatten()
                        .filter(|mode| choices.contains(mode));
                        return json!({"ok":reply, "remembered":remembered});
                    }
                    _ => {}
                }
                json!({"ok":reply})
            }
            Ok((Err(error), body)) if body.is_empty() => json!({"error":error}),
            Err(CallError::Lost) => json!({"error":"disconnected"}),
            _ => json!({"error":"invalid"}),
        }
    }
}

pub(crate) struct CommandLoop {
    context: Arc<Context>,
    task: JoinHandle<()>,
}

impl Drop for CommandLoop {
    fn drop(&mut self) {
        self.context.ended.store(true, Relaxed);
        self.context.files.cancel_all();
        self.task.abort();
    }
}

pub(crate) struct Attachment {
    context: Arc<Context>,
    tasks: Vec<JoinHandle<()>>,
    previous: Option<(Instant, u32, u64)>,
}

impl Attachment {
    pub async fn sample(&mut self, view: &View, sound: SoundStatus) {
        let now = Instant::now();
        let due = self
            .previous
            .is_none_or(|(then, ..)| now.duration_since(then) >= Duration::from_secs(2));
        let sample = if due {
            tokio::time::timeout(Duration::from_millis(180), view.stats())
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        if let Ok(mut info) = self.context.info.lock() {
            info.sound = sound;
            if let Some(stats) = sample {
                if let Some((then, frames, bytes)) = self.previous {
                    let elapsed = now.duration_since(then).as_secs_f64();
                    info.frames_per_second =
                        Some(f64::from(stats.video_frames.saturating_sub(frames)) / elapsed);
                    info.kilobits_per_second =
                        Some(stats.video_bytes.saturating_sub(bytes) as f64 * 0.008 / elapsed);
                }
                self.previous = Some((now, stats.video_frames, stats.video_bytes));
                info.media = Some(stats);
            }
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.context.transport.send_replace(None);
        for task in &self.tasks {
            task.abort();
        }
        if let Ok(mut info) = self.context.info.lock() {
            *info = Info {
                quality: info.quality,
                ..Info::default()
            };
        }
    }
}

/// # Safety
/// The caller provides `length` readable bytes for the whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m3_request(id: u64, input: *const u8, length: usize) -> u64 {
    if input.is_null() || !(1..=REQUEST_LIMIT).contains(&length) {
        return 0;
    }
    let Some(context) = viewer_context(id) else {
        return 0;
    };
    // SAFETY: The caller retains its buffer for the bounded parse.
    let Ok(operation) =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(input, length) })
    else {
        return 0;
    };
    let serial = context.next();
    if context.submit.try_send((serial, operation)).is_err() {
        return 0;
    }
    serial
}

/// # Safety
/// The caller supplies a writable output buffer with the stated capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m3_poll(
    id: u64,
    request: u64,
    output: *mut u8,
    capacity: usize,
) -> usize {
    let Some(context) = viewer_context(id) else {
        return 0;
    };
    let Ok(mut replies) = context.results.lock() else {
        return 0;
    };
    let Some(value) = replies.get(&request) else {
        return 0;
    };
    // SAFETY: The caller owns a disjoint output buffer.
    let bytes = unsafe { write_json(value, output, capacity) };
    if bytes != 0 {
        replies.remove(&request);
    }
    bytes
}

/// # Safety
/// The caller supplies a writable output buffer with the stated capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m3_info(id: u64, output: *mut u8, capacity: usize) -> usize {
    if capacity > OUTPUT_LIMIT {
        return 0;
    }
    let Some(context) = viewer_context(id) else {
        return 0;
    };
    let Ok(info) = context.info.lock() else {
        return 0;
    };
    let fields = json!({
        "session": &*info,
        "tasks": context.files.snapshot(),
        "local_root": context.files.root_label(),
        "volume": context.volume.load(Relaxed),
    });
    // SAFETY: The caller exclusively owns the output buffer for the serialization.
    unsafe { write_json(&fields, output, capacity) }
}
