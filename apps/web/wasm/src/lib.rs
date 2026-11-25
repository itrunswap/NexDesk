#![forbid(unsafe_code)]
#![cfg(target_arch = "wasm32")]

use nexdesk_session::{
    control::{Action, ControlMessage},
    envelope,
    pairing::{AccessCode, ClientPairing, LoginChallenge, SignalKey},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct BrowserPairing {
    client: Option<ClientPairing>,
    request: String,
    answer: Option<SignalKey>,
}

#[wasm_bindgen]
impl BrowserPairing {
    #[wasm_bindgen(constructor)]
    pub fn new(code: &[u8]) -> Result<Self, JsValue> {
        let code = AccessCode::parse(code).map_err(error)?;
        let (client, request) = ClientPairing::start(code).map_err(error)?;
        Ok(Self {
            client: Some(client),
            request: serde_json::to_string(&request).map_err(error)?,
            answer: None,
        })
    }

    pub fn request(&self) -> String {
        self.request.clone()
    }

    pub fn confirm(&mut self, challenge: &str, pin: &str, offer: &str) -> Result<String, JsValue> {
        let challenge: LoginChallenge = serde_json::from_str(challenge).map_err(error)?;
        let pin = (!pin.is_empty())
            .then(|| pin.parse())
            .transpose()
            .map_err(error)?;
        let client = self
            .client
            .take()
            .ok_or_else(|| error("pairing already used"))?;
        let (message, answer, identity) = client.confirm(challenge, pin, offer).map_err(error)?;
        self.answer = Some(answer);
        Ok(json!({"confirmation":message, "identity":identity.to_string()}).to_string())
    }

    pub fn answer(&mut self, sealed: &str) -> Result<String, JsValue> {
        self.answer
            .take()
            .ok_or_else(|| error("response already used"))?
            .open(sealed)
            .map_err(error)
    }
}

#[wasm_bindgen]
pub fn control_packet(raw: &str) -> Result<Vec<u8>, JsValue> {
    let action: Action = serde_json::from_str(raw).map_err(error)?;
    if !action.valid() {
        return Err(error("invalid desktop action"));
    }
    Ok(ControlMessage::Action(action).pack())
}

#[wasm_bindgen]
pub fn decode_control(packet: &[u8]) -> Result<String, JsValue> {
    match ControlMessage::unpack(packet) {
        Some(ControlMessage::State(state)) => Ok(json!({"state": state}).to_string()),
        Some(ControlMessage::End(reason)) => Ok(json!({"ended": reason}).to_string()),
        _ => Err(error("invalid desktop message")),
    }
}

#[wasm_bindgen]
pub fn collaboration_packet(id: u32, raw: &str, bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
    let value: Value = serde_json::from_str(raw).map_err(error)?;
    envelope::encode(u64::from(id), &value, bytes.to_vec())
        .ok_or_else(|| error("invalid collaboration message"))
}

#[wasm_bindgen]
pub struct CollaborationPacket {
    id: u32,
    message: String,
    bytes: Vec<u8>,
}

#[wasm_bindgen]
impl CollaborationPacket {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn message(&self) -> String {
        self.message.clone()
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

#[wasm_bindgen]
pub fn decode_collaboration(packet: &[u8]) -> Result<CollaborationPacket, JsValue> {
    let (id, value, bytes) =
        envelope::decode::<Value>(packet).ok_or_else(|| error("invalid collaboration message"))?;
    Ok(CollaborationPacket {
        id: u32::try_from(id).map_err(error)?,
        message: value.to_string(),
        bytes,
    })
}

#[wasm_bindgen]
pub struct FileHash(Sha256);

#[wasm_bindgen]
impl FileHash {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn digest(&self) -> Vec<u8> {
        self.0.clone().finalize().to_vec()
    }
}

impl Default for FileHash {
    fn default() -> Self {
        Self(Sha256::new())
    }
}

fn error(message: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&message.to_string())
}
