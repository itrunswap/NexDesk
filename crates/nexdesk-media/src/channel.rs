use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use libwebrtc::{
    data_channel::{DataChannel, DataChannelInit, DataChannelState},
    peer_connection::PeerConnection,
};
use tokio::sync::{Notify, mpsc};

use crate::MediaError;

pub use nexdesk_core::CONTROL_PACKET_LIMIT;
const SEND_LIMIT: u64 = 64 * 1024;

/// One ordered control stream, negotiated inside the authenticated SDP.
pub struct ControlPipe {
    channel: DataChannel,
    receive: mpsc::Receiver<Vec<u8>>,
    closed: Arc<AtomicBool>,
    changed: Arc<Notify>,
}

impl ControlPipe {
    pub(crate) fn new(peer: &PeerConnection) -> Result<Self, MediaError> {
        Self::negotiate(peer, 0, "nexdesk-control", "nexdesk.control.v2")
    }

    pub(crate) fn collaboration(peer: &PeerConnection) -> Result<Self, MediaError> {
        Self::negotiate(peer, 2, "nexdesk-collaboration", "nexdesk.collaboration.v1")
    }

    pub(crate) fn files(peer: &PeerConnection) -> Result<Self, MediaError> {
        Self::negotiate(peer, 4, "nexdesk-files", "nexdesk.files.v1")
    }

    pub(crate) fn shell(peer: &PeerConnection) -> Result<Self, MediaError> {
        Self::negotiate(peer, 6, "nexdesk-shell", "nexdesk.shell.v1")
    }

    fn negotiate(
        peer: &PeerConnection,
        id: i32,
        label: &str,
        protocol: &str,
    ) -> Result<Self, MediaError> {
        let channel = peer.create_data_channel(
            label,
            DataChannelInit {
                protocol: protocol.into(),
                negotiated: true,
                id,
                ..DataChannelInit::default()
            },
        )?;
        let (send, receive) = mpsc::channel(128);
        let closed = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(Notify::new());
        let flag = closed.clone();
        let wake = changed.clone();
        channel.on_message(Some(Box::new(move |packet| {
            if flag.load(Ordering::Acquire) {
                return;
            }
            if !packet.binary
                || !(1..=CONTROL_PACKET_LIMIT).contains(&packet.data.len())
                || send.try_send(packet.data.to_vec()).is_err()
            {
                flag.store(true, Ordering::Release);
                wake.notify_one();
            }
        })));
        let flag = closed.clone();
        let wake = changed.clone();
        channel.on_state_change(Some(Box::new(move |state| {
            if matches!(state, DataChannelState::Closing | DataChannelState::Closed) {
                flag.store(true, Ordering::Release);
            }
            wake.notify_one();
        })));
        Ok(Self {
            channel,
            receive,
            closed,
            changed,
        })
    }

    pub fn is_open(&self) -> bool {
        !self.is_closed() && self.channel.state() == DataChannelState::Open
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn send(&self, packet: &[u8]) -> bool {
        self.is_open()
            && (1..=CONTROL_PACKET_LIMIT).contains(&packet.len())
            && self.channel.buffered_amount() < SEND_LIMIT
            && self.channel.send(packet, true).is_ok()
    }

    pub async fn wait_open(&self) -> bool {
        loop {
            let changed = self.changed.notified();
            if self.is_open() {
                return true;
            }
            if self.is_closed() {
                return false;
            }
            changed.await;
        }
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        while !self.is_closed() {
            tokio::select! {
                biased;
                _ = self.changed.notified() => {},
                packet = self.receive.recv() => return packet.filter(|_| !self.is_closed()),
            }
        }
        None
    }
}

impl Drop for ControlPipe {
    fn drop(&mut self) {
        self.channel.on_message(None);
        self.channel.on_state_change(None);
        self.channel.close();
    }
}
