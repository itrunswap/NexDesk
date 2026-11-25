//! Wire format shared by native and browser collaboration clients.

use nexdesk_core::CONTROL_PACKET_LIMIT;
use prost::Message;
use serde::{Serialize, de::DeserializeOwned};

const VERSION: u32 = 1;

#[derive(Message)]
struct Wire {
    #[prost(uint32, tag = "1")]
    version: u32,
    #[prost(uint64, tag = "2")]
    id: u64,
    #[prost(bytes, tag = "3")]
    message: Vec<u8>,
    #[prost(bytes, tag = "4")]
    data: Vec<u8>,
}

pub fn encode<T: Serialize>(id: u64, message: &T, data: Vec<u8>) -> Option<Vec<u8>> {
    if id == 0 {
        return None;
    }
    let packet = Wire {
        version: VERSION,
        id,
        message: serde_json::to_vec(message).ok()?,
        data,
    };
    (packet.encoded_len() <= CONTROL_PACKET_LIMIT).then(|| packet.encode_to_vec())
}

pub fn decode<T: DeserializeOwned>(packet: &[u8]) -> Option<(u64, T, Vec<u8>)> {
    if !(1..=CONTROL_PACKET_LIMIT).contains(&packet.len()) {
        return None;
    }
    let packet = Wire::decode(packet).ok()?;
    if packet.version != VERSION || packet.id == 0 {
        return None;
    }
    Some((
        packet.id,
        serde_json::from_slice(&packet.message).ok()?,
        packet.data,
    ))
}
