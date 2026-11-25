use std::{
    env,
    fmt::Write,
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{RouteState, Service};

const OUTCOMES: [&str; 6] = [
    "ok",
    "unavailable",
    "rate_limited",
    "invalid",
    "timeout",
    "error",
];

#[derive(Default)]
pub(crate) struct Metrics {
    open: [AtomicU64; OUTCOMES.len()],
    handshake: [AtomicU64; OUTCOMES.len()],
}

impl Metrics {
    fn record(counters: &[AtomicU64; OUTCOMES.len()], status: StatusCode) {
        let index = match status {
            code if code.is_success() => 0,
            StatusCode::NOT_FOUND | StatusCode::GONE => 1,
            StatusCode::TOO_MANY_REQUESTS => 2,
            code if code.is_client_error() => 3,
            StatusCode::GATEWAY_TIMEOUT => 4,
            _ => 5,
        };
        counters[index].fetch_add(1, Relaxed);
    }
}

pub(crate) async fn open(
    State(service): State<Arc<Service>>,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    Metrics::record(&service.metrics.open, response.status());
    response
}

pub(crate) async fn handshake(
    State(service): State<Arc<Service>>,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    Metrics::record(&service.metrics.handshake, response.status());
    response
}

pub(crate) async fn start(service: Arc<Service>) -> io::Result<Option<SocketAddr>> {
    let Some(value) = env::var("NEXDESK_METRICS_LISTEN")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let listen = value.parse::<SocketAddr>().map_err(io::Error::other)?;
    if !listen.ip().is_loopback() {
        return Err(io::Error::other("metrics must bind a loopback address"));
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    let app = Router::new()
        .route("/metrics", get(scrape))
        .with_state(service);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!(
                "{}",
                serde_json::json!({"event":"metrics_error","kind":format!("{:?}", error.kind())})
            );
        }
    });
    Ok(Some(address))
}

async fn scrape(State(service): State<Arc<Service>>) -> Response {
    let (devices, pending, ready) = {
        let Ok(live) = service.live.lock() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let now = Instant::now();
        let mut counts = [0, 0];
        for route in live.routes.values().filter(|route| route.expires > now) {
            counts[usize::from(route.state == RouteState::Ready)] += 1;
        }
        (live.devices.len(), counts[0], counts[1])
    };
    let mut body = format!(
        "# TYPE nexdesk_directory_online_devices gauge\nnexdesk_directory_online_devices {devices}\n\
         # TYPE nexdesk_directory_routes gauge\nnexdesk_directory_routes{{state=\"pending\"}} {pending}\n\
         nexdesk_directory_routes{{state=\"ready\"}} {ready}\n\
         # TYPE nexdesk_directory_requests_total counter\n"
    );
    for (operation, counters) in [
        ("open", &service.metrics.open),
        ("handshake", &service.metrics.handshake),
    ] {
        for (outcome, count) in OUTCOMES.iter().zip(counters) {
            let _ = writeln!(
                body,
                "nexdesk_directory_requests_total{{operation=\"{operation}\",outcome=\"{outcome}\"}} {}",
                count.load(Relaxed)
            );
        }
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}
