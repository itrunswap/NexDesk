use std::{net::SocketAddr, time::Duration};

use nexdesk_core::DeviceId;
use nexdesk_directory::{
    DirectoryClient, DirectoryError, IceConfiguration, PairStep, RemoteIdentity, ServerOrigin,
};
use nexdesk_media::{ControllerOffer, IceOptions, IceServer, MediaError, View};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use crate::{
    collaboration::Capabilities,
    control::Grant,
    controller::{ControllerKey, ControllerProof},
    pairing::{
        AccessCode, ClientPairing, HTTP_BODY_LIMIT, HostIdentity, LoginChallenge,
        LoginConfirmation, PairingError, ProtectedAnswer, SignalKey,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("pairing rejected")]
    Rejected,
    #[error("the host declined the desktop request")]
    Declined,
    #[error("the host no longer permits this session to resume")]
    ResumeExpired,
    #[error("the host does not expose the desktop LAN protocol")]
    Unsupported,
    #[error("the controller did not confirm the host identity")]
    Unconfirmed,
    #[error("the host has reached its pairing or session limit")]
    Busy,
    #[error("unable to reach the LAN host")]
    Unreachable,
    #[error("the LAN host returned an invalid response")]
    InvalidResponse,
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    Media(#[from] MediaError),
}

pub struct ConnectedViewer {
    pub identity: HostIdentity,
    pub view: View,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopOffer {
    pub sdp: String,
    pub request_input: bool,
    pub resume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_proof: Option<ControllerProof>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopAnswer {
    pub sdp: String,
    pub grant: Grant,
    pub resume: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteIdentity>,
}

pub struct ConnectedDesktop {
    pub connection: ConnectedViewer,
    pub grant: Grant,
    pub resume: Zeroizing<String>,
    pub capabilities: Option<Capabilities>,
}

#[derive(Clone, Copy)]
pub struct DesktopRequest<'a> {
    pub input: bool,
    pub resume: Option<&'a str>,
    pub capabilities: Option<Capabilities>,
    pub controller: Option<&'a ControllerKey>,
}

pub async fn connect_desktop<F: Future<Output = bool>>(
    address: SocketAddr,
    code: AccessCode,
    trusted_host: Option<HostIdentity>,
    options: DesktopRequest<'_>,
    confirm_identity: impl FnOnce(HostIdentity) -> F,
    waiting_for_consent: impl FnOnce(),
) -> Result<ConnectedDesktop, SessionError> {
    let http = client()?;
    let base = format!("http://{address}");
    let (client, request) = ClientPairing::start(code)?;
    let mut capabilities = options.capabilities;
    let mut path = if capabilities.is_some() { "m3" } else { "m2" };
    let response = post(&http, &format!("{base}/{path}/pair/start"), &request, None).await;
    let challenge: LoginChallenge = match response {
        Err(SessionError::Unsupported) if capabilities.is_some() && options.resume.is_none() => {
            capabilities = None;
            path = "m2";
            post(&http, &format!("{base}/{path}/pair/start"), &request, None).await?
        }
        other => other?,
    };
    let rtc = if capabilities.is_some() {
        ControllerOffer::collaboration().await?
    } else {
        ControllerOffer::desktop().await?
    };
    let options = DesktopRequest {
        capabilities,
        ..options
    };
    let (proof, answer_key, identity) =
        protect_desktop(&rtc, client, challenge, trusted_host, options, None).await?;
    if !confirm_identity(identity).await {
        return Err(SessionError::Unconfirmed);
    }
    waiting_for_consent();
    let response: ProtectedAnswer = post(
        &http,
        &format!("{base}/{path}/pair/finish"),
        &proof,
        Some(Duration::from_secs(40)),
    )
    .await?;
    finish_desktop(rtc, answer_key, response, identity, options, None).await
}

pub async fn connect_routed<F: Future<Output = bool>>(
    origin: ServerOrigin,
    device: DeviceId,
    code: AccessCode,
    trusted_host: Option<HostIdentity>,
    options: DesktopRequest<'_>,
    confirm_identity: impl FnOnce(HostIdentity) -> F,
    waiting_for_consent: impl FnOnce(),
) -> Result<ConnectedDesktop, SessionError> {
    let remote = RemoteIdentity::new(&origin, &device);
    let directory = DirectoryClient::new(origin).map_err(directory_error)?;
    let route = directory.open(&device).await.map_err(directory_error)?;
    let (client, request) = ClientPairing::start(code)?;
    let challenge: LoginChallenge = directory
        .pair(&route, PairStep::Start, &request)
        .await
        .map_err(directory_error)?;
    let shell = options
        .capabilities
        .is_some_and(|bits| bits.includes(Capabilities::SHELL));
    let rtc = ControllerOffer::routed(ice_options(route.ice.clone()), shell).await?;
    let options = DesktopRequest {
        capabilities: Some(options.capabilities.unwrap_or_default()),
        ..options
    };
    let (proof, answer_key, identity) = protect_desktop(
        &rtc,
        client,
        challenge,
        trusted_host,
        options,
        Some(&remote),
    )
    .await?;
    if !confirm_identity(identity).await {
        return Err(SessionError::Unconfirmed);
    }
    waiting_for_consent();
    let response = directory
        .pair(&route, PairStep::Finish, &proof)
        .await
        .map_err(directory_error)?;
    finish_desktop(rtc, answer_key, response, identity, options, Some(&remote)).await
}

pub fn ice_options(ice: IceConfiguration) -> IceOptions {
    IceOptions::Routed {
        servers: ice
            .servers
            .into_iter()
            .map(|server| IceServer {
                urls: server.urls,
                username: server.username,
                password: server.credential,
            })
            .collect(),
        relay_only: ice.relay_only,
    }
}

fn directory_error(error: DirectoryError) -> SessionError {
    match error {
        DirectoryError::Offline | DirectoryError::Unreachable => SessionError::Unreachable,
        DirectoryError::Declined => SessionError::Declined,
        DirectoryError::Busy => SessionError::Busy,
        DirectoryError::Expired => SessionError::ResumeExpired,
        DirectoryError::Origin | DirectoryError::InvalidResponse => SessionError::InvalidResponse,
    }
}

async fn protect_desktop(
    rtc: &ControllerOffer,
    client: ClientPairing,
    challenge: LoginChallenge,
    trusted_host: Option<HostIdentity>,
    options: DesktopRequest<'_>,
    remote: Option<&RemoteIdentity>,
) -> Result<(LoginConfirmation, SignalKey, HostIdentity), SessionError> {
    let mut offer = DesktopOffer {
        sdp: rtc.sdp().to_owned(),
        request_input: options.input,
        resume: options.resume.map(str::to_owned),
        capabilities: options.capabilities,
        remote: remote.cloned(),
        controller_proof: None,
    };
    if let Some(controller) = options.controller {
        offer.controller_proof = Some(
            controller
                .prove(&challenge.id, &offer)
                .map_err(|_| SessionError::Rejected)?,
        );
    }
    let offer =
        Zeroizing::new(serde_json::to_string(&offer).map_err(|_| SessionError::InvalidResponse)?);
    tokio::task::spawn_blocking(move || client.confirm(challenge, trusted_host, &offer))
        .await
        .map_err(|_| SessionError::Rejected)?
        .map_err(SessionError::from)
}

async fn finish_desktop(
    rtc: ControllerOffer,
    answer_key: SignalKey,
    response: ProtectedAnswer,
    identity: HostIdentity,
    options: DesktopRequest<'_>,
    remote: Option<&RemoteIdentity>,
) -> Result<ConnectedDesktop, SessionError> {
    let clear = Zeroizing::new(answer_key.open(&response.answer)?);
    let answer: DesktopAnswer =
        serde_json::from_str(&clear).map_err(|_| SessionError::InvalidResponse)?;
    if answer.resume.len() != 43
        || answer.remote.as_ref() != remote
        || (!options.input && answer.grant == Grant::Control)
        || match (options.capabilities, answer.capabilities) {
            (Some(requested), Some(grant)) => !requested.includes(grant),
            (None, None) => false,
            _ => true,
        }
    {
        return Err(SessionError::InvalidResponse);
    }
    let view = rtc.apply_answer(&answer.sdp).await?;
    Ok(ConnectedDesktop {
        connection: ConnectedViewer { identity, view },
        grant: answer.grant,
        resume: Zeroizing::new(answer.resume),
        capabilities: answer.capabilities,
    })
}

pub async fn connect(
    address: SocketAddr,
    code: AccessCode,
    trusted_host: Option<HostIdentity>,
) -> Result<ConnectedViewer, SessionError> {
    let http = client()?;
    let (client, request) = ClientPairing::start(code)?;
    let base = format!("http://{address}");
    let challenge: LoginChallenge =
        post(&http, &format!("{base}/m1/pair/start"), &request, None).await?;
    let offer = ControllerOffer::new().await?;
    let sdp = offer.sdp().to_owned();
    let (proof, answer_key, identity) =
        tokio::task::spawn_blocking(move || client.confirm(challenge, trusted_host, &sdp))
            .await
            .map_err(|_| SessionError::Rejected)??;
    let response: ProtectedAnswer =
        post(&http, &format!("{base}/m1/pair/finish"), &proof, None).await?;
    let answer = answer_key.open(&response.answer)?;
    let view = offer.apply_answer(&answer).await?;
    Ok(ConnectedViewer { identity, view })
}

fn client() -> Result<reqwest::Client, SessionError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| SessionError::Unreachable)
}

async fn post<In: Serialize, Out: DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    body: &In,
    deadline: Option<Duration>,
) -> Result<Out, SessionError> {
    let mut request = http.post(url).json(body);
    if let Some(deadline) = deadline {
        request = request.timeout(deadline);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| SessionError::Unreachable)?;
    match response.status().as_u16() {
        200 => {}
        401 => return Err(SessionError::Rejected),
        403 => return Err(SessionError::Declined),
        404 => return Err(SessionError::Unsupported),
        410 => return Err(SessionError::ResumeExpired),
        429 | 503 => return Err(SessionError::Busy),
        _ => return Err(SessionError::InvalidResponse),
    }
    if response
        .content_length()
        .is_some_and(|length| length > HTTP_BODY_LIMIT as u64)
    {
        return Err(SessionError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SessionError::Unreachable)?
    {
        if chunk.len() > HTTP_BODY_LIMIT - bytes.len() {
            return Err(SessionError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| SessionError::InvalidResponse)
}
