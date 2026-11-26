//! Desktop ABI. The native viewer owns the connection, credentials and input queue.

use std::{
    net::SocketAddr,
    ptr,
    sync::{Arc, Mutex, atomic::Ordering::Relaxed},
    thread,
    time::Duration,
};

use nexdesk_core::{DeviceId, LanEndpoint};
use nexdesk_directory::{DirectoryClient, ServerOrigin};
use nexdesk_session::{
    ConnectedDesktop, DesktopRequest, SessionError,
    collaboration::Capabilities,
    connect_desktop, connect_routed,
    control::{Action, ActionKind, ControlMessage, DesktopState, EndReason, Grant},
    controller::ControllerIdentity,
    pairing::{AccessCode, HostIdentity, PairingError},
};
use nexdesk_store::{DesktopPreferences, HostPreferences, Store, StoreError, controller_key};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    collaboration::{Context, Operation},
    input_text,
    preview::{self, BUSY, HOST_CHANGED, MEDIA_FAILED, REJECTED, STOPPED, UNREACHABLE, Viewer},
};

const WAITING_CONSENT: u32 = 9;
const DECLINED: u32 = 10;
const RECONNECTING: u32 = 11;
const VERIFY_IDENTITY: u32 = 12;
const REVOKED: u32 = 13;
const CREDENTIALS: u32 = 14;
const UNSUPPORTED: u32 = 15;
const CAPTURE_FAILED: u32 = 16;
const MAX_OUTPUT: usize = 32 * 1024;
const DEVICE_AUTHORIZATION: u32 = 8;
const AUTO_CONFIRM_IDENTITY: u32 = 16;

pub(crate) struct DesktopContext {
    actions: mpsc::Sender<Action>,
    trusted: watch::Sender<bool>,
    release: watch::Sender<u64>,
    info: Mutex<DesktopInfo>,
    collaboration: Option<Arc<Context>>,
}

#[derive(Default, Serialize)]
struct DesktopInfo {
    identity: Option<String>,
    desktop: DesktopState,
    storage_failed: bool,
}

struct ConnectOptions {
    target: Target,
    code: Option<AccessCode>,
    remember: bool,
    control: bool,
    capabilities: Option<Capabilities>,
    device_authorization: bool,
    auto_confirm_identity: bool,
}

enum Target {
    Lan(SocketAddr),
    Directory(ServerOrigin, DeviceId),
}

impl Target {
    fn mode(&self) -> &'static str {
        match self {
            Self::Lan(_) => "lan",
            Self::Directory(_, _) => "id",
        }
    }

    fn pin(&self, store: &Store) -> Result<Option<HostIdentity>, StoreError> {
        match self {
            Self::Lan(address) => store.pin(*address),
            Self::Directory(origin, device) => store.directory_pin(origin, device),
        }
    }

    fn remembered(&self, store: &Store) -> Result<Option<(HostIdentity, AccessCode)>, StoreError> {
        match self {
            Self::Lan(address) => store.remembered(*address),
            Self::Directory(origin, device) => store.directory_code(origin, device),
        }
    }

    fn connected(
        &self,
        store: &Store,
        identity: HostIdentity,
        code: Option<&AccessCode>,
    ) -> Result<(), StoreError> {
        match self {
            Self::Lan(address) => store.connected(*address, identity, code),
            Self::Directory(origin, device) => {
                store.directory_connected(origin, device, identity, code)
            }
        }
    }
}

struct Commands {
    actions: mpsc::Receiver<Action>,
    trusted: watch::Receiver<bool>,
    release: watch::Receiver<u64>,
    cancel: oneshot::Receiver<()>,
    collaboration: Option<mpsc::Receiver<(u64, Operation)>>,
}

/// # Safety
/// Text buffers must be valid for their lengths throughout this call, as in M1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m2_connect(
    address: *const u8,
    address_len: usize,
    port: u16,
    code: *const u8,
    code_len: usize,
    flags: u32,
) -> u64 {
    // SAFETY: Both buffers are borrowed only during argument validation.
    let (Ok(address), Ok(code)) = (unsafe { input_text(address, address_len) }, unsafe {
        input_text(code, code_len)
    }) else {
        return 0;
    };
    connect_viewer(address, port, code, flags, None)
}

/// # Safety
/// Text buffers must be valid for their lengths throughout this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m3_connect(
    address: *const u8,
    address_len: usize,
    port: u16,
    code: *const u8,
    code_len: usize,
    flags: u32,
    capabilities: u32,
) -> u64 {
    // SAFETY: Both buffers are borrowed for this call only.
    let (Ok(address), Ok(code)) = (unsafe { input_text(address, address_len) }, unsafe {
        input_text(code, code_len)
    }) else {
        return 0;
    };
    let Some(capabilities) = Capabilities::from_bits(capabilities) else {
        return 0;
    };
    connect_viewer(address, port, code, flags, Some(capabilities))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryConnect<'a> {
    origin: &'a str,
    device: &'a str,
    code: &'a str,
    flags: u32,
    capabilities: u32,
}

/// # Safety
/// `request` must borrow `length` readable bytes for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m4_connect(request: *const u8, length: usize) -> u64 {
    if request.is_null() || !(1..=2048).contains(&length) {
        return 0;
    }
    // SAFETY: The caller provides the immutable request for the duration of this call.
    let Ok(options) = serde_json::from_slice::<DirectoryConnect>(unsafe {
        std::slice::from_raw_parts(request, length)
    }) else {
        return 0;
    };
    let (Ok(origin), Ok(device), Some(capabilities)) = (
        options.origin.parse(),
        options.device.parse(),
        Capabilities::from_bits(options.capabilities),
    ) else {
        return 0;
    };
    start_viewer(
        Target::Directory(origin, device),
        options.code,
        options.flags,
        Some(capabilities),
    )
}

fn connect_viewer(
    address: &str,
    port: u16,
    code: &str,
    flags: u32,
    capabilities: Option<Capabilities>,
) -> u64 {
    let Ok(address) = LanEndpoint::parse(address, &port.to_string()) else {
        return 0;
    };
    start_viewer(
        Target::Lan(address.socket_addr()),
        code,
        flags,
        capabilities,
    )
}

fn start_viewer(target: Target, code: &str, flags: u32, capabilities: Option<Capabilities>) -> u64 {
    if flags & !(15 | AUTO_CONFIRM_IDENTITY) != 0
        || flags & DEVICE_AUTHORIZATION != 0 && capabilities.is_none()
    {
        return 0;
    }
    let manual = if code.is_empty() && flags & 4 != 0 {
        None
    } else {
        let Ok(code) = AccessCode::parse(code.as_bytes()) else {
            return 0;
        };
        Some(code)
    };
    let options = ConnectOptions {
        target,
        code: manual,
        remember: flags & 2 != 0,
        control: flags & 1 != 0,
        capabilities,
        device_authorization: flags & DEVICE_AUTHORIZATION != 0,
        auto_confirm_identity: flags & AUTO_CONFIRM_IDENTITY != 0,
    };
    let (actions, receive_actions) = mpsc::channel(96);
    let (trusted, trust) = watch::channel(false);
    let (release, release_requests) = watch::channel(0);
    let (cancel, canceled) = oneshot::channel();
    let (collaboration, operations) = match capabilities {
        Some(_) => {
            let (context, commands) = Context::new();
            (Some(context), Some(commands))
        }
        None => (None, None),
    };
    let state = DesktopContext {
        actions,
        trusted,
        release,
        info: Mutex::new(DesktopInfo::default()),
        collaboration,
    };
    let Some((id, viewer)) = Viewer::create(Some(state), cancel) else {
        return 0;
    };
    let commands = Commands {
        actions: receive_actions,
        trusted: trust,
        release: release_requests,
        cancel: canceled,
        collaboration: operations,
    };
    if thread::Builder::new()
        .name("nexdesk-desktop-viewer".into())
        .spawn(move || {
            let mode = options.target.mode();
            let status = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_or(MEDIA_FAILED, |runtime| {
                    runtime.block_on(run(&viewer, options, commands))
                });
            let outcome = match status {
                CREDENTIALS => "credentials",
                UNREACHABLE => "unreachable",
                HOST_CHANGED => "identity",
                BUSY => "busy",
                DECLINED | REJECTED => "declined",
                REVOKED => "revoked",
                STOPPED => "ended",
                _ => "transport",
            };
            let _ = Store::open().and_then(|store| store.record_session("outgoing", mode, outcome));
            viewer.set_status(status);
            viewer.clear();
        })
        .is_err()
    {
        preview::nexdesk_m1_disconnect(id);
        return 0;
    }
    id
}

pub(crate) fn viewer_context(id: u64) -> Option<Arc<Context>> {
    preview::lookup(id)?.desktop.as_ref()?.collaboration.clone()
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m2_trust(id: u64) -> bool {
    if preview::nexdesk_m1_status(id) != VERIFY_IDENTITY {
        return false;
    }
    preview::lookup(id)
        .and_then(|viewer| {
            viewer
                .desktop
                .as_ref()
                .map(|desktop| desktop.trusted.send(true).is_ok())
        })
        .unwrap_or(false)
}

/// # Safety
/// `display` must contain `length` readable UTF-8 bytes, or be null with length zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m2_action(
    id: u64,
    kind: i32,
    generation: u64,
    x: i32,
    y: i32,
    code: u32,
    down: bool,
    display: *const u8,
    length: usize,
) -> bool {
    let Some(viewer) = preview::lookup(id) else {
        return false;
    };
    let Some(desktop) = &viewer.desktop else {
        return false;
    };
    if kind == ActionKind::Release as i32 {
        desktop
            .release
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        return true;
    }
    // SAFETY: No borrowed pointer survives the current call.
    let Ok(display) = (unsafe { input_text(display, length) }) else {
        return false;
    };
    let action = Action {
        kind,
        generation,
        x,
        y,
        code,
        down,
        display: display.to_owned(),
    };
    if !action.valid()
        || !matches!(
            action.kind(),
            ActionKind::Select
                | ActionKind::Move
                | ActionKind::Button
                | ActionKind::Wheel
                | ActionKind::Key
        )
    {
        return false;
    }
    if desktop.actions.try_send(action).is_ok() {
        return true;
    }
    desktop
        .release
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    false
}

/// # Safety
/// `output` must provide `capacity` writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m2_info(id: u64, output: *mut u8, capacity: usize) -> usize {
    let Some(viewer) = preview::lookup(id) else {
        return 0;
    };
    let Some(desktop) = &viewer.desktop else {
        return 0;
    };
    let Ok(info) = desktop.info.lock() else {
        return 0;
    };
    // SAFETY: The output belongs to the caller and is never retained.
    unsafe { write_json(&*info, output, capacity) }
}

async fn run(viewer: &Viewer, options: ConnectOptions, mut commands: Commands) -> u32 {
    let Some(context) = &viewer.desktop else {
        return MEDIA_FAILED;
    };
    let _collaboration = context
        .collaboration
        .as_ref()
        .zip(commands.collaboration.take())
        .map(|(context, commands)| context.start(commands));
    let stored = Store::open();
    let controller = if options.device_authorization {
        match controller_key() {
            Ok(key) => Some(key),
            Err(_) => return CREDENTIALS,
        }
    } else {
        None
    };
    let saved_identity = stored.as_ref().ok().map(|store| options.target.pin(store));
    let confirm_first = options.auto_confirm_identity && matches!(saved_identity, Some(Ok(None)));
    let pin = saved_identity.and_then(Result::ok).flatten();
    let (code, mut pin) = if let Some(code) = options.code {
        (code, pin)
    } else {
        let Some((pin, code)) = stored
            .as_ref()
            .ok()
            .and_then(|store| options.target.remembered(store).ok())
            .flatten()
        else {
            return CREDENTIALS;
        };
        (code, Some(pin))
    };
    let mut resume = None;
    let mut sequence = 0;
    let mut reconnects = 0usize;
    let mut noted = false;
    let mut control = options.control;
    let mut capabilities = options.capabilities;
    loop {
        let next_code = match AccessCode::parse(code.as_bytes()) {
            Ok(code) => code,
            Err(_) => return CREDENTIALS,
        };
        let renewing = resume.is_some();
        let trusted = &mut commands.trusted;
        let connection = async {
            let request = DesktopRequest {
                input: control,
                resume: resume
                    .as_ref()
                    .map(|token: &zeroize::Zeroizing<String>| token.as_str()),
                capabilities,
                controller: controller.as_ref(),
            };
            let confirm = move |identity: HostIdentity| async move {
                if let Ok(mut info) = context.info.lock() {
                    info.identity = Some(identity.to_string());
                }
                if pin.is_some() || confirm_first {
                    return true;
                }
                viewer.set_status(VERIFY_IDENTITY);
                tokio::time::timeout(
                    Duration::from_secs(17),
                    trusted.wait_for(|trusted| *trusted),
                )
                .await
                .is_ok_and(|result| result.is_ok())
            };
            let waiting = || {
                if !renewing {
                    viewer.set_status(WAITING_CONSENT);
                }
            };
            match &options.target {
                Target::Lan(address) => {
                    connect_desktop(*address, next_code, pin, request, confirm, waiting).await
                }
                Target::Directory(origin, device) => {
                    connect_routed(
                        origin.clone(),
                        device.clone(),
                        next_code,
                        pin,
                        request,
                        confirm,
                        waiting,
                    )
                    .await
                }
            }
        };
        let connected = tokio::select! {
            biased;
            _ = &mut commands.cancel => return STOPPED,
            result = connection => result,
        };
        match connected {
            Ok(mut session) => {
                let identity = session.connection.identity;
                if session.capabilities.is_none() {
                    capabilities = None;
                }
                pin = Some(identity);
                resume = Some(session.resume.clone());
                if let Ok(mut info) = context.info.lock() {
                    info.desktop = DesktopState::default();
                }
                while commands.actions.try_recv().is_ok() {}
                let outcome = drive(
                    viewer,
                    context,
                    &mut session,
                    &mut commands,
                    &mut sequence,
                    || {
                        if noted {
                            return;
                        }
                        noted = true;
                        let success = stored.as_ref().is_ok_and(|store| {
                            let _ = store.record_session(
                                "outgoing",
                                options.target.mode(),
                                "connected",
                            );
                            options
                                .target
                                .connected(store, identity, options.remember.then_some(&code))
                                .is_ok()
                        });
                        if !success && let Ok(mut info) = context.info.lock() {
                            info.storage_failed = true;
                        }
                    },
                )
                .await;
                if let Ok(mut info) = context.info.lock() {
                    info.desktop = DesktopState::default();
                }
                viewer.clear();
                match outcome {
                    Outcome::Done(status) => return status,
                    Outcome::Upgrade => {
                        resume = None;
                        control = true;
                        reconnects = 0;
                    }
                    Outcome::Lost => {}
                }
            }
            Err(SessionError::Busy | SessionError::Unreachable | SessionError::Media(_))
                if renewing => {}
            Err(error) => return status(error),
        }
        let Some(backoff) = [650, 1500, 3200, 4600].get(reconnects) else {
            return UNREACHABLE;
        };
        reconnects += 1;
        viewer.set_status(RECONNECTING);
        tokio::select! { _ = &mut commands.cancel => return STOPPED, _ = tokio::time::sleep(Duration::from_millis(*backoff)) => {} }
    }
}

enum Outcome {
    Done(u32),
    Upgrade,
    Lost,
}

async fn drive(
    viewer: &Viewer,
    context: &DesktopContext,
    session: &mut ConnectedDesktop,
    commands: &mut Commands,
    sequence: &mut u64,
    mut on_frame: impl FnMut(),
) -> Outcome {
    let view = &mut session.connection.view;
    let Some(pipes) = view.take_pipes() else {
        return Outcome::Done(MEDIA_FAILED);
    };
    let feature = context.collaboration.as_ref();
    let (mut channel, mut attachment) = match feature {
        Some(context) => context.attach(pipes, session.capabilities, session.connection.identity),
        None => (pipes.control, None),
    };
    let sound = feature
        .filter(|_| {
            session
                .capabilities
                .is_some_and(|grant| grant.includes(Capabilities::AUDIO))
        })
        .and_then(|context| view.listen_sound(context.volume.clone()));
    let mut timer = tokio::time::interval(Duration::from_millis(750));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_frame = tokio::time::Instant::now();
    let mut rendered = false;
    let mut state = DesktopState::default();
    let mut release_pending = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut commands.cancel => {
                if channel.send(&ControlMessage::Action(Action::simple(ActionKind::Close)).pack()) {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                return Outcome::Done(STOPPED);
            }
            _ = commands.release.changed() => {
                while commands.actions.try_recv().is_ok() {}
                release_pending = !channel.send(&ControlMessage::Action(Action::simple(ActionKind::Release)).pack());
            }
            message = channel.recv() => {
                let Some(message) = message else {return Outcome::Lost;};
                match ControlMessage::unpack(&message) {
                    Some(ControlMessage::State(mut next)) => {
                        next.can_input &= session.grant == Grant::Control;
                        if let Ok(mut info) = context.info.lock() { info.desktop = next.clone(); }
                        state = next;
                    }
                    Some(ControlMessage::End(reason)) => return Outcome::Done(match EndReason::try_from(reason) {
                        Ok(EndReason::Revoked) => REVOKED,
                        Ok(EndReason::CaptureUnavailable) => CAPTURE_FAILED,
                        Ok(EndReason::Closed) => STOPPED,
                        _ => MEDIA_FAILED,
                    }),
                    _ => return Outcome::Done(MEDIA_FAILED),
                }
            }
            frame = view.next_frame() => match frame {
                Ok(Some(frame)) => {
                    last_frame = tokio::time::Instant::now();
                    *sequence += 1;
                    if !viewer.publish(*sequence, frame) { return Outcome::Done(MEDIA_FAILED); }
                    if !rendered { rendered = true; on_frame(); }
                }
                Ok(None) => return Outcome::Lost,
                Err(_) => return Outcome::Done(MEDIA_FAILED),
            },
            _ = timer.tick() => {
                if let Some(context) = feature
                    && context.upgrade.swap(false, Relaxed) && session.grant == Grant::View
                {
                    let _ = channel.send(&ControlMessage::Action(Action::simple(ActionKind::Close)).pack());
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    return Outcome::Upgrade;
                }
                if view.has_ended() || last_frame.elapsed() > Duration::from_secs(10) { return Outcome::Lost; }
                if channel.is_open() {
                    if release_pending {
                        release_pending = !channel.send(&ControlMessage::Action(Action::simple(ActionKind::Release)).pack());
                    }
                    if !channel.send(&ControlMessage::Action(Action::simple(ActionKind::Heartbeat)).pack()) { return Outcome::Lost; }
                }
                if let Some(attachment) = &mut attachment {
                    let status = sound.as_ref().map(nexdesk_media::ReceivedSound::status).unwrap_or_default();
                    attachment.sample(view, status).await;
                }
            }
            action = commands.actions.recv() => {
                if let Some(action) = action && action.generation == state.generation
                    && (action.kind() == ActionKind::Select || (state.can_input && !release_pending))
                    && !channel.send(&ControlMessage::Action(action).pack()) {
                    release_pending = true;
                }
            }
        }
    }
}

fn status(error: SessionError) -> u32 {
    match error {
        SessionError::Pairing(PairingError::IdentityChanged) => HOST_CHANGED,
        SessionError::Rejected | SessionError::Pairing(_) => REJECTED,
        SessionError::Declined => DECLINED,
        SessionError::Unconfirmed => STOPPED,
        SessionError::ResumeExpired => REVOKED,
        SessionError::Unreachable => UNREACHABLE,
        SessionError::Busy => BUSY,
        SessionError::Unsupported => UNSUPPORTED,
        _ => MEDIA_FAILED,
    }
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum LocalOperation {
    DirectoryHealth {
        origin: String,
    },
    DirectoryPresence {
        origin: String,
        device: String,
    },
    Recent,
    DirectoryRecent,
    DirectoryRename {
        origin: String,
        device: String,
        label: String,
    },
    DirectoryForget {
        origin: String,
        device: String,
        remove_recent: bool,
    },
    Rename {
        endpoint: String,
        label: String,
    },
    Forget {
        endpoint: String,
        remove_recent: bool,
    },
    HostPreferences,
    SaveHostPreferences {
        settings: HostPreferences,
    },
    DesktopPreferences,
    SaveDesktopPreferences {
        settings: DesktopPreferences,
    },
    TrustedDevices,
    RevokeTrustedDevice {
        scope: String,
        identity: String,
    },
    SessionHistory,
    ClearSessionHistory,
}

/// # Safety
/// `request` and `output` are valid, nonoverlapping readable and writable buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m2_local(
    request: *const u8,
    length: usize,
    output: *mut u8,
    capacity: usize,
) -> usize {
    if request.is_null() || !(1..=12 * 1024).contains(&length) {
        return 0;
    }
    // SAFETY: The caller supplies a stable readable input for just this call.
    let Ok(operation) = serde_json::from_slice::<LocalOperation>(unsafe {
        std::slice::from_raw_parts(request, length)
    }) else {
        return 0;
    };
    let result = match local_operation(operation) {
        Ok(value) => json!({"ok":value}),
        Err(_) => json!({"error":"storage_unavailable"}),
    };
    // SAFETY: `output` remains caller-owned and cannot alias `request`.
    unsafe { write_json(&result, output, capacity) }
}

fn local_operation(operation: LocalOperation) -> Result<Value, StoreError> {
    if let LocalOperation::DirectoryHealth { origin } = operation {
        let origin = origin.parse().map_err(|_| StoreError::Invalid)?;
        let client = DirectoryClient::new(origin).map_err(|_| StoreError::Invalid)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| StoreError::Invalid)?;
        return Ok(json!(runtime.block_on(client.health())));
    }
    if let LocalOperation::DirectoryPresence { origin, device } = operation {
        let origin = origin.parse().map_err(|_| StoreError::Invalid)?;
        let device = device.parse().map_err(|_| StoreError::Invalid)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| StoreError::Invalid)?;
        return match runtime
            .block_on(async { DirectoryClient::new(origin)?.presence(&device).await })
        {
            Ok(presence) => Ok(json!(presence)),
            Err(_) => Ok(json!({"online":null})),
        };
    }
    let store = Store::open()?;
    let address = |endpoint: String| endpoint.parse().map_err(|_| StoreError::Invalid);
    match operation {
        LocalOperation::DirectoryPresence { .. } | LocalOperation::DirectoryHealth { .. } => {
            unreachable!("handled before database access")
        }
        LocalOperation::Recent => Ok(json!(store.recent()?)),
        LocalOperation::DirectoryRecent => Ok(json!(store.directory_recent()?)),
        LocalOperation::DirectoryRename {
            origin,
            device,
            label,
        } => {
            store.directory_rename(
                &origin.parse().map_err(|_| StoreError::Invalid)?,
                &device.parse().map_err(|_| StoreError::Invalid)?,
                &label,
            )?;
            Ok(Value::Null)
        }
        LocalOperation::DirectoryForget {
            origin,
            device,
            remove_recent,
        } => {
            store.directory_forget(
                &origin.parse().map_err(|_| StoreError::Invalid)?,
                &device.parse().map_err(|_| StoreError::Invalid)?,
                remove_recent,
            )?;
            Ok(Value::Null)
        }
        LocalOperation::Rename { endpoint, label } => {
            store.rename(address(endpoint)?, &label)?;
            Ok(Value::Null)
        }
        LocalOperation::Forget {
            endpoint,
            remove_recent,
        } => {
            store.forget(address(endpoint)?, remove_recent)?;
            Ok(Value::Null)
        }
        LocalOperation::HostPreferences => Ok(json!(store.host_preferences()?)),
        LocalOperation::DesktopPreferences => Ok(json!(store.desktop_preferences()?)),
        LocalOperation::SaveDesktopPreferences { settings } => {
            Ok(json!(store.save_desktop_preferences(settings)?))
        }
        LocalOperation::TrustedDevices => Ok(json!(store.trusted_devices()?)),
        LocalOperation::RevokeTrustedDevice { scope, identity } => {
            store.revoke_controller(&scope, &ControllerIdentity::parse(&identity)?)?;
            Ok(Value::Null)
        }
        LocalOperation::SessionHistory => Ok(json!(store.session_history()?)),
        LocalOperation::ClearSessionHistory => {
            store.clear_session_history()?;
            Ok(json!({}))
        }
        LocalOperation::SaveHostPreferences { settings } => {
            LanEndpoint::parse(&settings.address, &settings.port.to_string())
                .map_err(|_| StoreError::Invalid)?;
            if settings.source.len() > 128
                || settings.share_root.len() > 2048
                || (!settings.share_root.is_empty()
                    && !std::path::Path::new(&settings.share_root).is_absolute())
            {
                return Err(StoreError::Invalid);
            }
            store.save_host_preferences(&settings)?;
            Ok(Value::Null)
        }
    }
}

pub(crate) unsafe fn write_json(value: &impl Serialize, output: *mut u8, capacity: usize) -> usize {
    if output.is_null() || capacity > MAX_OUTPUT {
        return 0;
    }
    let Ok(bytes) = serde_json::to_vec(value) else {
        return 0;
    };
    if bytes.len() > capacity {
        return 0;
    }
    // SAFETY: Maximum length was checked against diagnosed caller capacity; the Rust allocation is distinct.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len()) };
    bytes.len()
}
