use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use axum::{
    Extension, Json,
    extract::{ConnectInfo, OriginalUri, State},
    http::{HeaderMap, StatusCode, header},
};
use nexdesk_directory::{IceConfiguration, RemoteIdentity};
use nexdesk_media::{
    CaptureKind, CaptureStatus, CaptureTarget, ControlPipe, DesktopMediaSettings, HostPeer,
    capture_sources,
};
use nexdesk_platform::{
    AudioCapture, DisplayMode, Input, InputError, InputLease, Monitor, NativeInput, Region,
    screen_region,
};
use nexdesk_session::{
    DesktopAnswer, DesktopOffer,
    collaboration::Capabilities,
    control::{Action, ActionKind, ControlMessage, DesktopState, Display, EndReason, Grant},
    ice_options,
    pairing::{LoginConfirmation, ProtectedAnswer},
};
use tokio::sync::{OwnedSemaphorePermit, watch};
use zeroize::Zeroizing;

use crate::{
    collaboration::Collaboration,
    desktop_host::{DesktopHost, Incoming},
    file_service::FileService,
    server::Host,
};

const CONTACT_TIME: Duration = Duration::from_secs(5);
const CONNECT_TIME: Duration = Duration::from_secs(12);

pub(crate) async fn finish(
    State(host): State<Arc<Host>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    OriginalUri(uri): OriginalUri,
    Extension(closed): Extension<watch::Receiver<bool>>,
    headers: HeaderMap,
    Json(request): Json<LoginConfirmation>,
) -> Result<Json<ProtectedAnswer>, StatusCode> {
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    finish_session(
        host,
        source.ip().to_canonical(),
        request,
        uri.path() == "/m3/pair/finish",
        None,
        closed,
    )
    .await
    .map(Json)
}

pub(crate) struct DirectorySession {
    pub remote: RemoteIdentity,
    pub flow: String,
    pub ice: IceConfiguration,
}

pub(crate) async fn finish_session(
    host: Arc<Host>,
    ip: IpAddr,
    request: LoginConfirmation,
    extended: bool,
    directory: Option<DirectorySession>,
    mut closed: watch::Receiver<bool>,
) -> Result<ProtectedAnswer, StatusCode> {
    let desktop = host.desktop.clone().ok_or(StatusCode::NOT_FOUND)?;
    let slot = desktop.reserve()?;
    let mut revoked = desktop.revocation(directory.is_some());
    let epoch = *revoked.borrow();
    let target = desktop.capture();
    let mut shutdown = host.shutdown.clone();
    if *shutdown.borrow() || *closed.borrow() || closed.has_changed().is_err() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let challenge = request.id.clone();
    let pending = host.take_challenge(
        ip,
        directory.as_ref().map(|session| session.flow.as_str()),
        &request.id,
    )?;
    let (clear, key) = pending
        .accept(request)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let clear = Zeroizing::new(clear);
    let mut offer: DesktopOffer =
        serde_json::from_str(&clear).map_err(|_| StatusCode::BAD_REQUEST)?;
    let controller = offer
        .controller_proof
        .take()
        .map(|proof| proof.verify(&challenge, &offer))
        .transpose()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let remote = directory.as_ref().map(|session| &session.remote);
    if offer.capabilities.is_some() != extended || offer.remote.as_ref() != remote {
        return Err(StatusCode::BAD_REQUEST);
    }
    let input = if offer.request_input && desktop.allows_control() && is_screen(target) {
        NativeInput::open().ok()
    } else {
        None
    };
    let share = desktop.share();
    let mut available = Capabilities::CLIPBOARD;
    if AudioCapture::supported() {
        available = available.with(Capabilities::AUDIO);
    }
    if share.is_some() {
        available = available
            .with(Capabilities::FILE_READ)
            .with(Capabilities::FILE_WRITE);
    }
    if monitor_for_capture(target).is_some() {
        available = available.with(Capabilities::DISPLAY);
    }
    if input.is_some() && cfg!(any(windows, target_os = "macos")) {
        available = available.with(Capabilities::TEXT);
    }
    if remote.is_some() && host.allow_shell.load(Ordering::Acquire) {
        available = available.with(Capabilities::SHELL);
    }
    let requested = offer
        .capabilities
        .unwrap_or_default()
        .intersection(available);
    let (id, access) = tokio::select! {
        biased;
        _ = revoked.changed() => return Err(StatusCode::FORBIDDEN),
        _ = shutdown.changed() => return Err(StatusCode::SERVICE_UNAVAILABLE),
        _ = closed.wait_for(|value| *value) => return Err(StatusCode::FORBIDDEN),
        result = desktop.authorize(Incoming {
            ip, request_input: offer.request_input, can_input: input.is_some(),
            capabilities: requested, remote: remote.cloned(), controller,
        }, offer.resume.as_deref()) => result?,
    };
    let media_settings = DesktopMediaSettings {
        capture: target,
        extended,
        audio: access.capabilities.includes(Capabilities::AUDIO),
        encoding: desktop.encoding(),
    };
    let shell = access.capabilities.includes(Capabilities::SHELL);
    let negotiate = async {
        if let Some(session) = &directory {
            host.media
                .answer_routed(
                    &offer.sdp,
                    media_settings,
                    ice_options(session.ice.clone()),
                    shell,
                )
                .await
        } else {
            host.media.answer_desktop(&offer.sdp, media_settings).await
        }
    };
    let (peer, sdp, pipes) = tokio::select! {
        biased;
        _ = revoked.changed() => return Err(StatusCode::FORBIDDEN),
        _ = shutdown.changed() => return Err(StatusCode::SERVICE_UNAVAILABLE),
        _ = closed.wait_for(|value| *value) => return Err(StatusCode::FORBIDDEN),
        result = tokio::time::timeout(Duration::from_secs(8), negotiate) => result
            .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?.map_err(|_| StatusCode::BAD_REQUEST)?,
    };
    let grant = access.input;
    let capabilities = access.capabilities;
    let service = FileService {
        scope: share.zip(access.files.clone()),
        capabilities,
        revoked: revoked.clone(),
        shutdown: shutdown.clone(),
    };
    let resume = desktop.begin(id, ip, access, epoch)?;
    let answer = Zeroizing::new(
        serde_json::to_string(&DesktopAnswer {
            sdp,
            grant,
            resume: resume.to_string(),
            capabilities: extended.then_some(capabilities),
            remote: remote.cloned(),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    let answer = key
        .seal(&answer)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let input = (grant == Grant::Control).then_some(input).flatten();
    let (files_stop, file_ended) = watch::channel(false);
    let shell_task = pipes
        .shell
        .map(|pipe| tokio::spawn(crate::shell::serve(pipe, file_ended.clone())));
    let file_task = pipes
        .files
        .map(|pipe| tokio::spawn(service.run(pipe, file_ended)));
    tokio::spawn(run(DesktopSession {
        owner: desktop,
        id,
        peer,
        channel: pipes.control,
        collaboration: pipes
            .collaboration
            .map(|pipe| Collaboration::new(pipe, capabilities)),
        files_stop,
        file_task,
        shell_task,
        input,
        target,
        directory: remote.is_some(),
        revoked,
        shutdown,
        closed,
        _slot: slot,
    }));
    Ok(ProtectedAnswer { answer })
}

struct DesktopSession {
    owner: Arc<DesktopHost>,
    id: u64,
    peer: HostPeer,
    channel: ControlPipe,
    collaboration: Option<Collaboration>,
    files_stop: watch::Sender<bool>,
    file_task: Option<tokio::task::JoinHandle<()>>,
    shell_task: Option<tokio::task::JoinHandle<()>>,
    input: Option<NativeInput>,
    target: CaptureTarget,
    directory: bool,
    revoked: watch::Receiver<u64>,
    shutdown: watch::Receiver<bool>,
    closed: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
}

async fn run(session: DesktopSession) {
    let DesktopSession {
        owner,
        id,
        peer,
        mut channel,
        mut collaboration,
        files_stop,
        file_task,
        shell_task,
        input,
        target,
        directory,
        mut revoked,
        mut shutdown,
        mut closed,
        _slot,
    } = session;
    let mut desktop = RemoteDesktop::new(target, input);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let start = Instant::now();
    let mut touched = None;
    let mut last_sent = None;
    let fixture = owner.fixture_interruption();
    tokio::pin!(fixture);
    let (reason, resumable) = loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => break (EndReason::Revoked, false),
            _ = revoked.changed() => break (EndReason::Revoked, false),
            _ = receiver_closed(&mut closed) => break (EndReason::Revoked, false),
            _ = &mut fixture => break (EndReason::Closed, true),
            _ = tick.tick() => {
                if let Some(collaboration) = &mut collaboration { collaboration.tick(&peer, &mut desktop); }
                let result = desktop.update(&peer).await;
                if result.is_err() { break (EndReason::CaptureUnavailable, false); }
                if peer.has_ended() || channel.is_closed() || touched.is_some_and(|time: Instant| time.elapsed() > CONTACT_TIME) {
                    break (EndReason::Closed, true);
                }
                if touched.is_none() && start.elapsed() > CONNECT_TIME { break (EndReason::Closed, false); }
                if channel.is_open() && last_sent.as_ref() != Some(&desktop.state) {
                    if !channel.send(&ControlMessage::State(desktop.state.clone()).pack()) { break (EndReason::Protocol, false); }
                    last_sent = Some(desktop.state.clone());
                }
            }
            message = Collaboration::recv(&mut collaboration), if collaboration.is_some() => {
                if revoked.has_changed().unwrap_or(true)
                    || shutdown.has_changed().unwrap_or(true)
                    || closed.has_changed().unwrap_or(true) {
                    break (EndReason::Revoked, false);
                }
                if let Some((id, request)) = message {
                    let keep = if let Some(collaboration) = &mut collaboration {
                        collaboration.respond(id, request, &peer, &mut desktop).await
                    } else { false };
                    if !keep { collaboration = None; }
                } else {
                    collaboration = None;
                }
            }
            packet = channel.recv() => {
                let Some(packet) = packet else { break (EndReason::Closed, true); };
                if revoked.has_changed().unwrap_or(true)
                    || shutdown.has_changed().unwrap_or(true)
                    || closed.has_changed().unwrap_or(true) {
                    break (EndReason::Revoked, false);
                }
                let Some(ControlMessage::Action(action)) = ControlMessage::unpack(&packet) else {
                    break (EndReason::Protocol, false);
                };
                touched = Some(Instant::now());
                if action.kind() == ActionKind::Close { break (EndReason::Closed, false); }
                if action.kind() == ActionKind::Select
                    && action.generation == desktop.state.generation
                    && desktop.state.selected != action.display
                    && desktop.choices.iter().any(|choice| choice.display.id == action.display)
                    && let Some(collaboration) = &mut collaboration
                {
                    collaboration.restore_display();
                }
                desktop.action(&peer, action);
            }
        }
    };
    desktop.release();
    let _ = files_stop.send(true);
    if let Some(file_task) = file_task {
        file_task.abort();
    }
    if let Some(shell_task) = shell_task {
        let _ = tokio::time::timeout(Duration::from_secs(2), shell_task).await;
    }
    drop(collaboration);
    peer.stop_capture();
    if !resumable && channel.send(&ControlMessage::End(reason as i32).pack()) {
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    owner.end(id, resumable, directory);
}

async fn receiver_closed(closed: &mut watch::Receiver<bool>) {
    let _ = closed.wait_for(|value| *value).await;
}

#[derive(Clone, PartialEq)]
struct Choice {
    display: Display,
    source_id: u64,
    platform_id: i64,
}

pub(crate) struct RemoteDesktop {
    target: CaptureTarget,
    choices: Vec<Choice>,
    selected_since: Instant,
    next_refresh: Instant,
    state: DesktopState,
    input: Option<InputLease<NativeInput>>,
    region: Option<Region>,
}

impl RemoteDesktop {
    fn new(target: CaptureTarget, input: Option<NativeInput>) -> Self {
        let now = Instant::now();
        Self {
            target,
            choices: Vec::new(),
            selected_since: now,
            next_refresh: now,
            state: DesktopState {
                generation: 1,
                ..DesktopState::default()
            },
            input: input.map(InputLease::new),
            region: None,
        }
    }

    async fn update(&mut self, peer: &HostPeer) -> Result<(), ()> {
        if is_screen(self.target) && Instant::now() >= self.next_refresh {
            self.next_refresh = Instant::now() + Duration::from_secs(2);
            if let Ok(Ok(choices)) = tokio::task::spawn_blocking(display_choices).await {
                self.refresh_displays(peer, choices);
            }
        }
        let status = peer.status();
        peer.refresh_quality(status);
        if status.unavailable {
            if let Some(fallback) = self
                .choices
                .iter()
                .find(|choice| choice.display.id != self.state.selected)
                .cloned()
            {
                self.select(peer, &fallback, "display_changed");
                return Ok(());
            }
            return Err(());
        }
        if status.submitted == 0 && self.selected_since.elapsed() > CONNECT_TIME {
            return Err(());
        }
        self.apply_region(status);
        Ok(())
    }

    fn refresh_displays(&mut self, peer: &HostPeer, choices: Vec<Choice>) {
        let CaptureTarget::Source { id, .. } = self.target else {
            return;
        };
        if self.state.selected.is_empty()
            && let Some(choice) = choices.iter().find(|choice| choice.source_id == id)
        {
            self.state.selected.clone_from(&choice.display.id);
        }
        self.state.displays = choices
            .iter()
            .map(|choice| choice.display.clone())
            .collect();
        if let Some(choice) = choices
            .iter()
            .find(|choice| choice.display.id == self.state.selected)
        {
            if choice.source_id != id {
                self.select(peer, choice, "display_changed");
            }
        } else if let Some(choice) = choices.first() {
            self.select(peer, choice, "display_changed");
        } else {
            self.release();
            self.state.can_input = false;
            self.region = None;
        }
        self.choices = choices;
    }

    fn select(&mut self, peer: &HostPeer, choice: &Choice, notice: &str) {
        self.release();
        let target = CaptureTarget::Source {
            kind: CaptureKind::Screen,
            id: choice.source_id,
        };
        if peer.switch_source(target).is_err() {
            self.state.notice = "capture_unavailable".into();
            return;
        }
        self.target = target;
        self.selected_since = Instant::now();
        self.region = None;
        self.state.generation += 1;
        self.state.width = 0;
        self.state.height = 0;
        self.state.can_input = false;
        self.state.selected.clone_from(&choice.display.id);
        self.state.notice = notice.into();
    }

    pub(crate) fn monitor(&self, display: &str) -> Option<Monitor> {
        let choice = self
            .choices
            .iter()
            .find(|choice| choice.display.id == self.state.selected)?;
        let monitor = Monitor::locate(choice.source_id, choice.platform_id)?;
        (monitor.key() == display || self.state.selected == display).then_some(monitor)
    }

    pub(crate) fn text(&mut self, generation: u64, text: &str) -> Result<(), InputError> {
        if !self.state.can_input || generation != self.state.generation {
            return Err(InputError::Unavailable);
        }
        self.input
            .as_mut()
            .ok_or(InputError::Unavailable)?
            .insert_text(text)
    }

    pub(crate) fn begin_mode_change(&mut self) {
        self.release();
        self.state.generation += 1;
        self.state.can_input = false;
        self.state.width = 0;
        self.state.height = 0;
        self.region = None;
        self.selected_since = Instant::now();
    }

    pub(crate) fn refresh_capture(&mut self, peer: &HostPeer) {
        let _ = peer.switch_source(self.target);
    }

    pub(crate) fn mode_visible(&self, mode: DisplayMode) -> bool {
        self.state.width.abs_diff(mode.width) <= 1 && self.state.height.abs_diff(mode.height) <= 1
    }

    fn apply_region(&mut self, status: CaptureStatus) {
        let Some(capture) = status.region else {
            return;
        };
        let bounds = Region {
            left: capture.left,
            top: capture.top,
            width: capture.width,
            height: capture.height,
        };
        let choice = self
            .choices
            .iter()
            .find(|choice| choice.display.id == self.state.selected);
        let region =
            choice.and_then(|choice| screen_region(choice.source_id, choice.platform_id, bounds));
        if self.region != region && self.region.is_some() {
            self.release();
            self.state.generation += 1;
        }
        self.region = region;
        self.state.width = ((capture.width + 1) & !1).min(4096);
        self.state.height = ((capture.height + 1) & !1).min(4096);
        self.state.can_input = region.is_some()
            && self.input.is_some()
            && status.hardware_encoded == 0
            && self.selected_since.elapsed() >= Duration::from_millis(300);
    }

    fn release(&mut self) {
        if let Some(input) = &mut self.input {
            input.release();
        }
    }

    fn action(&mut self, peer: &HostPeer, action: Action) {
        if action.kind() == ActionKind::Release {
            self.release();
            return;
        }
        if action.kind() == ActionKind::Heartbeat || action.generation != self.state.generation {
            return;
        }
        if action.kind() == ActionKind::Select {
            if action.display != self.state.selected
                && let Some(choice) = self
                    .choices
                    .iter()
                    .find(|choice| choice.display.id == action.display)
                    .cloned()
            {
                self.select(peer, &choice, "");
            }
            return;
        }
        if !self.state.can_input {
            return;
        }
        let (Some(input), Some(region)) = (&mut self.input, self.region) else {
            return;
        };
        if let Err(InputError::Rejected | InputError::Unavailable) =
            apply_action(input, region, action)
        {
            input.release();
            self.input = None;
            self.state.can_input = false;
            self.state.notice = "input_unavailable".into();
        }
    }
}

fn apply_action<T: nexdesk_platform::Injector>(
    input: &mut InputLease<T>,
    region: Region,
    action: Action,
) -> Result<(), InputError> {
    match action.kind() {
        ActionKind::Move | ActionKind::Button => {
            let (x, y) = region.point(action.x as u16, action.y as u16);
            input.send(Input::Move { x, y })?;
            if action.kind() == ActionKind::Button {
                input.send(Input::Button {
                    button: action.code as u8,
                    down: action.down,
                })?;
            }
        }
        ActionKind::Key => input.send(Input::Key {
            usage: action.code,
            down: action.down,
        })?,
        ActionKind::Wheel => input.send(Input::Wheel {
            horizontal: action.x,
            vertical: action.y,
        })?,
        _ => {}
    }
    Ok(())
}

fn is_screen(target: CaptureTarget) -> bool {
    matches!(
        target,
        CaptureTarget::Source {
            kind: CaptureKind::Screen,
            ..
        }
    )
}

fn monitor_for_capture(target: CaptureTarget) -> Option<Monitor> {
    let CaptureTarget::Source {
        kind: CaptureKind::Screen,
        id,
    } = target
    else {
        return None;
    };
    let source = capture_sources(CaptureKind::Screen)
        .ok()?
        .into_iter()
        .find(|source| source.id == id)?;
    Monitor::locate(source.id, source.display_id)
}

fn display_choices() -> Result<Vec<Choice>, nexdesk_media::MediaError> {
    Ok(capture_sources(CaptureKind::Screen)?
        .into_iter()
        .take(32)
        .enumerate()
        .map(|(index, source)| {
            let platform_id = source.display_id;
            let id = if let Some(monitor) = Monitor::locate(source.id, platform_id) {
                monitor.key().to_owned()
            } else if platform_id > 0 {
                format!("display-{platform_id}")
            } else {
                format!("screen-{}", source.id)
            };
            let label = if source.title.is_empty() {
                format!("Display {}", index + 1)
            } else {
                source.title.chars().take(25).collect()
            };
            Choice {
                display: Display { id, label },
                source_id: source.id,
                platform_id,
            }
        })
        .collect())
}
