//! Versioned Protobuf messages for the authenticated native control channel.

use nexdesk_core::CONTROL_PACKET_LIMIT;
use prost::{Enumeration, Message, Oneof};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    View,
    Control,
}

#[derive(Clone, PartialEq, Message)]
struct Packet {
    #[prost(uint32, tag = "1")]
    version: u32,
    #[prost(oneof = "ControlMessage", tags = "2, 3, 4")]
    message: Option<ControlMessage>,
}

#[derive(Clone, PartialEq, Oneof)]
pub enum ControlMessage {
    #[prost(message, tag = "2")]
    Action(Action),
    #[prost(message, tag = "3")]
    State(DesktopState),
    #[prost(enumeration = "EndReason", tag = "4")]
    End(i32),
}

#[derive(Clone, PartialEq, Message, Serialize, Deserialize)]
pub struct Action {
    #[prost(enumeration = "ActionKind", tag = "1")]
    pub kind: i32,
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    #[prost(sint32, tag = "3")]
    pub x: i32,
    #[prost(sint32, tag = "4")]
    pub y: i32,
    #[prost(uint32, tag = "5")]
    pub code: u32,
    #[prost(bool, tag = "6")]
    pub down: bool,
    #[prost(string, tag = "7")]
    pub display: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Enumeration)]
#[repr(i32)]
pub enum ActionKind {
    Unspecified = 0,
    Move = 1,
    Button = 2,
    Wheel = 3,
    Key = 4,
    Release = 5,
    Select = 6,
    Close = 7,
    Heartbeat = 8,
}

#[derive(Clone, PartialEq, Message, Serialize)]
pub struct DesktopState {
    #[prost(uint64, tag = "1")]
    pub generation: u64,
    #[prost(bool, tag = "2")]
    pub can_input: bool,
    #[prost(string, tag = "3")]
    pub selected: String,
    #[prost(message, repeated, tag = "4")]
    pub displays: Vec<Display>,
    #[prost(uint32, tag = "5")]
    pub width: u32,
    #[prost(uint32, tag = "6")]
    pub height: u32,
    #[prost(string, tag = "7")]
    pub notice: String,
}

#[derive(Clone, PartialEq, Message, Serialize)]
pub struct Display {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Enumeration)]
#[repr(i32)]
pub enum EndReason {
    Closed = 0,
    Revoked = 1,
    CaptureUnavailable = 2,
    Protocol = 3,
}

impl ControlMessage {
    pub fn pack(self) -> Vec<u8> {
        Packet {
            version: VERSION,
            message: Some(self),
        }
        .encode_to_vec()
    }

    pub fn unpack(bytes: &[u8]) -> Option<Self> {
        if !(1..=CONTROL_PACKET_LIMIT).contains(&bytes.len()) {
            return None;
        }
        let packet = Packet::decode(bytes).ok()?;
        if packet.version != VERSION {
            return None;
        }
        let message = packet.message?;
        match &message {
            Self::Action(action) if action.valid() => Some(message),
            Self::State(state) if state.valid() => Some(message),
            Self::End(reason) if EndReason::try_from(*reason).is_ok() => Some(message),
            _ => None,
        }
    }
}

impl Action {
    pub fn simple(kind: ActionKind) -> Self {
        Self {
            kind: kind as i32,
            ..Self::default()
        }
    }

    pub fn valid(&self) -> bool {
        match self.kind() {
            ActionKind::Move => (0..=65535).contains(&self.x) && (0..=65535).contains(&self.y),
            ActionKind::Button => {
                (1..=5).contains(&self.code)
                    && (0..=65535).contains(&self.x)
                    && (0..=65535).contains(&self.y)
            }
            ActionKind::Wheel => {
                (-1200..=1200).contains(&self.x) && (-1200..=1200).contains(&self.y)
            }
            ActionKind::Key => (0x0007_0004..=0x0007_00e7).contains(&self.code),
            ActionKind::Select => !self.display.is_empty() && self.display.len() <= 64,
            ActionKind::Release | ActionKind::Close | ActionKind::Heartbeat => true,
            ActionKind::Unspecified => false,
        }
    }
}

impl DesktopState {
    fn valid(&self) -> bool {
        self.selected.len() <= 64
            && self.notice.len() <= 96
            && self.displays.len() <= 32
            && self.width <= 4096
            && self.height <= 4096
            && self.displays.iter().all(|display| {
                !display.id.is_empty() && display.id.len() <= 64 && display.label.len() <= 100
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_coordinates_and_unversioned_messages() {
        let invalid = Action {
            kind: ActionKind::Button as i32,
            x: 65_536,
            y: 10,
            code: 1,
            ..Action::default()
        };
        assert!(ControlMessage::unpack(&ControlMessage::Action(invalid).pack()).is_none());
        let release = ControlMessage::Action(Action::simple(ActionKind::Release));
        assert_eq!(
            ControlMessage::unpack(&release.clone().pack()),
            Some(release.clone())
        );
        let legacy = Packet {
            version: 1,
            message: Some(release),
        }
        .encode_to_vec();
        assert!(ControlMessage::unpack(&legacy).is_none());
    }
}
