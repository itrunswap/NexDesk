use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use nexdesk_files::{BLOCK, FileError, FileReply, FileRequest, Root, TransferStore};
use nexdesk_media::ControlPipe;
use nexdesk_session::collaboration::{Capabilities, decode, encode};
use tokio::sync::watch;

pub(crate) struct FileService {
    pub scope: Option<(Arc<Root>, Arc<Mutex<TransferStore>>)>,
    pub capabilities: Capabilities,
    pub revoked: watch::Receiver<u64>,
    pub shutdown: watch::Receiver<bool>,
}

impl FileService {
    pub async fn run(mut self, mut pipe: ControlPipe, mut stopped: watch::Receiver<bool>) {
        loop {
            let packet = tokio::select! {
                biased;
                _ = self.revoked.changed() => break,
                _ = self.shutdown.changed() => break,
                _ = stopped.changed() => break,
                packet = pipe.recv() => packet,
            };
            let Some((id, request, bytes)) = packet.as_deref().and_then(decode::<FileRequest>)
            else {
                break;
            };
            let scope = self
                .scope
                .as_ref()
                .filter(|_| bytes.len() <= BLOCK && self.allows(&request));
            let result = if let Some((root, transfers)) = scope {
                let root = root.clone();
                let transfers = transfers.clone();
                let revoked = self.revoked.clone();
                let shutdown = self.shutdown.clone();
                let stopped = stopped.clone();
                tokio::task::spawn_blocking(move || {
                    let canceled = || {
                        revoked.has_changed().unwrap_or(true)
                            || shutdown.has_changed().unwrap_or(true)
                            || stopped.has_changed().unwrap_or(true)
                    };
                    transfers
                        .lock()
                        .map_err(|_| FileError::Io)?
                        .execute(&root, request, &bytes, canceled)
                })
                .await
                .unwrap_or(Err(FileError::Io))
            } else {
                Err(FileError::Denied)
            };
            if self.revoked.has_changed().unwrap_or(true) || stopped.has_changed().unwrap_or(true) {
                break;
            }
            let (reply, block): (Result<FileReply, FileError>, Vec<u8>) = match result {
                Ok((reply, block)) => (Ok(reply), block),
                Err(error) => (Err(error), Vec::new()),
            };
            if !encode(id, &reply, block).is_some_and(|packet| pipe.send(&packet)) {
                break;
            }
            // Acknowledge at most one block per interval to keep SCTP pressure bounded.
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }

    fn allows(&self, request: &FileRequest) -> bool {
        match request {
            FileRequest::List { .. } | FileRequest::Cancel { .. } => self.capabilities.any_files(),
            FileRequest::ReadOpen { .. } | FileRequest::ReadBlock { .. } => {
                self.capabilities.includes(Capabilities::FILE_READ)
            }
            _ => self.capabilities.includes(Capabilities::FILE_WRITE),
        }
    }
}
