use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use nexdesk_directory::ServerOrigin;
use nexdesk_media::{HostMedia, HostPeer, MediaConfig};
use nexdesk_session::pairing::{
    AccessCode, HTTP_BODY_LIMIT, HostPairing, LoginChallenge, LoginConfirmation, LoginRequest,
    PendingLogin, ProtectedAnswer,
};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
};

use crate::{desktop_host::DesktopHost, desktop_session};

const MAX_PEERS: usize = 4;
const MAX_STARTS: usize = 8;
const MAX_GLOBAL_STARTS: usize = 32;
const MAX_CHALLENGES: usize = 16;
const WINDOW: Duration = Duration::from_secs(30);
const SESSION_DEADLINE: Duration = Duration::from_secs(10 * 60);

pub struct Options {
    pub listen: SocketAddr,
    pub browser: bool,
    pub media: MediaConfig,
    pub desktop: bool,
    pub volatile: bool,
    pub managed: bool,
    pub share_root: Option<std::path::PathBuf>,
    pub directory: Option<ServerOrigin>,
    pub allow_shell: bool,
}

pub(crate) struct Host {
    pub(crate) media: HostMedia,
    pairing: Mutex<HostPairing>,
    directory_pairing: Option<Mutex<HostPairing>>,
    pub(crate) allow_shell: AtomicBool,
    admission: Mutex<Admission>,
    slots: Arc<Semaphore>,
    pub(crate) shutdown: watch::Receiver<bool>,
    browser: Option<BrowserGrant>,
    pub(crate) desktop: Option<Arc<DesktopHost>>,
}

struct BrowserGrant {
    origin: String,
    token: Mutex<Option<[u8; 32]>>,
    issued: Instant,
}

#[derive(Default)]
struct Admission {
    starts: VecDeque<(IpAddr, Instant)>,
    pending: HashMap<String, Challenge>,
}

struct Challenge {
    ip: IpAddr,
    flow: Option<String>,
    started: Instant,
    login: PendingLogin,
}

impl Admission {
    fn start(&mut self, ip: IpAddr, now: Instant) -> Result<(), StatusCode> {
        self.starts
            .retain(|(_, at)| now.saturating_duration_since(*at) < WINDOW);
        self.pending
            .retain(|_, challenge| now.saturating_duration_since(challenge.started) < WINDOW);
        let same_ip = self
            .starts
            .iter()
            .filter(|(source, _)| *source == ip)
            .count();
        if self.pending.len() >= MAX_CHALLENGES
            || self.starts.len() >= MAX_GLOBAL_STARTS
            || same_ip >= MAX_STARTS
        {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        self.starts.push_back((ip, now));
        Ok(())
    }

    fn take(
        &mut self,
        id: &str,
        ip: IpAddr,
        flow: Option<&str>,
        now: Instant,
    ) -> Option<PendingLogin> {
        let challenge = self.pending.get(id)?;
        if challenge.ip != ip || challenge.flow.as_deref() != flow {
            return None;
        }
        let challenge = self.pending.remove(id)?;
        (now.saturating_duration_since(challenge.started) < WINDOW).then_some(challenge.login)
    }
}

pub async fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let listener = if options.managed {
        None
    } else {
        Some(TcpListener::bind(options.listen).await?)
    };
    let address = listener.as_ref().map(TcpListener::local_addr).transpose()?;
    let media = HostMedia::new(options.media)?;
    let persistent = options.desktop && !options.volatile;
    let allow_shell = options.allow_shell && crate::shell::ordinary_user();
    let uses_directory = options.managed || options.directory.is_some();
    let ((code, pairing), directory_pairing) =
        tokio::task::spawn_blocking(move || -> Result<_, nexdesk_store::StoreError> {
            fn temporary() -> Result<(AccessCode, HostPairing), nexdesk_store::StoreError> {
                let code = AccessCode::generate()?;
                let pairing = HostPairing::enroll(&code)?;
                Ok((code, pairing))
            }
            let lan = if persistent {
                nexdesk_store::local_host()?
            } else {
                temporary()?
            };
            let directory = if uses_directory {
                Some(if persistent {
                    nexdesk_store::directory_host()?
                } else {
                    temporary()?
                })
            } else {
                None
            };
            Ok((lan, directory))
        })
        .await??;
    let (shutdown_sender, shutdown) = watch::channel(false);
    let mut invitation = None;
    let browser = if options.browser {
        let address = address.ok_or_else(|| io::Error::other("missing preview listener"))?;
        let origin = format!("http://{address}");
        let mut token = [0; 32];
        SystemRandom::new()
            .fill(&mut token)
            .map_err(|_| io::Error::other("unable to create browser invitation"))?;
        invitation = Some(format!("{origin}/#{}", URL_SAFE_NO_PAD.encode(token)));
        Some(BrowserGrant {
            origin,
            token: Mutex::new(Some(sha256(&token))),
            issued: Instant::now(),
        })
    } else {
        None
    };
    // Startup details go only to the foreground operator. Session logs below omit credentials.
    let desktop = if options.desktop {
        let share = if let Some(path) = options.share_root {
            Some(Arc::new(
                tokio::task::spawn_blocking(move || nexdesk_files::Root::open(&path)).await??,
            ))
        } else {
            None
        };
        let desktop = DesktopHost::new(options.media.capture, persistent, share);
        desktop.ready(
            address,
            &pairing,
            &code,
            directory_pairing.as_ref(),
            allow_shell,
        );
        Some(desktop)
    } else {
        let address = address.ok_or_else(|| io::Error::other("missing preview listener"))?;
        println!("LAN {address}");
        println!("Identity {}", pairing.identity());
        println!("Pairing code {}", std::str::from_utf8(code.as_bytes())?);
        if let Some(url) = invitation {
            println!("Local browser invitation (single-use) {url}");
        }
        None
    };
    drop(code);
    let host = Arc::new(Host {
        media,
        pairing: Mutex::new(pairing),
        directory_pairing: directory_pairing.map(|(_, pairing)| Mutex::new(pairing)),
        allow_shell: AtomicBool::new(allow_shell),
        admission: Mutex::new(Admission::default()),
        slots: Arc::new(Semaphore::new(MAX_PEERS)),
        shutdown,
        browser,
        desktop,
    });
    if let Some(desktop) = host.desktop.clone() {
        desktop.listen_local(host.clone(), shutdown_sender.clone(), options.managed);
    }
    if !options.managed
        && let Some(origin) = options.directory
    {
        tokio::spawn(crate::directory_host::run(
            host.clone(),
            origin,
            host.shutdown.clone(),
            None,
        ));
    }
    let shutting_down = shutdown_sender.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutting_down.send(true);
        }
    });
    let result = if let Some(listener) = listener {
        let closed = host.shutdown.clone();
        serve_http(listener, host, closed).await
    } else {
        let mut ended = host.shutdown.clone();
        let _ = ended.wait_for(|closed| *closed).await;
        Ok(())
    };
    signal.abort();
    let _ = shutdown_sender.send(true);
    result?;
    Ok(())
}

pub(crate) async fn serve_http(
    listener: TcpListener,
    host: Arc<Host>,
    mut closed: watch::Receiver<bool>,
) -> io::Result<()> {
    let app = Router::new()
        .route("/", get(browser_page))
        .route("/viewer.js", get(browser_script))
        .route("/m1/pair/start", post(login_start_legacy))
        .route("/m1/pair/finish", post(login_finish))
        .route("/m1/browser/offer", post(browser_offer))
        .route("/m2/pair/start", post(login_start_desktop))
        .route("/m2/pair/finish", post(desktop_session::finish))
        .route("/m3/pair/start", post(login_start_desktop))
        .route("/m3/pair/finish", post(desktop_session::finish))
        .layer(DefaultBodyLimit::max(HTTP_BODY_LIMIT))
        .layer(Extension(closed.clone()))
        .with_state(host);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = closed.wait_for(|closed| *closed).await;
    })
    .await
}

async fn login_start_legacy(
    State(host): State<Arc<Host>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginChallenge>, StatusCode> {
    if host.desktop.is_some() {
        return Err(StatusCode::NOT_FOUND);
    }
    login_start(&host, source, headers, request)
}

async fn login_start_desktop(
    State(host): State<Arc<Host>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Extension(closed): Extension<watch::Receiver<bool>>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginChallenge>, StatusCode> {
    if *closed.borrow() || closed.has_changed().is_err() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if host.desktop.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    login_start(&host, source, headers, request)
}

fn login_start(
    host: &Host,
    source: SocketAddr,
    headers: HeaderMap,
    request: LoginRequest,
) -> Result<Json<LoginChallenge>, StatusCode> {
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    host.start_challenge(source.ip().to_canonical(), None, request)
        .map(Json)
}

impl Host {
    pub(crate) fn clear_challenges(&self, directory: bool) {
        if let Ok(mut admission) = self.admission.lock() {
            admission
                .pending
                .retain(|_, challenge| challenge.flow.is_some() != directory);
        }
    }

    pub(crate) fn start_challenge(
        &self,
        ip: IpAddr,
        flow: Option<String>,
        request: LoginRequest,
    ) -> Result<LoginChallenge, StatusCode> {
        let now = Instant::now();
        let mut admission = self
            .admission
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        admission.start(ip, now)?;
        let pairing = if flow.is_some() {
            self.directory_pairing
                .as_ref()
                .ok_or(StatusCode::NOT_FOUND)?
        } else {
            &self.pairing
        };
        let (login, reply) = pairing
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .start(request)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        admission.pending.insert(
            reply.id.clone(),
            Challenge {
                ip,
                flow,
                started: now,
                login,
            },
        );
        Ok(reply)
    }
}

async fn login_finish(
    State(host): State<Arc<Host>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginConfirmation>,
) -> Result<Json<ProtectedAnswer>, StatusCode> {
    if host.desktop.is_some() {
        return Err(StatusCode::NOT_FOUND);
    }
    let pending = host.pending(&headers, source, &request.id)?;
    let (offer, key) = pending
        .accept(request)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let (peer, answer, slot) = create_peer(&host, &offer).await?;
    let answer = key
        .seal(&answer)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    spawn_peer(peer, slot, host.shutdown.clone());
    Ok(Json(ProtectedAnswer { answer }))
}

impl Host {
    pub(crate) fn pending(
        &self,
        headers: &HeaderMap,
        source: SocketAddr,
        id: &str,
    ) -> Result<PendingLogin, StatusCode> {
        if headers.contains_key(header::ORIGIN) {
            return Err(StatusCode::FORBIDDEN);
        }
        self.take_challenge(source.ip().to_canonical(), None, id)
    }

    pub(crate) fn take_challenge(
        &self,
        ip: IpAddr,
        flow: Option<&str>,
        id: &str,
    ) -> Result<PendingLogin, StatusCode> {
        self.admission
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .take(id, ip, flow, Instant::now())
            .ok_or(StatusCode::UNAUTHORIZED)
    }

    pub(crate) fn directory_key(
        &self,
        origin: &ServerOrigin,
    ) -> Option<(ring::signature::Ed25519KeyPair, String)> {
        let pairing = self.directory_pairing.as_ref()?.lock().ok()?;
        let setup = pairing.export_setup();
        let purpose = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &setup);
        let seed = ring::hmac::sign(
            &purpose,
            format!("nexdesk/directory/identity/v1\n{origin}").as_bytes(),
        );
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed.as_ref()).ok()?;
        Some((key, pairing.identity().to_string()))
    }

    pub(crate) fn rotate(
        &self,
        persist: bool,
        directory: bool,
        chosen: Option<AccessCode>,
    ) -> Result<AccessCode, nexdesk_store::StoreError> {
        use nexdesk_store::StoreError;
        let code = match chosen {
            Some(code) => code,
            None => AccessCode::generate()?,
        };
        let mut admission = self.admission.lock().map_err(|_| StoreError::Invalid)?;
        let pairing = if directory {
            self.directory_pairing.as_ref().ok_or(StoreError::Invalid)?
        } else {
            &self.pairing
        };
        let mut current = pairing.lock().map_err(|_| StoreError::Invalid)?;
        let pairing = HostPairing::from_setup(&code, &current.export_setup())?;
        if persist {
            if directory {
                nexdesk_store::save_directory_host(&code, &pairing)?;
            } else {
                nexdesk_store::save_local_host(&code, &pairing)?;
            }
        }
        admission
            .pending
            .retain(|_, challenge| challenge.flow.is_some() != directory);
        *current = pairing;
        Ok(code)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserOffer {
    sdp: String,
}

#[derive(Serialize)]
struct BrowserAnswer {
    sdp: String,
}

async fn browser_offer(
    State(host): State<Arc<Host>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<BrowserOffer>,
) -> Result<Json<BrowserAnswer>, StatusCode> {
    let grant = host.browser.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if !source.ip().is_loopback() || origin != Some(grant.origin.as_str()) {
        return Err(StatusCode::FORBIDDEN);
    }
    grant.accept(&headers)?;
    let (peer, sdp, slot) = create_peer(&host, &request.sdp).await?;
    spawn_peer(peer, slot, host.shutdown.clone());
    Ok(Json(BrowserAnswer { sdp }))
}

impl BrowserGrant {
    fn accept(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        let raw = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| value.len() == 43)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let supplied = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let mut grant = self
            .token
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let expected = grant.as_ref().ok_or(StatusCode::UNAUTHORIZED)?;
        if self.issued.elapsed() > SESSION_DEADLINE || sha256(&supplied) != *expected {
            return Err(StatusCode::UNAUTHORIZED);
        }
        *grant = None;
        Ok(())
    }
}

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut hash = [0; 32];
    hash.copy_from_slice(digest::digest(&digest::SHA256, input).as_ref());
    hash
}

async fn create_peer(
    host: &Host,
    offer: &str,
) -> Result<(HostPeer, String, OwnedSemaphorePermit), StatusCode> {
    let slot = host
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let (peer, answer) = tokio::time::timeout(Duration::from_secs(8), host.media.answer(offer))
        .await
        .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok((peer, answer, slot))
}

fn spawn_peer(peer: HostPeer, slot: OwnedSemaphorePermit, mut shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        let _slot = slot;
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = tick.tick() => {
                    let capture = peer.status();
                    if peer.has_ended() || capture.unavailable || started.elapsed() > SESSION_DEADLINE {
                        break;
                    }
                    if let Ok(Ok(media)) = tokio::time::timeout(Duration::from_secs(1), peer.stats()).await {
                        eprintln!("{}", serde_json::json!({ "capture": capture, "media": media }));
                    }
                }
            }
        }
    });
}

async fn browser_page(State(host): State<Arc<Host>>) -> Response {
    if host.browser.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'"),
        ],
        Html(include_str!("../web/index.html")),
    ).into_response()
}

async fn browser_script(State(host): State<Arc<Host>>) -> Response {
    if host.browser.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        include_str!("../web/viewer.js"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexdesk_session::pairing::ClientPairing;
    use std::net::Ipv4Addr;

    #[test]
    fn source_budget_refills_after_its_window() {
        let mut gate = Admission::default();
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let start = Instant::now();
        for _ in 0..MAX_STARTS {
            assert_eq!(gate.start(address, start), Ok(()));
        }
        assert_eq!(
            gate.start(address, start),
            Err(StatusCode::TOO_MANY_REQUESTS)
        );
        assert_eq!(gate.start(address, start + WINDOW), Ok(()));
    }

    #[test]
    fn challenge_is_source_bound_single_use_and_expires() -> Result<(), Box<dyn std::error::Error>>
    {
        let host = HostPairing::enroll(&AccessCode::parse(b"Z5Ta9p")?)?;
        let mut gate = Admission::default();
        let start = Instant::now();
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let (_, request) = ClientPairing::start(AccessCode::parse(b"Z5Ta9p")?)?;
        let (login, challenge) = host.start(request)?;
        gate.pending.insert(
            challenge.id.clone(),
            Challenge {
                ip: address,
                flow: None,
                started: start,
                login,
            },
        );
        let another_source = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
        assert!(
            gate.take(&challenge.id, another_source, None, start)
                .is_none()
        );
        assert!(
            gate.take(&challenge.id, address, Some("different flow"), start)
                .is_none()
        );
        assert!(gate.take(&challenge.id, address, None, start).is_some());
        assert!(gate.take(&challenge.id, address, None, start).is_none());

        let (_, request) = ClientPairing::start(AccessCode::parse(b"Z5Ta9p")?)?;
        let (login, challenge) = host.start(request)?;
        gate.pending.insert(
            challenge.id.clone(),
            Challenge {
                ip: address,
                flow: None,
                started: start,
                login,
            },
        );
        assert!(
            gate.take(&challenge.id, address, None, start + WINDOW)
                .is_none()
        );
        Ok(())
    }
}
