#![forbid(unsafe_code)]

mod metrics;
mod store;

use std::{
    collections::{HashMap, VecDeque},
    env, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path as PathParam, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, Uri, header},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use nexdesk_core::DeviceId;
use nexdesk_directory::{
    BODY_LIMIT, DevicePresence, FromHost, IceConfiguration, IceServer, OpenRequest,
    PROTOCOL_VERSION, PairStep, RoutedRequest, SOCKET_LIMIT, ServerOrigin, SessionRoute, ToHost,
    registration_message, token_bytes, valid_ice_url,
};
use ring::{
    digest, hmac,
    rand::{SecureRandom, SystemRandom},
    signature,
};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

use store::{Enrollment, Registry};

const MAX_ONLINE: usize = 1024;
const MAX_ROUTES: usize = 512;
const MAX_DEVICE_ROUTES: usize = 8;
const ROUTE_LIFE: Duration = Duration::from_secs(90);
const WINDOW: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_secs(10);

struct Service {
    origin: ServerOrigin,
    registry: Arc<Registry>,
    enrollment: Option<[u8; 32]>,
    proxy: Option<IpAddr>,
    web: PathBuf,
    ice: IcePolicy,
    live: Mutex<Live>,
    serial: AtomicU64,
    pairing_slots: Arc<Semaphore>,
    metrics: metrics::Metrics,
}

#[derive(Default)]
struct Live {
    devices: HashMap<DeviceId, Device>,
    routes: HashMap<String, Route>,
    opens: VecDeque<(IpAddr, DeviceId, Instant)>,
}

struct Device {
    lease: u64,
    send: mpsc::Sender<Dispatch>,
}

struct Route {
    device: DeviceId,
    lease: u64,
    source: IpAddr,
    expires: Instant,
    state: RouteState,
    host_ice: IceConfiguration,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RouteState {
    New,
    Starting,
    Ready,
}

type Reply = (StatusCode, Value);

struct Dispatch {
    call: RoutedRequest,
    result: oneshot::Sender<Reply>,
}

struct IcePolicy {
    stun: Vec<String>,
    turn: Vec<String>,
    secret: Option<hmac::Key>,
    relay_only: bool,
}

impl IcePolicy {
    fn read() -> Result<Self, io::Error> {
        fn urls(name: &str, kinds: &[&str]) -> Result<Vec<String>, io::Error> {
            let raw = env::var(name).unwrap_or_default();
            let values: Vec<String> = raw
                .split(|character: char| character.is_whitespace() || character == ',')
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
            if values.len() > 4
                || values.iter().any(|value| {
                    !valid_ice_url(value)
                        || !value
                            .split_once(':')
                            .is_some_and(|(kind, _)| kinds.contains(&kind))
                })
            {
                return Err(io::Error::other(format!("invalid {name} URLs")));
            }
            Ok(values)
        }
        let stun = urls("NEXDESK_STUN_URLS", &["stun", "stuns"])?;
        let turn = urls("NEXDESK_TURN_URLS", &["turn", "turns"])?;
        let secret = env::var("NEXDESK_TURN_SECRET")
            .ok()
            .filter(|secret| !secret.is_empty());
        let relay_only = env::var("NEXDESK_ICE_RELAY_ONLY").as_deref() == Ok("1");
        if !turn.is_empty() && secret.as_ref().is_none_or(|secret| secret.len() < 16)
            || relay_only && turn.is_empty()
        {
            return Err(io::Error::other(
                "TURN URLs require a secret and relay-only mode requires TURN",
            ));
        }
        Ok(Self {
            stun,
            turn,
            relay_only,
            secret: secret.map(|secret| {
                hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes())
            }),
        })
    }

    fn credentials(&self) -> Result<IceConfiguration, StatusCode> {
        let mut servers = Vec::new();
        if !self.stun.is_empty() {
            servers.push(IceServer {
                urls: self.stun.clone(),
                username: String::new(),
                credential: String::new(),
            });
        }
        if let Some(secret) = self.secret.as_ref().filter(|_| !self.turn.is_empty()) {
            let expiry = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
                .as_secs()
                + 3600;
            let username = format!("{expiry}:{}", random::<12>()?);
            let credential = STANDARD.encode(hmac::sign(secret, username.as_bytes()).as_ref());
            servers.push(IceServer {
                urls: self.turn.clone(),
                username,
                credential,
            });
        }
        Ok(IceConfiguration {
            servers,
            relay_only: self.relay_only,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if let [command, db, device] = args.as_slice()
        && command == "revoke"
    {
        if !Registry::open(Path::new(db))?.revoke(&device.parse()?)? {
            return Err(io::Error::other("device does not exist").into());
        }
        println!("Device revoked");
        return Ok(());
    }
    if let [command, db, destination] = args.as_slice()
        && command == "backup"
    {
        if !Path::new(db).is_file() {
            return Err(io::Error::other("source registry does not exist").into());
        }
        Registry::open(Path::new(db))?.backup(Path::new(destination))?;
        println!("Registry backup completed");
        return Ok(());
    }
    if !args.is_empty() {
        return Err(io::Error::other(
            "Usage: nexdesk-signaling [revoke DATABASE DEVICE_ID | backup DATABASE OUTPUT]",
        )
        .into());
    }
    let listen: SocketAddr = env::var("NEXDESK_SIGNAL_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:54000".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    let origin: ServerOrigin = env::var("NEXDESK_SIGNAL_ORIGIN")
        .unwrap_or_else(|_| format!("http://{address}"))
        .parse()?;
    let database = env::var_os("NEXDESK_SIGNAL_DB")
        .map_or_else(|| PathBuf::from("nexdesk-directory.sqlite3"), PathBuf::from);
    let web = env::var_os("NEXDESK_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|path| Some(path.parent()?.join("web")))
                .filter(|path| path.is_dir())
                .unwrap_or_else(|| PathBuf::from("apps/web/dist"))
        });
    let enrollment = env::var("NEXDESK_ENROLLMENT_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| sha256(key.as_bytes()));
    let service = Arc::new(Service {
        origin,
        registry: Arc::new(Registry::open(&database)?),
        enrollment,
        proxy: env::var("NEXDESK_TRUSTED_PROXY")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<IpAddr>().map(|ip| ip.to_canonical()))
            .transpose()?,
        web,
        ice: IcePolicy::read()?,
        live: Mutex::new(Live::default()),
        serial: AtomicU64::new(1),
        pairing_slots: Arc::new(Semaphore::new(64)),
        metrics: metrics::Metrics::default(),
    });
    let metrics_address = metrics::start(service.clone()).await?;
    let app = Router::new()
        .route(
            "/healthz",
            get(|| async { Json(json!({"ok":true,"protocol":PROTOCOL_VERSION})) }),
        )
        .route("/api/v1/host", get(upgrade_host))
        .route("/api/v1/devices/{device}/presence", get(presence))
        .route(
            "/api/v1/devices/{device}/session",
            post(open_route).layer(from_fn_with_state(service.clone(), metrics::open)),
        )
        .route(
            "/api/v1/sessions/{route}/{step}",
            post(transfer).layer(from_fn_with_state(service.clone(), metrics::handshake)),
        )
        .fallback(static_file)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(service);
    println!("{}", json!({"event":"ready","listen":address}));
    if let Some(address) = metrics_address {
        println!("{}", json!({"event":"metrics_ready","listen":address}));
    }
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

async fn upgrade_host(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // This endpoint authenticates a native host, independently of browser requests.
    if headers.contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(slot) = service.pairing_slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_message_size(SOCKET_LIMIT)
        .max_frame_size(SOCKET_LIMIT)
        .on_upgrade(move |socket| host_socket(service, socket, slot))
}

async fn send(socket: &mut WebSocket, message: ToHost) -> Option<()> {
    let json = serde_json::to_string(&message).ok()?;
    if json.len() > SOCKET_LIMIT {
        return None;
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(json.into())),
    )
    .await
    .ok()?
    .ok()
}

async fn register(service: &Arc<Service>, socket: &mut WebSocket) -> Option<DeviceId> {
    let nonce = random::<32>().ok()?;
    send(
        socket,
        ToHost::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await?;
    let Message::Text(message) = tokio::time::timeout(Duration::from_secs(10), socket.recv())
        .await
        .ok()??
        .ok()?
    else {
        return None;
    };
    let FromHost::Register(proof) = serde_json::from_str(&message).ok()? else {
        return None;
    };
    if proof.version != PROTOCOL_VERSION || token_bytes::<32>(&proof.identity).is_none() {
        return None;
    }
    let key = token_bytes::<32>(&proof.key)?;
    let signed = token_bytes::<64>(&proof.signature)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(
            &registration_message(&service.origin, &nonce, &proof.key, &proof.identity),
            &signed,
        )
        .ok()?;
    let can_enroll = service.enrollment.is_none_or(|expected| {
        proof.enrollment.as_ref().is_some_and(|actual| {
            let key = hmac::Key::new(hmac::HMAC_SHA256, &expected);
            let supplied = hmac::Key::new(hmac::HMAC_SHA256, &sha256(actual.as_bytes()));
            hmac::verify(
                &key,
                b"nexdesk enrollment",
                hmac::sign(&supplied, b"nexdesk enrollment").as_ref(),
            )
            .is_ok()
        })
    });
    let registry = service.registry.clone();
    match tokio::task::spawn_blocking(move || {
        registry.register(&proof.key, &proof.identity, can_enroll)
    })
    .await
    .ok()?
    .ok()?
    {
        Enrollment::Registered(device) => Some(device),
        Enrollment::Denied => {
            send(socket, ToHost::Revoked).await;
            None
        }
    }
}

async fn host_socket(service: Arc<Service>, mut socket: WebSocket, slot: OwnedSemaphorePermit) {
    let Some(device) = register(&service, &mut socket).await else {
        return;
    };
    drop(slot);
    let lease = service.serial.fetch_add(1, Relaxed);
    let (send_queue, mut queue) = mpsc::channel::<Dispatch>(16);
    if let Ok(mut live) = service.live.lock() {
        if live.devices.len() >= MAX_ONLINE && !live.devices.contains_key(&device) {
            return;
        }
        live.devices.insert(
            device.clone(),
            Device {
                lease,
                send: send_queue,
            },
        );
    } else {
        return;
    }
    let _lease = HostLease {
        service: &service,
        device: &device,
        lease,
    };
    if send(
        &mut socket,
        ToHost::Registered {
            device: device.to_string(),
        },
    )
    .await
    .is_none()
    {
        return;
    }
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut activity = Instant::now();
    let mut pending = HashMap::<u64, oneshot::Sender<Reply>>::new();
    loop {
        tokio::select! {
            queued = queue.recv() => {
                let Some(dispatch) = queued else { break };
                pending.retain(|_, send| !send.is_closed());
                if pending.len() >= 16 {
                    let _ = dispatch.result.send((StatusCode::SERVICE_UNAVAILABLE, Value::Null));
                    continue;
                }
                if dispatch.result.is_closed() { continue }
                pending.insert(dispatch.call.call, dispatch.result);
                if send(&mut socket, ToHost::Request(dispatch.call)).await.is_none() { break }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                activity = Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(FromHost::Reply { call, status, body }) = serde_json::from_str(&text) else { break };
                        let Ok(status) = StatusCode::from_u16(status) else { break };
                        if let Some(result) = pending.remove(&call) {
                            let _ = result.send((status, body));
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    _ => break,
                }
            }
            _ = heartbeat.tick() => {
                if activity.elapsed() > HEARTBEAT * 4 { break }
                let registry = service.registry.clone();
                let id = device.clone();
                if !matches!(tokio::task::spawn_blocking(move || registry.allowed(&id)).await, Ok(Ok(true))) {
                    send(&mut socket, ToHost::Revoked).await;
                    break;
                }
                if !matches!(tokio::time::timeout(HEARTBEAT, socket.send(Message::Ping(Default::default()))).await, Ok(Ok(()))) {
                    break;
                }
            }
        }
    }
}

struct HostLease<'a> {
    service: &'a Service,
    device: &'a DeviceId,
    lease: u64,
}

impl Drop for HostLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut live) = self.service.live.lock() {
            if live
                .devices
                .get(self.device)
                .is_some_and(|entry| entry.lease == self.lease)
            {
                live.devices.remove(self.device);
            }
            live.routes.retain(|_, route| route.lease != self.lease);
        }
    }
}

async fn open_route(
    State(service): State<Arc<Service>>,
    PathParam(device): PathParam<String>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<OpenRequest>,
) -> Result<Json<SessionRoute>, StatusCode> {
    check_origin(&service, &headers)?;
    if request.version != PROTOCOL_VERSION {
        return Err(StatusCode::BAD_REQUEST);
    }
    let device: DeviceId = device.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let source = client_address(&service, source, &headers)?;
    let now = Instant::now();
    let mut live = service
        .live
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    live.routes.retain(|_, route| route.expires > now);
    if live.routes.len() >= MAX_ROUTES {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    admit_lookup(&mut live, source, &device, now)?;
    let lease = live
        .devices
        .get(&device)
        .ok_or(StatusCode::NOT_FOUND)?
        .lease;
    if live
        .routes
        .values()
        .filter(|route| route.device == device)
        .count()
        >= MAX_DEVICE_ROUTES
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let id = random::<32>()?;
    let ice = service.ice.credentials()?;
    live.routes.insert(
        id.clone(),
        Route {
            device,
            lease,
            source,
            expires: now + ROUTE_LIFE,
            state: RouteState::New,
            host_ice: service.ice.credentials()?,
        },
    );
    Ok(Json(SessionRoute { id, ice }))
}

async fn presence(
    State(service): State<Arc<Service>>,
    PathParam(device): PathParam<String>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    check_origin(&service, &headers)?;
    let device: DeviceId = device.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let source = client_address(&service, source, &headers)?;
    let mut live = service
        .live
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    admit_lookup(&mut live, source, &device, Instant::now())?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(DevicePresence {
            online: live.devices.contains_key(&device),
        }),
    ))
}

fn admit_lookup(
    live: &mut Live,
    source: IpAddr,
    device: &DeviceId,
    now: Instant,
) -> Result<(), StatusCode> {
    live.opens
        .retain(|(_, _, at)| now.saturating_duration_since(*at) < WINDOW);
    if live.opens.len() >= 512
        || live.opens.iter().filter(|(ip, _, _)| *ip == source).count() >= 24
        || live
            .opens
            .iter()
            .filter(|(_, target, _)| target == device)
            .count()
            >= 12
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    live.opens.push_back((source, device.clone(), now));
    Ok(())
}

async fn transfer(
    State(service): State<Arc<Service>>,
    PathParam((id, step)): PathParam<(String, String)>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let source = match client_address(&service, source, &headers) {
        Ok(source) => source,
        Err(status) => return status.into_response(),
    };
    let result = forward(&service, id, step, source, headers, body).await;
    match result {
        Ok((status, value)) => {
            (status, [(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
        }
        Err(status) => status.into_response(),
    }
}

async fn forward(
    service: &Arc<Service>,
    id: String,
    step: String,
    source: IpAddr,
    headers: HeaderMap,
    body: Value,
) -> Result<Reply, StatusCode> {
    check_origin(service, &headers)?;
    if token_bytes::<32>(&id).is_none() || !body.is_object() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let start = match step.as_str() {
        "start" => true,
        "finish" => false,
        _ => return Err(StatusCode::NOT_FOUND),
    };
    let (send_queue, ice) = {
        let mut live = service
            .live
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let entry = live.routes.get_mut(&id).ok_or(StatusCode::GONE)?;
        if entry.source != source || entry.expires <= Instant::now() {
            return Err(StatusCode::GONE);
        }
        if entry.state
            != if start {
                RouteState::New
            } else {
                RouteState::Ready
            }
        {
            return Err(StatusCode::CONFLICT);
        }
        entry.state = RouteState::Starting;
        let (device, lease, ice) = (entry.device.clone(), entry.lease, entry.host_ice.clone());
        if !start {
            live.routes.remove(&id);
        }
        let device = live
            .devices
            .get(&device)
            .filter(|device| device.lease == lease)
            .ok_or(StatusCode::NOT_FOUND)?;
        (device.send.clone(), ice)
    };
    let (result, reply) = oneshot::channel();
    let call = RoutedRequest {
        call: service.serial.fetch_add(1, Relaxed),
        flow: id.clone(),
        source,
        step: if start {
            PairStep::Start
        } else {
            PairStep::Finish
        },
        ice,
        body,
    };
    let response = if send_queue.try_send(Dispatch { call, result }).is_err() {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    } else {
        tokio::time::timeout(Duration::from_secs(if start { 8 } else { 42 }), reply)
            .await
            .map_err(|_| StatusCode::GATEWAY_TIMEOUT)
            .and_then(|result| result.map_err(|_| StatusCode::NOT_FOUND))
    };
    if start && let Ok(mut live) = service.live.lock() {
        if matches!(&response, Ok((status, _)) if status.is_success()) {
            if let Some(entry) = live.routes.get_mut(&id) {
                entry.state = RouteState::Ready;
            }
        } else {
            live.routes.remove(&id);
        }
    }
    response
}

fn check_origin(service: &Service, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(header::ORIGIN)
        .is_some_and(|value| value.to_str().ok() != Some(service.origin.as_str()))
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

fn client_address(
    service: &Service,
    source: SocketAddr,
    headers: &HeaderMap,
) -> Result<IpAddr, StatusCode> {
    let ip = source.ip().to_canonical();
    if service.proxy != Some(ip) {
        return Ok(ip);
    }
    headers
        .get("x-nexdesk-source")
        .and_then(|address| address.to_str().ok())
        .and_then(|address| address.parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .ok_or(StatusCode::BAD_REQUEST)
}

async fn static_file(State(service): State<Arc<Service>>, uri: Uri) -> Response {
    let path = match uri.path() {
        "/" => "index.html",
        path => path.trim_start_matches('/'),
    };
    if path.is_empty()
        || path.len() > 180
        || path.split('/').any(|piece| {
            piece.is_empty()
                || piece.starts_with('.')
                || !piece
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mime = match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let file = service.web.join(path);
    let bytes = tokio::task::spawn_blocking(move || {
        if std::fs::metadata(&file)?.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("asset too large"));
        }
        std::fs::read(file)
    })
    .await;
    let Ok(Ok(bytes)) = bytes else {
        return StatusCode::NOT_FOUND.into_response();
    };
    ([
        (header::CONTENT_TYPE, mime),
        (header::CACHE_CONTROL, "no-cache"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; media-src 'self' blob:; img-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    ], bytes).into_response()
}

fn random<const SIZE: usize>() -> Result<String, StatusCode> {
    let mut bytes = [0; SIZE];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hash = [0; 32];
    hash.copy_from_slice(digest::digest(&digest::SHA256, bytes).as_ref());
    hash
}
