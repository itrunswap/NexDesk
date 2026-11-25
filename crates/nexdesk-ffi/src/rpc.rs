use std::time::Duration;

use nexdesk_media::ControlPipe;
use nexdesk_session::collaboration::{decode, encode};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CallError {
    Lost,
    Invalid,
}

struct Call {
    packet: Vec<u8>,
    response: oneshot::Sender<Vec<u8>>,
    deadline: Duration,
}

#[derive(Clone)]
pub(crate) struct Rpc(mpsc::Sender<Call>);

impl Rpc {
    pub fn new(mut pipe: ControlPipe) -> (Self, JoinHandle<()>) {
        let (send, mut calls) = mpsc::channel::<Call>(12);
        let worker = tokio::spawn(async move {
            if !tokio::time::timeout(Duration::from_secs(5), pipe.wait_open())
                .await
                .unwrap_or(false)
            {
                return;
            }
            loop {
                let Some(call) = (tokio::select! {
                    biased;
                    _ = pipe.recv() => None,
                    call = calls.recv() => call,
                }) else {
                    return;
                };
                if call.response.is_closed() {
                    continue;
                }
                if !pipe.send(&call.packet) {
                    return;
                }
                let Ok(Some(reply)) = tokio::time::timeout(call.deadline, pipe.recv()).await else {
                    return;
                };
                let _ = call.response.send(reply);
            }
        });
        (Self(send), worker)
    }

    pub async fn call<In: Serialize, Out: DeserializeOwned>(
        &self,
        id: u64,
        data: Vec<u8>,
        request: &In,
        deadline: Duration,
    ) -> Result<(Out, Vec<u8>), CallError> {
        let (send, receive) = oneshot::channel();
        let packet = encode(id, request, data).ok_or(CallError::Invalid)?;
        self.0
            .send(Call {
                packet,
                response: send,
                deadline,
            })
            .await
            .map_err(|_| CallError::Lost)?;
        let bytes = receive.await.map_err(|_| CallError::Lost)?;
        let (serial, reply, binary) = decode(&bytes).ok_or(CallError::Invalid)?;
        if serial != id {
            return Err(CallError::Invalid);
        }
        Ok((reply, binary))
    }

    pub fn closed(&self) -> bool {
        self.0.is_closed()
    }
}
