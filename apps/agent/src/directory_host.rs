use std::{io, sync::Arc, time::Duration};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use nexdesk_directory::{
    FromHost, PROTOCOL_VERSION, PairStep, Registration, RemoteIdentity, RoutedRequest,
    SOCKET_LIMIT, ServerOrigin, ToHost, registration_message, token_bytes,
};
use nexdesk_session::pairing::{LoginConfirmation, LoginRequest};
use ring::signature::KeyPair;
use serde_json::{Value, json};
use tokio::{
    sync::watch,
    task::JoinSet,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::{
    desktop_host::report,
    desktop_session::{DirectorySession, finish_session},
    server::Host,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

// The directory sends a heartbeat every 10 seconds. A silent TCP connection
// must not keep the receiver registered locally after that traffic stops.
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn run(
    host: Arc<Host>,
    origin: ServerOrigin,
    mut shutdown: watch::Receiver<bool>,
    generation: Option<u64>,
) {
    if generation.is_some() {
        status(&origin, generation, "connecting", None);
    }
    let mut delay = Duration::from_secs(1);
    while !*shutdown.borrow() {
        let outcome = tokio::select! {
            biased;
            _ = shutdown.wait_for(|value| *value) => return,
            result = connect(&host, &origin, generation) => result,
        };
        match outcome {
            Ok(LinkExit::Revoked) => {
                status(&origin, generation, "revoked", None);
                return;
            }
            Ok(LinkExit::Disconnected) => delay = Duration::from_secs(1),
            Err(_) => {}
        }
        status(&origin, generation, "offline", None);
        tokio::select! {
            _ = shutdown.wait_for(|value| *value) => return,
            _ = sleep(delay) => {}
        }
        delay = (delay * 2).min(Duration::from_secs(20));
    }
}

fn status(origin: &ServerOrigin, generation: Option<u64>, value: &str, device: Option<&str>) {
    report(json!({
        "event":"directory",
        "status":value,
        "origin":origin.as_str(),
        "generation":generation,
        "device":device,
    }));
}

enum LinkExit {
    Disconnected,
    Revoked,
}

async fn connect(
    host: &Arc<Host>,
    origin: &ServerOrigin,
    generation: Option<u64>,
) -> Result<LinkExit> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(SOCKET_LIMIT))
        .max_frame_size(Some(SOCKET_LIMIT));
    let (mut socket, _) = timeout(
        Duration::from_secs(10),
        connect_async_with_config(origin.websocket(), Some(config), true),
    )
    .await??;
    let ToHost::Challenge { nonce } = read_registration(&mut socket).await? else {
        return Err(io::Error::other("invalid directory challenge").into());
    };
    if token_bytes::<32>(&nonce).is_none() {
        return Err(io::Error::other("invalid directory challenge").into());
    }
    let (key, identity) = host
        .directory_key(origin)
        .ok_or_else(|| io::Error::other("device identity unavailable"))?;
    let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let signature = key.sign(&registration_message(origin, &nonce, &public, &identity));
    write(
        &mut socket,
        FromHost::Register(Registration {
            version: PROTOCOL_VERSION,
            identity,
            key: public,
            signature: URL_SAFE_NO_PAD.encode(signature.as_ref()),
            enrollment: std::env::var("NEXDESK_ENROLLMENT_KEY")
                .ok()
                .filter(|key| !key.is_empty()),
        }),
    )
    .await?;
    let device = match read_registration(&mut socket).await? {
        ToHost::Registered { device } => device.parse()?,
        ToHost::Revoked => return Ok(LinkExit::Revoked),
        _ => return Err(io::Error::other("invalid directory registration").into()),
    };
    let identity = RemoteIdentity::new(origin, &device);
    status(origin, generation, "online", Some(&identity.device));
    serve(socket, host, identity).await
}

async fn read_registration(socket: &mut Socket) -> Result<ToHost> {
    let next = timeout(Duration::from_secs(10), socket.next()).await?;
    let Some(Ok(Message::Text(text))) = next else {
        return Err(io::Error::other("directory closed the registration connection").into());
    };
    Ok(serde_json::from_str(&text)?)
}

async fn write(socket: &mut Socket, message: FromHost) -> Result<()> {
    let json = serde_json::to_string(&message)?;
    if json.len() > SOCKET_LIMIT {
        return Err(io::Error::other("directory message exceeds limit").into());
    }
    timeout(
        SOCKET_WRITE_TIMEOUT,
        socket.send(Message::Text(json.into())),
    )
    .await??;
    Ok(())
}

async fn serve(mut socket: Socket, host: &Arc<Host>, identity: RemoteIdentity) -> Result<LinkExit> {
    let (close, protection) = watch::channel(false);
    let mut calls = JoinSet::new();
    let idle = sleep(LINK_IDLE_TIMEOUT);
    tokio::pin!(idle);
    let result = loop {
        tokio::select! {
            _ = &mut idle => break LinkExit::Disconnected,
            request = socket.next() => {
                let Some(Ok(message)) = request else { break LinkExit::Disconnected };
                idle.as_mut().reset(Instant::now() + LINK_IDLE_TIMEOUT);
                match message {
                    Message::Text(text) => match serde_json::from_str::<ToHost>(&text)? {
                        ToHost::Revoked => break LinkExit::Revoked,
                        ToHost::Request(request) if calls.len() < 8 => {
                            calls.spawn(handle(host.clone(), identity.clone(), protection.clone(), request));
                        }
                        ToHost::Request(request) => {
                            write(&mut socket, reply(request.call, StatusCode::SERVICE_UNAVAILABLE, Value::Null)).await?;
                        }
                        _ => break LinkExit::Disconnected,
                    },
                    Message::Ping(_) => {
                        // Tungstenite has queued the matching pong while reading the ping.
                        timeout(SOCKET_WRITE_TIMEOUT, socket.flush()).await??;
                    }
                    Message::Pong(_) => {}
                    _ => break LinkExit::Disconnected,
                }
            }
            completed = calls.join_next(), if !calls.is_empty() => {
                if let Some(Ok(response)) = completed { write(&mut socket, response).await?; }
            }
        }
    };
    let _ = close.send(true);
    Ok(result)
}

async fn handle(
    host: Arc<Host>,
    identity: RemoteIdentity,
    closed: watch::Receiver<bool>,
    request: RoutedRequest,
) -> FromHost {
    let call = request.call;
    let result = if token_bytes::<32>(&request.flow).is_none() || !request.ice.valid() {
        Err(StatusCode::BAD_REQUEST)
    } else {
        handle_pair(host, identity, closed, request).await
    };
    match result {
        Ok(value) => reply(call, StatusCode::OK, value),
        Err(status) => reply(call, status, Value::Null),
    }
}

async fn handle_pair(
    host: Arc<Host>,
    identity: RemoteIdentity,
    closed: watch::Receiver<bool>,
    request: RoutedRequest,
) -> std::result::Result<Value, StatusCode> {
    let value = match request.step {
        PairStep::Start => {
            let message: LoginRequest =
                serde_json::from_value(request.body).map_err(|_| StatusCode::BAD_REQUEST)?;
            let challenge = host.start_challenge(request.source, Some(request.flow), message)?;
            serde_json::to_value(challenge)
        }
        PairStep::Finish => {
            let message: LoginConfirmation =
                serde_json::from_value(request.body).map_err(|_| StatusCode::BAD_REQUEST)?;
            let directory = DirectorySession {
                remote: identity,
                flow: request.flow,
                ice: request.ice,
            };
            let answer =
                finish_session(host, request.source, message, true, Some(directory), closed)
                    .await?;
            serde_json::to_value(answer)
        }
    };
    value.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn reply(call: u64, status: StatusCode, body: Value) -> FromHost {
    FromHost::Reply {
        call,
        status: status.as_u16(),
        body,
    }
}
