use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::DesktopOffer;

const PURPOSE: &[u8] = b"NexDesk controller authorization v1\0";

#[derive(Debug, thiserror::Error)]
#[error("invalid controller identity proof")]
pub struct ControllerError;

/// Private keys are managed by the desktop credential store.
pub struct ControllerKey(Ed25519KeyPair);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerIdentity([u8; 32]);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerProof {
    key: String,
    signature: String,
}

impl ControllerKey {
    pub fn generate() -> Result<Zeroizing<Vec<u8>>, ControllerError> {
        let key =
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| ControllerError)?;
        Ok(Zeroizing::new(key.as_ref().to_vec()))
    }

    pub fn from_pkcs8(key: &[u8]) -> Result<Self, ControllerError> {
        Ed25519KeyPair::from_pkcs8(key)
            .map(Self)
            .map_err(|_| ControllerError)
    }

    pub(crate) fn prove(
        &self,
        challenge: &str,
        offer: &DesktopOffer,
    ) -> Result<ControllerProof, ControllerError> {
        Ok(ControllerProof {
            key: BASE64.encode(self.0.public_key().as_ref()),
            signature: BASE64.encode(self.0.sign(&message(challenge, offer)?).as_ref()),
        })
    }
}

impl ControllerIdentity {
    pub fn parse(key: &str) -> Result<Self, ControllerError> {
        if key.len() != 43 {
            return Err(ControllerError);
        }
        let mut bytes = [0; 32];
        if BASE64.decode_slice(key, &mut bytes).ok() == Some(bytes.len()) {
            return Ok(Self(bytes));
        }
        Err(ControllerError)
    }

    pub fn encoded(&self) -> String {
        BASE64.encode(self.0)
    }
}

impl ControllerProof {
    pub fn verify(
        &self,
        challenge: &str,
        offer: &DesktopOffer,
    ) -> Result<ControllerIdentity, ControllerError> {
        let key = ControllerIdentity::parse(&self.key)?;
        if self.signature.len() != 86 {
            return Err(ControllerError);
        }
        let mut signature = [0; 64];
        if BASE64.decode_slice(&self.signature, &mut signature).ok() != Some(signature.len()) {
            return Err(ControllerError);
        }
        UnparsedPublicKey::new(&ED25519, key.0)
            .verify(&message(challenge, offer)?, &signature)
            .map_err(|_| ControllerError)?;
        Ok(key)
    }
}

fn message(challenge: &str, offer: &DesktopOffer) -> Result<Vec<u8>, ControllerError> {
    if challenge.is_empty() || challenge.len() > 128 || offer.controller_proof.is_some() {
        return Err(ControllerError);
    }
    let serialized = Zeroizing::new(serde_json::to_vec(offer).map_err(|_| ControllerError)?);
    let offer_hash = digest::digest(&digest::SHA256, &serialized);
    let mut message = Vec::with_capacity(PURPOSE.len() + challenge.len() + 33);
    message.extend_from_slice(PURPOSE);
    message.extend_from_slice(challenge.as_bytes());
    message.push(0);
    message.extend_from_slice(offer_hash.as_ref());
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_is_bound_to_the_challenge_and_requested_access() -> Result<(), ControllerError> {
        let key = ControllerKey::from_pkcs8(&ControllerKey::generate()?)?;
        let mut offer = DesktopOffer {
            sdp: "fixture".into(),
            request_input: true,
            resume: None,
            capabilities: None,
            remote: None,
            controller_proof: None,
        };
        let proof = key.prove("first", &offer)?;
        assert!(proof.verify("first", &offer).is_ok());
        assert!(proof.verify("second", &offer).is_err());
        offer.request_input = false;
        assert!(proof.verify("first", &offer).is_err());
        Ok(())
    }
}
