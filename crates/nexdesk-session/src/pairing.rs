//! M1 LAN pairing and authenticated, single-use SDP messages.

use argon2::Argon2;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64};
use opaque_ke::{
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, Ristretto255, ServerLogin, ServerLoginParameters, ServerRegistration,
    ServerSetup, TripleDh,
};
use rand::rngs::OsRng;
use ring::{aead, digest, hkdf, rand::SecureRandom};
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use zeroize::Zeroizing;

const CREDENTIAL: &[u8] = b"nexdesk/lan/viewer";
const CONTEXT: &[u8] = b"nexdesk/lan/opaque/v1";
const CHALLENGE_LEN: usize = 16;
const OPAQUE_MESSAGE_LIMIT: usize = 1024;
const NONCE_LEN: usize = 12;
pub const SDP_LIMIT: usize = 48 * 1024;
pub const HTTP_BODY_LIMIT: usize = 80 * 1024;

struct Suite;

impl CipherSuite for Suite {
    type OprfCs = Ristretto255;
    type KeyExchange = TripleDh<Ristretto255, Sha512>;
    type Ksf = Argon2<'static>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PairingError {
    #[error("enter six ASCII letters or digits")]
    InvalidCode,
    #[error("peer identity did not match the trusted identity")]
    IdentityChanged,
    #[error("pairing rejected")]
    Rejected,
    #[error("invalid or unauthenticated session description")]
    InvalidSignal,
    #[error("could not generate secure randomness")]
    Randomness,
}

/// Six case-sensitive ASCII letters or digits. The value is cleared on drop.
pub struct AccessCode(Zeroizing<[u8; 6]>);

impl AccessCode {
    pub fn parse(code: &[u8]) -> Result<Self, PairingError> {
        if code.len() != 6 || !code.iter().all(u8::is_ascii_alphanumeric) {
            return Err(PairingError::InvalidCode);
        }
        let mut value = Zeroizing::new([0; 6]);
        value.copy_from_slice(code);
        Ok(Self(value))
    }

    pub fn generate() -> Result<Self, PairingError> {
        const ALPHABET: &[u8; 62] =
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let mut value = Zeroizing::new([0; 6]);
        let random = ring::rand::SystemRandom::new();
        for character in value.iter_mut() {
            loop {
                let mut byte = [0];
                random
                    .fill(&mut byte)
                    .map_err(|_| PairingError::Randomness)?;
                if byte[0] < 248 {
                    *character = ALPHABET[usize::from(byte[0]) % ALPHABET.len()];
                    break;
                }
            }
        }
        Ok(Self(value))
    }

    /// Only for displaying the current code in the host's local terminal.
    pub fn as_bytes(&self) -> &[u8] {
        &*self.0
    }
}

/// A SHA-256 digest of the OPAQUE authenticated host public key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostIdentity([u8; 32]);

impl HostIdentity {
    fn of(public_key: &[u8]) -> Self {
        let mut identity = [0; 32];
        identity.copy_from_slice(digest::digest(&digest::SHA256, public_key).as_ref());
        Self(identity)
    }
}

impl std::fmt::Display for HostIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&BASE64.encode(self.0))
    }
}

impl std::str::FromStr for HostIdentity {
    type Err = PairingError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let identity = decode(text.trim(), 32)?
            .try_into()
            .map_err(|_| PairingError::IdentityChanged)?;
        Ok(Self(identity))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginChallenge {
    pub id: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginConfirmation {
    pub id: String,
    pub message: String,
    pub offer: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedAnswer {
    pub answer: String,
}

/// Holds the locally enrolled credential record. Registration is never exposed over LAN.
pub struct HostPairing {
    setup: ServerSetup<Suite>,
    credential: ServerRegistration<Suite>,
    identity: HostIdentity,
}

pub struct PendingLogin {
    login: ServerLogin<Suite>,
    id: [u8; CHALLENGE_LEN],
}

impl HostPairing {
    pub fn enroll(code: &AccessCode) -> Result<Self, PairingError> {
        let mut rng = OsRng;
        Self::register(code, ServerSetup::new(&mut rng))
    }

    pub fn from_setup(code: &AccessCode, bytes: &[u8]) -> Result<Self, PairingError> {
        let setup = ServerSetup::deserialize(bytes).map_err(|_| PairingError::Rejected)?;
        Self::register(code, setup)
    }

    pub fn export_setup(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.setup.serialize().to_vec())
    }

    fn register(code: &AccessCode, setup: ServerSetup<Suite>) -> Result<Self, PairingError> {
        let mut rng = OsRng;
        let client = ClientRegistration::<Suite>::start(&mut rng, code.as_bytes())
            .map_err(|_| PairingError::Rejected)?;
        let server = ServerRegistration::start(&setup, client.message, CREDENTIAL)
            .map_err(|_| PairingError::Rejected)?;
        let registration = client
            .state
            .finish(
                &mut rng,
                code.as_bytes(),
                server.message,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|_| PairingError::Rejected)?;
        let credential = ServerRegistration::finish(registration.message);
        let identity = HostIdentity::of(&setup.keypair().public().serialize());
        Ok(Self {
            setup,
            credential,
            identity,
        })
    }

    pub fn identity(&self) -> HostIdentity {
        self.identity
    }

    pub fn start(
        &self,
        request: LoginRequest,
    ) -> Result<(PendingLogin, LoginChallenge), PairingError> {
        let request =
            CredentialRequest::deserialize(&decode(&request.message, OPAQUE_MESSAGE_LIMIT)?)
                .map_err(|_| PairingError::Rejected)?;
        let mut id = [0; CHALLENGE_LEN];
        ring::rand::SystemRandom::new()
            .fill(&mut id)
            .map_err(|_| PairingError::Randomness)?;
        let result = ServerLogin::start(
            &mut OsRng,
            &self.setup,
            Some(self.credential.clone()),
            request,
            CREDENTIAL,
            server_parameters(),
        )
        .map_err(|_| PairingError::Rejected)?;
        Ok((
            PendingLogin {
                login: result.state,
                id,
            },
            LoginChallenge {
                id: BASE64.encode(id),
                message: BASE64.encode(result.message.serialize()),
            },
        ))
    }
}

impl PendingLogin {
    /// Consumes the challenge. The offer is available only after key confirmation.
    pub fn accept(self, request: LoginConfirmation) -> Result<(String, SignalKey), PairingError> {
        if request.id != BASE64.encode(self.id) {
            return Err(PairingError::Rejected);
        }
        let message =
            CredentialFinalization::deserialize(&decode(&request.message, OPAQUE_MESSAGE_LIMIT)?)
                .map_err(|_| PairingError::Rejected)?;
        let result = self
            .login
            .finish(message, server_parameters())
            .map_err(|_| PairingError::Rejected)?;
        let session_key = Zeroizing::new(result.session_key);
        let offer =
            SignalKey::derive(&session_key[..], &self.id, b"offer")?.open(&request.offer)?;
        let answer = SignalKey::derive(&session_key[..], &self.id, b"answer")?;
        Ok((offer, answer))
    }
}

pub struct ClientPairing {
    login: ClientLogin<Suite>,
    code: AccessCode,
}

impl ClientPairing {
    pub fn start(code: AccessCode) -> Result<(Self, LoginRequest), PairingError> {
        let result =
            ClientLogin::start(&mut OsRng, code.as_bytes()).map_err(|_| PairingError::Rejected)?;
        Ok((
            Self {
                login: result.state,
                code,
            },
            LoginRequest {
                message: BASE64.encode(result.message.serialize()),
            },
        ))
    }

    pub fn confirm(
        self,
        challenge: LoginChallenge,
        trusted_host: Option<HostIdentity>,
        offer: &str,
    ) -> Result<(LoginConfirmation, SignalKey, HostIdentity), PairingError> {
        let id: [u8; CHALLENGE_LEN] = decode(&challenge.id, CHALLENGE_LEN)?
            .try_into()
            .map_err(|_| PairingError::Rejected)?;
        let response =
            CredentialResponse::deserialize(&decode(&challenge.message, OPAQUE_MESSAGE_LIMIT)?)
                .map_err(|_| PairingError::Rejected)?;
        let result = self
            .login
            .finish(
                &mut OsRng,
                self.code.as_bytes(),
                response,
                ClientLoginFinishParameters::new(Some(CONTEXT), Default::default(), None),
            )
            .map_err(|_| PairingError::Rejected)?;
        let identity = HostIdentity::of(&result.server_s_pk.serialize());
        if trusted_host.is_some_and(|expected| expected != identity) {
            return Err(PairingError::IdentityChanged);
        }
        let session_key = Zeroizing::new(result.session_key);
        let protected_offer = SignalKey::derive(&session_key[..], &id, b"offer")?.seal(offer)?;
        let answer = SignalKey::derive(&session_key[..], &id, b"answer")?;
        Ok((
            LoginConfirmation {
                id: challenge.id,
                message: BASE64.encode(result.message.serialize()),
                offer: protected_offer,
            },
            answer,
            identity,
        ))
    }
}

/// A direction-specific key used once for a complete SDP description, including its fingerprint.
pub struct SignalKey(aead::LessSafeKey);

impl SignalKey {
    fn derive(
        session_key: &[u8],
        id: &[u8; CHALLENGE_LEN],
        label: &[u8],
    ) -> Result<Self, PairingError> {
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, CONTEXT);
        let prk = salt.extract(session_key);
        let info = [label, id.as_slice()];
        let mut key = Zeroizing::new([0; 32]);
        prk.expand(&info, &aead::CHACHA20_POLY1305)
            .and_then(|output| output.fill(&mut *key))
            .map_err(|_| PairingError::InvalidSignal)?;
        let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
            .map_err(|_| PairingError::InvalidSignal)?;
        Ok(Self(aead::LessSafeKey::new(key)))
    }

    pub fn seal(self, sdp: &str) -> Result<String, PairingError> {
        if sdp.is_empty() || sdp.len() > SDP_LIMIT {
            return Err(PairingError::InvalidSignal);
        }
        let mut nonce = [0; NONCE_LEN];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| PairingError::Randomness)?;
        let mut ciphertext = sdp.as_bytes().to_vec();
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(CONTEXT),
                &mut ciphertext,
            )
            .map_err(|_| PairingError::InvalidSignal)?;
        let mut message = nonce.to_vec();
        message.extend_from_slice(&ciphertext);
        Ok(BASE64.encode(message))
    }

    pub fn open(self, message: &str) -> Result<String, PairingError> {
        let mut message = Zeroizing::new(
            decode(message, NONCE_LEN + SDP_LIMIT + aead::MAX_TAG_LEN)
                .map_err(|_| PairingError::InvalidSignal)?,
        );
        if message.len() <= NONCE_LEN {
            return Err(PairingError::InvalidSignal);
        }
        let (nonce, ciphertext) = message.split_at_mut(NONCE_LEN);
        let nonce = aead::Nonce::try_assume_unique_for_key(nonce)
            .map_err(|_| PairingError::InvalidSignal)?;
        let plaintext = self
            .0
            .open_in_place(nonce, aead::Aad::from(CONTEXT), ciphertext)
            .map_err(|_| PairingError::InvalidSignal)?;
        std::str::from_utf8(plaintext)
            .map(str::to_owned)
            .map_err(|_| PairingError::InvalidSignal)
    }
}

fn decode(input: &str, max_bytes: usize) -> Result<Vec<u8>, PairingError> {
    if input.len() > (max_bytes * 4).div_ceil(3) {
        return Err(PairingError::Rejected);
    }
    let decoded = BASE64.decode(input).map_err(|_| PairingError::Rejected)?;
    if decoded.len() > max_bytes {
        return Err(PairingError::Rejected);
    }
    Ok(decoded)
}

fn server_parameters() -> ServerLoginParameters<'static, 'static> {
    ServerLoginParameters {
        context: Some(CONTEXT),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &[u8] = b"a0Z9b2";

    #[test]
    fn verifies_host_and_both_descriptions() -> Result<(), PairingError> {
        let host = HostPairing::enroll(&AccessCode::parse(CODE)?)?;
        let (client, request) = ClientPairing::start(AccessCode::parse(CODE)?)?;
        let (pending, challenge) = host.start(request)?;
        let (confirmation, answer_key, identified) =
            client.confirm(challenge, Some(host.identity()), "offer fingerprint value")?;
        assert_eq!(identified, host.identity());
        let (offer, answer) = pending.accept(confirmation)?;
        assert_eq!(offer, "offer fingerprint value");
        assert_eq!(
            answer_key.open(&answer.seal("answer fingerprint value")?)?,
            "answer fingerprint value"
        );
        Ok(())
    }

    #[test]
    fn rejects_wrong_code_and_changed_device() -> Result<(), PairingError> {
        let host = HostPairing::enroll(&AccessCode::parse(CODE)?)?;
        let (wrong, request) = ClientPairing::start(AccessCode::parse(b"a0Z9b3")?)?;
        let (_, challenge) = host.start(request)?;
        assert!(matches!(
            wrong.confirm(challenge, None, "offer"),
            Err(PairingError::Rejected)
        ));
        let (client, request) = ClientPairing::start(AccessCode::parse(CODE)?)?;
        let (_, challenge) = host.start(request)?;
        assert!(matches!(
            client.confirm(challenge, Some(HostIdentity([0; 32])), "offer"),
            Err(PairingError::IdentityChanged)
        ));
        Ok(())
    }

    #[test]
    fn rejects_modified_sdp_and_wrong_direction() -> Result<(), PairingError> {
        let host = HostPairing::enroll(&AccessCode::parse(CODE)?)?;
        let (client, request) = ClientPairing::start(AccessCode::parse(CODE)?)?;
        let (pending, challenge) = host.start(request)?;
        let (mut confirmation, answer, _) = client.confirm(challenge, None, "fingerprint")?;
        let mut corrupted = decode(&confirmation.offer, SDP_LIMIT)?;
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        assert!(matches!(
            answer.open(&confirmation.offer),
            Err(PairingError::InvalidSignal)
        ));
        confirmation.offer = BASE64.encode(corrupted);
        assert!(matches!(
            pending.accept(confirmation),
            Err(PairingError::InvalidSignal)
        ));
        Ok(())
    }

    #[test]
    fn restored_setup_survives_password_rotation() -> Result<(), PairingError> {
        let original = HostPairing::enroll(&AccessCode::parse(CODE)?)?;
        let setup = original.export_setup();
        let replacement = b"9aD0e3";
        let restored = HostPairing::from_setup(&AccessCode::parse(replacement)?, &setup)?;
        assert_eq!(original.identity(), restored.identity());
        let (client, request) = ClientPairing::start(AccessCode::parse(replacement)?)?;
        let (pending, challenge) = restored.start(request)?;
        let (proof, _, _) = client.confirm(challenge, Some(original.identity()), "offer")?;
        assert_eq!(pending.accept(proof)?.0, "offer");
        let (client, request) = ClientPairing::start(AccessCode::parse(CODE)?)?;
        let (_, challenge) = restored.start(request)?;
        assert!(matches!(
            client.confirm(challenge, None, "offer"),
            Err(PairingError::Rejected)
        ));
        Ok(())
    }
}
