use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use nexdesk_media::{ControlPipe, HostPeer};
use nexdesk_platform::Clipboard;
use nexdesk_session::collaboration::{
    Capabilities, Problem, Reply, Request, decode, encode, valid_text,
};

use crate::{desktop_session::RemoteDesktop, resolution::Resolution};

pub(crate) struct Collaboration {
    pipe: ControlPipe,
    capabilities: Capabilities,
    resolution: Resolution,
    clipboard: Arc<Mutex<Option<Clipboard>>>,
}

impl Collaboration {
    pub fn new(pipe: ControlPipe, capabilities: Capabilities) -> Self {
        Self {
            pipe,
            capabilities,
            resolution: Resolution::default(),
            clipboard: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn recv(own: &mut Option<Self>) -> Option<(u64, Request)> {
        let packet = own.as_mut()?.pipe.recv().await?;
        let (id, request, data) = decode(&packet)?;
        data.is_empty().then_some((id, request))
    }

    pub fn tick(&mut self, peer: &HostPeer, desktop: &mut RemoteDesktop) {
        if self.resolution.tick() {
            desktop.begin_mode_change();
            desktop.refresh_capture(peer);
        }
    }

    pub fn restore_display(&mut self) {
        self.resolution.restore();
    }

    pub async fn respond(
        &mut self,
        id: u64,
        request: Request,
        peer: &HostPeer,
        desktop: &mut RemoteDesktop,
    ) -> bool {
        let response = self.process(request, peer, desktop).await;
        encode(id, &response, Vec::new()).is_some_and(|packet| self.pipe.send(&packet))
    }

    async fn process(
        &mut self,
        request: Request,
        peer: &HostPeer,
        desktop: &mut RemoteDesktop,
    ) -> Result<Reply, Problem> {
        match request {
            Request::Quality { quality } => {
                if !quality.valid() {
                    return Err(Problem::Invalid);
                }
                peer.set_quality(quality)
                    .map_err(|_| Problem::Unavailable)?;
                Ok(Reply::Quality { quality })
            }
            Request::Diagnostics => {
                let media = tokio::time::timeout(Duration::from_millis(350), peer.stats())
                    .await
                    .map_err(|_| Problem::Unavailable)?
                    .map_err(|_| Problem::Unavailable)?;
                Ok(Reply::Diagnostics {
                    media: Box::new(media),
                    audio: peer.sound(),
                })
            }
            Request::ClipboardRead => self.clipboard(None).await,
            Request::ClipboardWrite { text } => self.clipboard(Some(text)).await,
            Request::Text { generation, text } => {
                self.require(Capabilities::TEXT)?;
                if !valid_text(&text) {
                    return Err(Problem::Invalid);
                }
                desktop
                    .text(generation, &text)
                    .map_err(|_| Problem::Unavailable)?;
                Ok(Reply::Ok)
            }
            Request::Modes { display } => {
                self.require(Capabilities::DISPLAY)?;
                Resolution::modes(desktop.monitor(&display).ok_or(Problem::Unavailable)?)
            }
            Request::Apply { display, mode } => {
                self.require(Capabilities::DISPLAY)?;
                let monitor = desktop.monitor(&display).ok_or(Problem::Unavailable)?;
                desktop.begin_mode_change();
                let result = self.resolution.apply(monitor, mode);
                if result.is_ok() {
                    desktop.refresh_capture(peer);
                }
                result
            }
            Request::Confirm { token } => {
                self.require(Capabilities::DISPLAY)?;
                let mode = self.resolution.pending(token).ok_or(Problem::Expired)?;
                if !desktop.mode_visible(mode) {
                    return Err(Problem::Busy);
                }
                self.resolution.confirm(token)
            }
            Request::Rollback { token } => {
                self.require(Capabilities::DISPLAY)?;
                desktop.begin_mode_change();
                let result = self.resolution.rollback(token);
                if result.is_ok() {
                    desktop.refresh_capture(peer);
                }
                result
            }
        }
    }

    fn require(&self, capability: Capabilities) -> Result<(), Problem> {
        if self.capabilities.includes(capability) {
            Ok(())
        } else {
            Err(Problem::Denied)
        }
    }

    async fn clipboard(&self, text: Option<String>) -> Result<Reply, Problem> {
        self.require(Capabilities::CLIPBOARD)?;
        if text.as_deref().is_some_and(|text| !valid_text(text)) {
            return Err(Problem::Invalid);
        }
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || {
            let mut active = clipboard.lock().map_err(|_| Problem::Unavailable)?;
            if active.is_none() {
                *active = Some(Clipboard::open().map_err(|_| Problem::Unavailable)?);
            }
            let clipboard = active.as_mut().ok_or(Problem::Unavailable)?;
            if let Some(text) = text {
                clipboard.write(text).map_err(|_| Problem::Unavailable)?;
                Ok(Reply::Ok)
            } else {
                let text = clipboard.read().map_err(|_| Problem::Unavailable)?;
                if !valid_text(&text) {
                    return Err(Problem::Invalid);
                }
                Ok(Reply::Clipboard { text })
            }
        })
        .await
        .map_err(|_| Problem::Unavailable)?
    }
}
