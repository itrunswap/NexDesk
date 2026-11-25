#![forbid(unsafe_code)]

use std::{fmt, net::IpAddr, str::FromStr, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use nexdesk_core::DeviceId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 1;
pub const BODY_LIMIT: usize = 80 * 1024;
pub const SOCKET_LIMIT: usize = BODY_LIMIT + 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerOrigin(String);

impl FromStr for ServerOrigin {
    type Err = DirectoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.len() > 512 || value.chars().any(char::is_control) {
            return Err(DirectoryError::Origin);
        }
        let url = reqwest::Url::parse(value).map_err(|_| DirectoryError::Origin)?;
        let Some(host) = url.host_str() else {
            return Err(DirectoryError::Origin);
        };
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(DirectoryError::Origin);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }
}

impl fmt::Display for ServerOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl ServerOrigin {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn websocket(&self) -> String {
        format!("{}/api/v1/host", self.0.replacen("http", "ws", 1))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("enter an HTTPS directory origin, or an HTTP loopback origin for development")]
    Origin,
    #[error("unable to reach the device directory")]
    Unreachable,
    #[error("this device is offline or unavailable")]
    Offline,
    #[error("the directory declined the request")]
    Declined,
    #[error("the directory or device is busy")]
    Busy,
    #[error("the connection request expired")]
    Expired,
    #[error("the directory returned an invalid response")]
    InvalidResponse,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIdentity {
    pub origin: String,
    pub device: String,
}

impl RemoteIdentity {
    pub fn new(origin: &ServerOrigin, device: &DeviceId) -> Self {
        Self {
            origin: origin.to_string(),
            device: device.to_string(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IceConfiguration {
    pub servers: Vec<IceServer>,
    #[serde(default)]
    pub relay_only: bool,
}

impl IceConfiguration {
    pub fn valid(&self) -> bool {
        self.servers.len() <= 8
            && self.servers.iter().all(|server| {
                (1..=4).contains(&server.urls.len())
                    && server.urls.iter().all(|url| valid_ice_url(url))
                    && server.username.len() <= 256
                    && server.credential.len() <= 256
            })
            && (!self.relay_only
                || self.servers.iter().any(|server| {
                    server
                        .urls
                        .iter()
                        .any(|url| url.starts_with("turn:") || url.starts_with("turns:"))
                }))
    }
}

pub fn valid_ice_url(value: &str) -> bool {
    value.len() <= 256
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && value.split_once(':').is_some_and(|(scheme, endpoint)| {
            matches!(scheme, "stun" | "stuns" | "turn" | "turns")
                && !endpoint.is_empty()
                && !endpoint.contains(['/', '@', '#', '\\'])
        })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub version: u16,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePresence {
    pub online: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRoute {
    pub id: String,
    pub ice: IceConfiguration,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub version: u16,
    pub identity: String,
    pub key: String,
    pub signature: String,
    pub enrollment: Option<String>,
}

pub fn registration_message(
    origin: &ServerOrigin,
    nonce: &str,
    key: &str,
    identity: &str,
) -> Vec<u8> {
    format!("nexdesk/directory/register/v1\n{origin}\n{nonce}\n{key}\n{identity}").into_bytes()
}

pub fn token_bytes<const SIZE: usize>(encoded: &str) -> Option<[u8; SIZE]> {
    if encoded.len() != (SIZE * 4).div_ceil(3) {
        return None;
    }
    URL_SAFE_NO_PAD.decode(encoded).ok()?.try_into().ok()
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairStep {
    Start,
    Finish,
}

impl PairStep {
    fn path(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Finish => "finish",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedRequest {
    pub call: u64,
    pub flow: String,
    pub source: IpAddr,
    pub step: PairStep,
    pub ice: IceConfiguration,
    pub body: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToHost {
    Challenge { nonce: String },
    Registered { device: String },
    Request(RoutedRequest),
    Revoked,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FromHost {
    Register(Registration),
    Reply { call: u64, status: u16, body: Value },
}

pub struct DirectoryClient {
    origin: ServerOrigin,
    http: reqwest::Client,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryHealth {
    Ready,
    Unreachable,
    Timeout,
    Tls,
    InvalidResponse,
    Incompatible,
}

impl DirectoryClient {
    pub fn new(origin: ServerOrigin) -> Result<Self, DirectoryError> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| DirectoryError::Unreachable)?;
        Ok(Self { origin, http })
    }

    pub async fn health(&self) -> DirectoryHealth {
        #[derive(Deserialize)]
        struct Health {
            ok: bool,
            protocol: u16,
        }

        let response = self
            .http
            .get(format!("{}/healthz", self.origin))
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) if error.is_timeout() => return DirectoryHealth::Timeout,
            Err(error) => {
                let mut cause = std::error::Error::source(&error);
                while let Some(error) = cause {
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::InvalidData)
                    {
                        return DirectoryHealth::Tls;
                    }
                    cause = error.source();
                }
                return DirectoryHealth::Unreachable;
            }
        };
        match read_json::<Health>(response).await {
            Ok(Health { ok: true, protocol }) if protocol == PROTOCOL_VERSION => {
                DirectoryHealth::Ready
            }
            Ok(Health { ok: true, .. }) => DirectoryHealth::Incompatible,
            _ => DirectoryHealth::InvalidResponse,
        }
    }

    pub async fn open(&self, device: &DeviceId) -> Result<SessionRoute, DirectoryError> {
        let route: SessionRoute = self
            .post(
                &format!("/api/v1/devices/{device}/session"),
                &OpenRequest {
                    version: PROTOCOL_VERSION,
                },
                10,
            )
            .await?;
        if token_bytes::<32>(&route.id).is_none() || !route.ice.valid() {
            return Err(DirectoryError::InvalidResponse);
        }
        Ok(route)
    }

    pub async fn presence(&self, device: &DeviceId) -> Result<DevicePresence, DirectoryError> {
        let response = self
            .http
            .get(format!("{}/api/v1/devices/{device}/presence", self.origin))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| DirectoryError::Unreachable)?;
        read_json(response).await
    }

    pub async fn pair<T: Serialize, R: DeserializeOwned>(
        &self,
        route: &SessionRoute,
        step: PairStep,
        body: &T,
    ) -> Result<R, DirectoryError> {
        if token_bytes::<32>(&route.id).is_none() {
            return Err(DirectoryError::InvalidResponse);
        }
        let timeout = match step {
            PairStep::Start => 10,
            PairStep::Finish => 45,
        };
        self.post(
            &format!("/api/v1/sessions/{}/{}", route.id, step.path()),
            body,
            timeout,
        )
        .await
    }

    async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
        timeout: u64,
    ) -> Result<R, DirectoryError> {
        let response = self
            .http
            .post(format!("{}{path}", self.origin))
            .json(body)
            .timeout(Duration::from_secs(timeout))
            .send()
            .await
            .map_err(|_| DirectoryError::Unreachable)?;
        read_json(response).await
    }
}

async fn read_json<R: DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<R, DirectoryError> {
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(DirectoryError::Declined),
        404 => return Err(DirectoryError::Offline),
        410 => return Err(DirectoryError::Expired),
        409 | 429 | 503 => return Err(DirectoryError::Busy),
        _ => return Err(DirectoryError::InvalidResponse),
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DirectoryError::Unreachable)?
    {
        if chunk.len() > BODY_LIMIT - bytes.len() {
            return Err(DirectoryError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| DirectoryError::InvalidResponse)
}
