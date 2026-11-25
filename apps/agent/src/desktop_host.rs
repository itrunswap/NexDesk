use std::{
    io::{self, BufRead, Write},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64};
use nexdesk_directory::RemoteIdentity;
use nexdesk_files::{Root, TransferStore};
use nexdesk_media::{CaptureKind, CaptureTarget, capture_sources};
use nexdesk_session::{
    collaboration::Capabilities,
    control::Grant,
    controller::ControllerIdentity,
    pairing::{AccessCode, HostPairing},
};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::{
    receivers::{ManagedReceivers, ReceiverSettings},
    server::Host,
};

const CONSENT_TIME: Duration = Duration::from_secs(24);
const RESUME_TIME: Duration = Duration::from_secs(20);

pub(crate) struct DesktopHost {
    capture: watch::Sender<CaptureTarget>,
    share: watch::Sender<Option<Arc<Root>>>,
    permissions: watch::Sender<(bool, Capabilities)>,
    persistent: bool,
    unattended: AtomicBool,
    trust_changes: Arc<Mutex<()>>,
    slots: Arc<Semaphore>,
    next_request: AtomicU64,
    local: Mutex<LocalState>,
    revoked: [watch::Sender<u64>; 2],
    #[cfg(debug_assertions)]
    fixture_drop: watch::Sender<u64>,
}

#[derive(Default)]
struct LocalState {
    pending: Option<PendingApproval>,
    resume: Option<Resume>,
}

struct Resume {
    id: u64,
    digest: [u8; 32],
    ip: IpAddr,
    access: Access,
    expires: Option<Instant>,
}

#[derive(Clone)]
pub(crate) struct Access {
    pub input: Grant,
    pub capabilities: Capabilities,
    pub files: Option<Arc<Mutex<TransferStore>>>,
    remote: Option<RemoteIdentity>,
    unattended: bool,
}

fn access(
    input: Grant,
    capabilities: Capabilities,
    remote: Option<RemoteIdentity>,
    unattended: bool,
) -> Access {
    let capabilities = if input == Grant::Control {
        capabilities
    } else {
        capabilities.without(Capabilities::TEXT)
    };
    Access {
        input,
        capabilities,
        files: capabilities
            .any_files()
            .then(|| Arc::new(Mutex::new(TransferStore::default()))),
        remote,
        unattended,
    }
}

pub(crate) struct Incoming {
    pub ip: IpAddr,
    pub request_input: bool,
    pub can_input: bool,
    pub capabilities: Capabilities,
    pub remote: Option<RemoteIdentity>,
    pub controller: Option<ControllerIdentity>,
}

struct Approval {
    decision: Decision,
    capabilities: Capabilities,
    remember_controller: bool,
}

struct PendingApproval {
    id: u64,
    directory: bool,
    send: oneshot::Sender<Approval>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Deny,
    View,
    Control,
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum LocalCommand {
    Decide {
        request: u64,
        decision: Decision,
        #[serde(default)]
        capabilities: Capabilities,
        #[serde(default)]
        remember_controller: bool,
    },
    Revoke,
    Rotate {
        #[serde(default)]
        directory: bool,
        #[serde(default)]
        request: Option<u64>,
        #[serde(default)]
        password: Option<String>,
    },
    SetSource {
        source: String,
    },
    Configure {
        source: String,
        path: String,
    },
    ApplySettings {
        config: ReceiverSettings,
    },
    RevokeController {
        request: u64,
        scope: String,
        identity: String,
    },
    Stop,
    #[cfg(debug_assertions)]
    FixtureDropTransport,
}

impl DesktopHost {
    pub(crate) fn new(
        capture: CaptureTarget,
        persistent: bool,
        share: Option<Arc<Root>>,
    ) -> Arc<Self> {
        let permissions = nexdesk_store::DesktopPermissions::default();
        Arc::new(Self {
            capture: watch::channel(capture).0,
            share: watch::channel(share).0,
            permissions: watch::channel((
                permissions.allow_control,
                permissions.allowed_capabilities,
            ))
            .0,
            persistent,
            unattended: AtomicBool::new(false),
            trust_changes: Arc::new(Mutex::new(())),
            slots: Arc::new(Semaphore::new(1)),
            next_request: AtomicU64::new(1),
            local: Mutex::new(LocalState::default()),
            revoked: std::array::from_fn(|_| watch::channel(0).0),
            #[cfg(debug_assertions)]
            fixture_drop: watch::channel(0).0,
        })
    }

    pub(crate) fn ready(
        &self,
        address: Option<SocketAddr>,
        pairing: &HostPairing,
        code: &AccessCode,
        directory: Option<&(AccessCode, HostPairing)>,
        shell: bool,
    ) {
        let mut event = json!({"event":"ready", "managed":address.is_none(),
            "identity":pairing.identity().to_string(), "code":code_text(code)});
        if let Some(address) = address {
            event["address"] = json!(address.ip().to_string());
            event["port"] = json!(address.port());
        }
        if let Some((code, pairing)) = directory {
            event["directory_code"] = json!(code_text(code));
            event["directory_identity"] = json!(pairing.identity().to_string());
            event["shell_allowed"] = json!(shell);
        }
        report(event);
    }

    pub(crate) fn reserve(&self) -> Result<OwnedSemaphorePermit, StatusCode> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)
    }

    pub(crate) fn revocation(&self, directory: bool) -> watch::Receiver<u64> {
        self.revoked[usize::from(directory)].subscribe()
    }

    pub(crate) fn capture(&self) -> CaptureTarget {
        *self.capture.borrow()
    }

    pub(crate) fn share(&self) -> Option<Arc<Root>> {
        self.share.borrow().clone()
    }

    pub(crate) fn allows_control(&self) -> bool {
        self.permissions.borrow().0
    }

    pub(crate) async fn fixture_interruption(&self) {
        #[cfg(debug_assertions)]
        if !self.persistent {
            let mut receiver = self.fixture_drop.subscribe();
            let _ = receiver.changed().await;
            return;
        }
        std::future::pending::<()>().await;
    }

    pub(crate) async fn authorize(
        &self,
        incoming: Incoming,
        token: Option<&str>,
    ) -> Result<(u64, Access), StatusCode> {
        let Incoming {
            ip,
            request_input,
            can_input,
            capabilities,
            remote,
            controller,
        } = incoming;
        let (allow_control, allowed) = *self.permissions.borrow();
        let can_input = can_input && allow_control;
        let capabilities = capabilities.intersection(allowed.with(Capabilities::SHELL));
        let capabilities = if can_input {
            capabilities
        } else {
            capabilities.without(Capabilities::TEXT)
        };
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        if let Some(token) = token {
            let mut local = self
                .local
                .lock()
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            let Some(resume) = &local.resume else {
                return Err(StatusCode::GONE);
            };
            if token.len() != 43
                || (remote.is_none() && resume.ip != ip)
                || resume.access.remote != remote
                || (resume.access.unattended && !self.unattended.load(Ordering::Acquire))
                || resume.digest != token_digest(token)
                || resume
                    .expires
                    .is_none_or(|deadline| deadline <= Instant::now())
            {
                return Err(StatusCode::GONE);
            }
            let mut access = resume.access.clone();
            let previous = access.capabilities;
            if !request_input || !can_input {
                access.input = Grant::View;
            }
            access.capabilities = access
                .capabilities
                .intersection(capabilities)
                .without(Capabilities::SHELL);
            if access.input != Grant::Control {
                access.capabilities = access.capabilities.without(Capabilities::TEXT);
            }
            if !access.capabilities.any_files() {
                access.files = None;
            } else if previous.without(access.capabilities).any_files() {
                access.files = Some(Arc::new(Mutex::new(TransferStore::default())));
            }
            local.resume = None;
            return Ok((id, access));
        }
        let allowed = self.persistent && self.unattended.load(Ordering::Acquire);
        let trust_epoch = self.revocation(remote.is_some());
        let scope = remote
            .as_ref()
            .map_or("lan", |identity| identity.origin.as_str());
        if allowed && let Some(identity) = controller.clone() {
            let mode = scope.to_owned();
            let stored = tokio::task::spawn_blocking(move || {
                nexdesk_store::Store::open()
                    .and_then(|store| store.controller_grant(&mode, &identity))
            })
            .await;
            if let Ok(Ok(Some((control, permitted)))) = stored
                && (!request_input || !can_input || control)
                && permitted.includes(capabilities)
            {
                let input = if request_input && can_input {
                    Grant::Control
                } else {
                    Grant::View
                };
                return Ok((id, access(input, capabilities, remote, true)));
            }
        }
        let (send, receive) = oneshot::channel();
        self.local
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .pending = Some(PendingApproval {
            id,
            directory: remote.is_some(),
            send,
        });
        let _prompt = Prompt { owner: self, id };
        let displays = matches!(
            self.capture(),
            CaptureTarget::Source {
                kind: CaptureKind::Screen,
                ..
            }
        );
        let mut event = json!({"event":"request", "request":id, "peer":ip.to_string(),
            "request_input":request_input, "can_input":can_input, "displays":displays, "capabilities":capabilities,
            "mode": if remote.is_some() {"id"} else {"lan"}});
        if allowed && let Some(identity) = &controller {
            event["controller"] = json!(identity.encoded());
        }
        report(event);
        let Ok(Ok(approval)) = tokio::time::timeout(CONSENT_TIME, receive).await else {
            return Err(StatusCode::FORBIDDEN);
        };
        let input = match approval.decision {
            Decision::Deny => return Err(StatusCode::FORBIDDEN),
            Decision::Control if request_input && can_input => Grant::Control,
            Decision::Control | Decision::View => Grant::View,
        };
        let mut accepted = approval.capabilities.intersection(capabilities);
        if input != Grant::Control {
            accepted = accepted.without(Capabilities::TEXT);
        }
        if allowed
            && approval.remember_controller
            && let Some(identity) = controller
        {
            let mode = scope.to_owned();
            let changes = self.trust_changes.clone();
            let saved = tokio::task::spawn_blocking(move || {
                let _guard = changes
                    .lock()
                    .map_err(|_| nexdesk_store::StoreError::Invalid)?;
                if trust_epoch.has_changed().unwrap_or(true) {
                    return Ok(());
                }
                let store = nexdesk_store::Store::open()?;
                store.trust_controller(
                    &mode,
                    &identity,
                    input == Grant::Control,
                    accepted.without(Capabilities::SHELL),
                )
            })
            .await;
            if !matches!(saved, Ok(Ok(()))) {
                report(json!({"event":"issue", "code":"trust_store"}));
            }
        }
        Ok((id, access(input, accepted, remote, false)))
    }

    pub(crate) fn begin(
        &self,
        id: u64,
        ip: IpAddr,
        access: Access,
        epoch: u64,
    ) -> Result<Zeroizing<String>, StatusCode> {
        let mut local = self
            .local
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let (allow_control, allowed) = *self.permissions.borrow();
        if *self.revoked[usize::from(access.remote.is_some())].borrow() != epoch
            || (access.unattended && !self.unattended.load(Ordering::Acquire))
            || (!allow_control && access.input == Grant::Control)
            || !allowed
                .with(Capabilities::SHELL)
                .includes(access.capabilities)
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let mut token = Zeroizing::new([0; 32]);
        SystemRandom::new()
            .fill(&mut *token)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let token = Zeroizing::new(BASE64.encode(token.as_slice()));
        let grant = access.input;
        let capabilities = access.capabilities;
        let mode = if access.remote.is_some() { "id" } else { "lan" };
        let unattended = access.unattended;
        local.resume = Some(Resume {
            id,
            digest: token_digest(&token),
            ip,
            access,
            expires: None,
        });
        report(
            json!({"event":"active", "request":id, "peer":ip.to_string(), "grant":grant,
                "capabilities":capabilities, "mode":mode, "unattended":unattended}),
        );
        self.record(mode, "connected");
        Ok(token)
    }

    pub(crate) fn end(self: &Arc<Self>, id: u64, resumable: bool, directory: bool) {
        if let Ok(mut local) = self.local.lock()
            && local.resume.as_ref().is_some_and(|resume| resume.id == id)
        {
            if resumable {
                if let Some(resume) = &mut local.resume {
                    resume.expires = Some(Instant::now() + RESUME_TIME);
                }
                let owner = Arc::downgrade(self);
                tokio::spawn(async move {
                    tokio::time::sleep(RESUME_TIME).await;
                    if let Some(owner) = owner.upgrade()
                        && let Ok(mut local) = owner.local.lock()
                        && local
                            .resume
                            .as_ref()
                            .is_some_and(|resume| resume.id == id && resume.expires.is_some())
                    {
                        local.resume = None;
                    }
                });
            } else {
                local.resume = None;
            }
        }
        report(json!({"event":"ended", "request":id}));
        self.record(if directory { "id" } else { "lan" }, "ended");
    }

    fn record(&self, mode: &'static str, outcome: &'static str) {
        if self.persistent {
            tokio::task::spawn_blocking(move || {
                let _ = nexdesk_store::Store::open()
                    .and_then(|store| store.record_session("incoming", mode, outcome));
            });
        }
    }

    pub(crate) fn configure_unattended(&self, enabled: bool) {
        if self
            .unattended
            .swap(enabled && self.persistent, Ordering::AcqRel)
            && !enabled
        {
            self.revoke();
        }
    }

    pub(crate) fn configure_permissions(&self, permissions: nexdesk_store::DesktopPermissions) {
        let next = (permissions.allow_control, permissions.allowed_capabilities);
        let (control, caps) = self.permissions.send_replace(next);
        if (control && !next.0) || !next.1.includes(caps) {
            self.revoke();
        }
    }

    fn revoke(&self) {
        if let Ok(mut local) = self.local.lock() {
            for revoked in &self.revoked {
                revoked.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
            }
            *local = LocalState::default();
        }
    }

    fn revoke_mode(&self, directory: bool) {
        if let Ok(mut local) = self.local.lock() {
            self.revoked[usize::from(directory)]
                .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
            if local
                .pending
                .as_ref()
                .is_some_and(|pending| pending.directory == directory)
            {
                local.pending = None;
            }
            if local
                .resume
                .as_ref()
                .is_some_and(|resume| resume.access.remote.is_some() == directory)
            {
                local.resume = None;
            }
        }
    }

    pub(crate) fn clear_resume(&self, directory: bool) {
        if let Ok(mut local) = self.local.lock()
            && local
                .resume
                .as_ref()
                .is_some_and(|resume| resume.access.remote.is_some() == directory)
        {
            local.resume = None;
        }
    }

    pub(crate) fn configure_shared(
        &self,
        target: Option<CaptureTarget>,
        share: Option<Option<Arc<Root>>>,
    ) {
        let target = target.filter(|target| *target != self.capture());
        if target.is_some() || share.is_some() {
            self.revoke();
        }
        if let Some(target) = target {
            self.capture.send_replace(target);
        }
        if let Some(share) = share {
            self.share.send_replace(share);
        }
    }

    pub(crate) fn listen_local(
        self: Arc<Self>,
        host: Arc<Host>,
        shutdown: watch::Sender<bool>,
        managed: bool,
    ) {
        let (send, mut receive) = mpsc::channel(16);
        let reader = std::thread::Builder::new()
            .name("nexdesk-local-ipc".into())
            .spawn(move || {
                let mut stdin = io::stdin().lock();
                let mut line = Vec::with_capacity(256);
                while read_command(&mut stdin, &mut line) {
                    let command = serde_json::from_slice::<LocalCommand>(&line);
                    zeroize::Zeroize::zeroize(line.as_mut_slice());
                    if let Ok(command) = command
                        && send.blocking_send(command).is_err()
                    {
                        break;
                    }
                    line.clear();
                }
            });
        if reader.is_err() {
            let _ = shutdown.send(true);
            return;
        }
        tokio::spawn(async move {
            let mut receivers = managed.then(ManagedReceivers::default);
            let mut shutdown_in = shutdown.subscribe();
            loop {
                let command = tokio::select! {
                    _ = shutdown_in.changed() => None,
                    command = receive.recv() => command,
                };
                match command {
                    Some(LocalCommand::Decide {
                        request,
                        decision,
                        capabilities,
                        remember_controller,
                    }) => {
                        if let Ok(mut local) = self.local.lock()
                            && local
                                .pending
                                .as_ref()
                                .is_some_and(|pending| pending.id == request)
                            && let Some(pending) = local.pending.take()
                        {
                            let _ = pending.send.send(Approval {
                                decision,
                                capabilities,
                                remember_controller,
                            });
                        }
                    }
                    Some(LocalCommand::Revoke) => self.revoke(),
                    Some(LocalCommand::ApplySettings { config }) => {
                        if let Some(receivers) = &mut receivers {
                            receivers.apply(&self, &host, config).await;
                        }
                    }
                    Some(LocalCommand::RevokeController {
                        request,
                        scope,
                        identity,
                    }) => {
                        let id_mode = scope != "lan";
                        let owner = self.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            let _guard = owner
                                .trust_changes
                                .lock()
                                .map_err(|_| nexdesk_store::StoreError::Invalid)?;
                            let identity = ControllerIdentity::parse(&identity)?;
                            nexdesk_store::Store::open()?.revoke_controller(&scope, &identity)?;
                            owner.revoke_mode(id_mode);
                            Ok::<(), nexdesk_store::StoreError>(())
                        })
                        .await;
                        let ok = matches!(result, Ok(Ok(())));
                        report(json!({"event":"settings_result", "request":request, "ok":ok}));
                    }
                    Some(LocalCommand::Configure { source, path }) => {
                        let selected = tokio::task::spawn_blocking(move || {
                            let target = capture_target(&source)?;
                            let root = if path.is_empty() {
                                None
                            } else {
                                Some(Arc::new(Root::open(&PathBuf::from(path)).ok()?))
                            };
                            Some((target, root))
                        })
                        .await;
                        if let Ok(Some((target, root))) = selected {
                            self.revoke();
                            self.capture.send_replace(target);
                            self.share.send_replace(root);
                            report(json!({"event":"configure_result", "ok":true}));
                        } else {
                            report(json!({"event":"configure_result", "ok":false}));
                        }
                    }
                    Some(LocalCommand::SetSource { source }) => {
                        let selection = source.clone();
                        if let Ok(Some(target)) =
                            tokio::task::spawn_blocking(move || capture_target(&selection)).await
                        {
                            if target != self.capture() {
                                self.capture.send_replace(target);
                                self.revoke();
                            }
                            report(json!({"event":"source_result", "source":source, "ok":true}));
                        } else {
                            report(json!({"event":"source_result", "source":source, "ok":false}));
                        }
                    }
                    #[cfg(debug_assertions)]
                    Some(LocalCommand::FixtureDropTransport) => {
                        if !self.persistent {
                            self.fixture_drop
                                .send_modify(|value| *value = value.wrapping_add(1));
                        }
                    }
                    Some(LocalCommand::Rotate {
                        directory,
                        request,
                        password,
                    }) => {
                        let chosen = password
                            .map(|password| {
                                let password = zeroize::Zeroizing::new(password);
                                AccessCode::parse(password.as_bytes())
                            })
                            .transpose();
                        let Ok(chosen) = chosen else {
                            if let Some(request) = request {
                                report(
                                    json!({"event":"settings_result", "request":request, "error":"invalid_code"}),
                                );
                            }
                            continue;
                        };
                        let owner = host.clone();
                        let persist = self.persistent;
                        match tokio::task::spawn_blocking(move || {
                            owner.rotate(persist, directory, chosen)
                        })
                        .await
                        {
                            Ok(Ok(code)) => {
                                self.revoke_mode(directory);
                                report(
                                    json!({"event":"code", "code":code_text(&code), "directory":directory}),
                                );
                                if let Some(request) = request {
                                    report(
                                        json!({"event":"settings_result", "request":request, "ok":true}),
                                    );
                                }
                            }
                            _ if request.is_some() => report(
                                json!({"event":"settings_result", "request":request, "error":"credential_store"}),
                            ),
                            _ => report(json!({"event":"issue", "code":"credential_store"})),
                        }
                    }
                    Some(LocalCommand::Stop) | None => break,
                }
            }
            self.revoke();
            let _ = shutdown.send(true);
        });
    }
}

struct Prompt<'a> {
    owner: &'a DesktopHost,
    id: u64,
}

impl Drop for Prompt<'_> {
    fn drop(&mut self) {
        if let Ok(mut local) = self.owner.local.lock()
            && local
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == self.id)
        {
            local.pending = None;
        }
        report(json!({"event":"request_closed", "request":self.id}));
    }
}

fn read_command(stdin: &mut impl BufRead, line: &mut Vec<u8>) -> bool {
    loop {
        let Ok(bytes) = stdin.fill_buf() else {
            return false;
        };
        if bytes.is_empty() {
            return false;
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(bytes.len(), |i| i + 1);
        if line.len() + length > 12 * 1024 {
            return false;
        }
        line.extend_from_slice(&bytes[..length]);
        stdin.consume(length);
        if newline.is_some() {
            return true;
        }
    }
}

fn token_digest(token: &str) -> [u8; 32] {
    let mut result = [0; 32];
    result.copy_from_slice(digest::digest(&digest::SHA256, token.as_bytes()).as_ref());
    result
}

pub(crate) fn capture_target(source: &str) -> Option<CaptureTarget> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if source == "picker" {
        return Some(CaptureTarget::SystemPicker);
    }
    let id = source.strip_prefix("screen:")?.parse().ok()?;
    capture_sources(CaptureKind::Screen)
        .ok()?
        .iter()
        .any(|display| display.id == id)
        .then_some(CaptureTarget::Source {
            kind: CaptureKind::Screen,
            id,
        })
}

fn code_text(code: &AccessCode) -> &str {
    std::str::from_utf8(code.as_bytes()).unwrap_or_default()
}

pub(crate) fn report(event: Value) {
    let _ = writeln!(io::stdout().lock(), "{event}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resume_keeps_grant_and_is_single_use_and_revocable() -> Result<(), StatusCode> {
        let host = DesktopHost::new(CaptureTarget::Pattern, false, None);
        let ip = IpAddr::from([127, 0, 0, 1]);
        let caps = Capabilities::TEXT.with(Capabilities::CLIPBOARD);
        let access = |input| Access {
            input,
            capabilities: caps,
            files: None,
            remote: None,
            unattended: false,
        };
        let request = |ip, can_input| Incoming {
            ip,
            request_input: true,
            can_input,
            capabilities: caps,
            remote: None,
            controller: None,
        };
        let token = host.begin(1, ip, access(Grant::Control), 0)?;
        assert_eq!(
            host.authorize(request(ip, true), Some(&token)).await.err(),
            Some(StatusCode::GONE)
        );
        host.end(1, true, false);
        assert_eq!(
            host.authorize(request(IpAddr::from([127, 0, 0, 2]), true), Some(&token))
                .await
                .err(),
            Some(StatusCode::GONE)
        );
        let (_, grant) = host.authorize(request(ip, false), Some(&token)).await?;
        assert_eq!(grant.input, Grant::View);
        assert_eq!(grant.capabilities, Capabilities::CLIPBOARD);
        assert_eq!(
            host.authorize(request(ip, true), Some(&token)).await.err(),
            Some(StatusCode::GONE)
        );

        let token = host.begin(2, ip, access(Grant::View), 0)?;
        host.end(2, true, false);
        host.revoke();
        assert_eq!(
            host.authorize(request(ip, true), Some(&token)).await.err(),
            Some(StatusCode::GONE)
        );
        Ok(())
    }

    #[tokio::test]
    async fn directory_resume_after_network_change_keeps_original_route_scope()
    -> Result<(), StatusCode> {
        let host = DesktopHost::new(CaptureTarget::Pattern, false, None);
        let remote = RemoteIdentity {
            origin: "https://directory.test".into(),
            device: "162534897".into(),
        };
        let token = host.begin(
            1,
            IpAddr::from([192, 168, 0, 10]),
            access(
                Grant::View,
                Capabilities::CLIPBOARD,
                Some(remote.clone()),
                false,
            ),
            0,
        )?;
        host.end(1, true, false);
        let request = |origin: RemoteIdentity| Incoming {
            ip: IpAddr::from([192, 168, 3, 10]),
            request_input: true,
            can_input: true,
            capabilities: Capabilities::CLIPBOARD.with(Capabilities::SHELL),
            remote: Some(origin),
            controller: None,
        };
        let different = RemoteIdentity {
            origin: "https://different.test".into(),
            ..remote.clone()
        };
        assert_eq!(
            host.authorize(request(different), Some(&token)).await.err(),
            Some(StatusCode::GONE)
        );
        let (_, grant) = host.authorize(request(remote), Some(&token)).await?;
        assert_eq!(grant.input, Grant::View);
        assert_eq!(grant.capabilities, Capabilities::CLIPBOARD);
        Ok(())
    }
}
