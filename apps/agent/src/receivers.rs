use std::{
    io,
    net::SocketAddr,
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use nexdesk_directory::ServerOrigin;
use nexdesk_files::Root;
use nexdesk_store::{DesktopPreferences, Store};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, sync::watch};

use crate::{
    desktop_host::{DesktopHost, capture_target, report},
    directory_host,
    server::{Host, serve_http},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiverSettings {
    pub request: u64,
    pub settings: DesktopPreferences,
    pub default_directory: String,
    pub lan_address: Option<String>,
    pub source: String,
    pub save: bool,
}

#[derive(Default)]
pub(crate) struct ManagedReceivers {
    lan: Option<Receiver<SocketAddr>>,
    directory: Option<Receiver<DirectoryReceiver>>,
    share_root: String,
    source: String,
}

struct Receiver<T> {
    value: T,
    close: watch::Sender<bool>,
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close.send_replace(true);
    }
}

struct DirectoryReceiver {
    origin: ServerOrigin,
    allow_shell: bool,
    generation: u64,
}

impl ManagedReceivers {
    pub(crate) async fn apply(
        &mut self,
        desktop: &DesktopHost,
        host: &Arc<Host>,
        request: ReceiverSettings,
    ) {
        let id = request.request;
        if let Err(error) = self.configure(desktop, host, request).await {
            report(json!({"event":"settings_result", "request":id, "error":error}));
        }
    }

    async fn configure(
        &mut self,
        desktop: &DesktopHost,
        host: &Arc<Host>,
        request: ReceiverSettings,
    ) -> Result<(), &'static str> {
        let settings = request.settings.validated().map_err(|_| "invalid")?;
        let origin = settings
            .active_origin(&request.default_directory)
            .map_err(|_| "directory")?;
        let address = if settings.lan_allowed {
            request
                .lan_address
                .as_deref()
                .map(|address| settings.lan_endpoint(address))
                .transpose()
                .map_err(|_| "address")?
        } else {
            None
        };
        let replace_lan = self.lan.as_ref().map(|lan| lan.value) != address;
        let mut lan_issue = (settings.lan_allowed && address.is_none()).then_some("address");
        let listener = if let Some(address) = address.filter(|_| replace_lan) {
            match TcpListener::bind(address).await {
                Ok(listener) => Some(listener),
                Err(error) => {
                    let issue = if error.kind() == io::ErrorKind::AddrInUse {
                        "port_in_use"
                    } else {
                        "listen"
                    };
                    if request.save {
                        return Err(issue);
                    }
                    lan_issue = Some(issue);
                    None
                }
            }
        } else {
            None
        };

        let save = request.save;
        let replace_share = settings.share_root != self.share_root;
        let sharing = !self.share_root.is_empty();
        let has_receiver = address.is_some() || origin.is_some();
        let source = request.source;
        let new_source = (source != self.source).then(|| source.clone());
        if source.is_empty() && has_receiver {
            return Err("source");
        }
        let (settings, target, share, share_issue) = tokio::task::spawn_blocking(move || {
            let target = new_source
                .filter(|source| !source.is_empty())
                .map(|source| capture_target(&source).ok_or("source"))
                .transpose()?;
            let mut warning = None;
            let share = if replace_share {
                let root = open_share(&settings.share_root);
                match root {
                    Ok(root) => Some(root),
                    Err(_) if !save => {
                        warning = Some("share_root");
                        sharing.then_some(None)
                    }
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            if save {
                open_share(&settings.download_root).map_err(|_| "download_root")?;
                let store = Store::open().map_err(|_| "storage")?;
                let settings = store
                    .save_desktop_preferences(settings)
                    .map_err(|_| "storage")?;
                Ok((settings, target, share, warning))
            } else {
                Ok((settings, target, share, warning))
            }
        })
        .await
        .map_err(|_| "unavailable")??;
        self.source = source;
        if let Some(root) = &share {
            self.share_root = if root.is_some() {
                settings.share_root.clone()
            } else {
                String::new()
            };
        }
        desktop.configure_shared(target, share);
        desktop.configure_permissions(settings.permissions);
        desktop.configure_unattended(settings.unattended);
        if replace_lan && (listener.is_some() || address.is_none()) {
            self.lan = None;
            host.clear_challenges(false);
            desktop.clear_resume(false);
            if let Some((listener, address)) = listener.zip(address) {
                let (close, closed) = watch::channel(false);
                let owner = host.clone();
                tokio::spawn(async move {
                    if serve_http(listener, owner, closed).await.is_err() {
                        report(json!({"event":"lan_issue", "endpoint":address, "error":"listen"}));
                    }
                });
                self.lan = Some(Receiver {
                    value: address,
                    close,
                });
            }
        }

        let allow_shell = settings.allow_shell && origin.is_some() && crate::shell::ordinary_user();
        let directory_changed = self.directory.as_ref().map(|link| &link.value.origin)
            != origin.as_ref()
            || self
                .directory
                .as_ref()
                .is_some_and(|link| link.value.allow_shell != allow_shell);
        if directory_changed {
            self.directory = None;
            host.clear_challenges(true);
            desktop.clear_resume(true);
            host.allow_shell.store(allow_shell, Ordering::Release);
            if let Some(origin) = origin {
                let (close, closed) = watch::channel(false);
                let generation = request.request;
                tokio::spawn(directory_host::run(
                    host.clone(),
                    origin.clone(),
                    closed,
                    Some(generation),
                ));
                self.directory = Some(Receiver {
                    value: DirectoryReceiver {
                        origin,
                        allow_shell,
                        generation,
                    },
                    close,
                });
            }
        }

        report(json!({
            "event":"settings_result", "request":request.request, "settings":settings,
            "endpoint":self.lan.as_ref().map(|lan| lan.value.to_string()),
            "directory":self.directory.as_ref().map(|link| link.value.origin.as_str()),
            "generation":self.directory.as_ref().map(|link| link.value.generation),
            "lan_issue":lan_issue, "share_issue":share_issue,
        }));
        Ok(())
    }
}

fn open_share(path: &str) -> Result<Option<Arc<Root>>, &'static str> {
    if path.is_empty() {
        return Ok(None);
    }
    let root = Root::open(Path::new(path)).map_err(|_| "share_root")?;
    Ok(Some(Arc::new(root)))
}
