use std::{net::SocketAddr, path::Path};

use nexdesk_core::{DEFAULT_LAN_PORT, LanEndpoint, local_listener_ip};
use nexdesk_directory::ServerOrigin;
use nexdesk_session::collaboration::Capabilities;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{HostPreferences, Store, StoreError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopPermissions {
    pub request_control: bool,
    pub request_capabilities: Capabilities,
    pub device_authorization: bool,
    pub allow_control: bool,
    pub allowed_capabilities: Capabilities,
}

impl Default for DesktopPermissions {
    fn default() -> Self {
        Self {
            request_control: true,
            request_capabilities: Capabilities::AUDIO
                .with(Capabilities::CLIPBOARD)
                .with(Capabilities::TEXT),
            device_authorization: false,
            allow_control: true,
            allowed_capabilities: Capabilities::ALL.without(Capabilities::SHELL),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopPreferences {
    pub use_self_hosted: bool,
    pub directory: String,
    pub lan_allowed: bool,
    pub address: String,
    pub port: u16,
    pub source: String,
    pub share_root: String,
    pub allow_shell: bool,
    pub unattended: bool,
    pub download_root: String,
    pub quality: String,
    pub fps: u16,
    pub permissions: DesktopPermissions,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            use_self_hosted: false,
            directory: String::new(),
            lan_allowed: true,
            address: String::new(),
            port: DEFAULT_LAN_PORT,
            source: String::new(),
            share_root: String::new(),
            allow_shell: false,
            unattended: false,
            download_root: String::new(),
            quality: "auto".into(),
            fps: 30,
            permissions: DesktopPermissions::default(),
        }
    }
}

impl DesktopPreferences {
    pub fn validated(mut self) -> Result<Self, StoreError> {
        if !self.directory.trim().is_empty() {
            self.directory = self
                .directory
                .parse::<ServerOrigin>()
                .map_err(|_| StoreError::Invalid)?
                .to_string();
        } else if self.use_self_hosted {
            return Err(StoreError::Invalid);
        } else {
            self.directory.clear();
        }
        if self.port == 0
            || (!self.address.is_empty() && self.lan_endpoint(&self.address).is_err())
            || !(self.source.is_empty()
                || self.source == "picker"
                || self
                    .source
                    .strip_prefix("screen:")
                    .is_some_and(|id| id.len() <= 20 && id.parse::<u64>().is_ok()))
            || !valid_path(&self.share_root)
            || !valid_path(&self.download_root)
            || !matches!(self.quality.as_str(), "auto" | "clear" | "smooth")
            || !(10..=60).contains(&self.fps)
            || !Capabilities::ALL.without(Capabilities::SHELL).includes(
                self.permissions
                    .request_capabilities
                    .with(self.permissions.allowed_capabilities),
            )
        {
            return Err(StoreError::Invalid);
        }
        if !self.address.is_empty() {
            self.address = self.lan_endpoint(&self.address)?.ip().to_string();
        }
        Ok(self)
    }

    pub fn active_origin(&self, default: &str) -> Result<Option<ServerOrigin>, StoreError> {
        let value = if self.use_self_hosted {
            &self.directory
        } else {
            default
        };
        if value.is_empty() {
            return Ok(None);
        }
        Ok(Some(value.parse().map_err(|_| StoreError::Invalid)?))
    }

    pub fn lan_endpoint(&self, address: &str) -> Result<SocketAddr, StoreError> {
        let endpoint = LanEndpoint::parse(address, &self.port.to_string())
            .map_err(|_| StoreError::Invalid)?
            .socket_addr();
        if !local_listener_ip(endpoint.ip()) {
            return Err(StoreError::Invalid);
        }
        Ok(endpoint)
    }
}

fn valid_path(path: &str) -> bool {
    path.is_empty() || (path.len() <= 2048 && !path.contains('\0') && Path::new(path).is_absolute())
}

impl Store {
    pub fn desktop_preferences(&self) -> Result<DesktopPreferences, StoreError> {
        let settings = self
            .db
            .query_row(
                "SELECT use_self_hosted,directory,lan_allowed,address,port,source,
                        share_root,allow_shell,download_root,quality,fps
                 FROM desktop_preferences WHERE id=1",
                [],
                |row| {
                    Ok(DesktopPreferences {
                        use_self_hosted: row.get(0)?,
                        directory: row.get(1)?,
                        lan_allowed: row.get(2)?,
                        address: row.get(3)?,
                        port: row.get(4)?,
                        source: row.get(5)?,
                        share_root: row.get(6)?,
                        allow_shell: row.get(7)?,
                        download_root: row.get(8)?,
                        quality: row.get(9)?,
                        fps: row.get(10)?,
                        unattended: false,
                        permissions: DesktopPermissions::default(),
                    })
                },
            )
            .optional()?;
        if let Some(mut settings) = settings {
            settings.unattended = self.unattended_allowed()?;
            settings.permissions = self.desktop_permissions()?;
            return settings.validated();
        }

        let Some(HostPreferences {
            address,
            port,
            source,
            share_root,
            directory,
        }) = self.host_preferences()?
        else {
            return Ok(DesktopPreferences::default());
        };
        DesktopPreferences {
            use_self_hosted: !directory.is_empty(),
            directory,
            address,
            port,
            source,
            share_root,
            ..DesktopPreferences::default()
        }
        .validated()
    }

    fn desktop_permissions(&self) -> Result<DesktopPermissions, StoreError> {
        let saved = self
            .db
            .query_row(
                "SELECT request_control,request_capabilities,device_authorization,
                    allow_control,allowed_capabilities FROM desktop_permissions WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((request_control, request, device_authorization, allow_control, allowed)) = saved
        else {
            return Ok(DesktopPermissions::default());
        };
        Ok(DesktopPermissions {
            request_control,
            request_capabilities: Capabilities::from_bits(request).ok_or(StoreError::Invalid)?,
            device_authorization,
            allow_control,
            allowed_capabilities: Capabilities::from_bits(allowed).ok_or(StoreError::Invalid)?,
        })
    }

    pub fn save_desktop_preferences(
        &self,
        settings: DesktopPreferences,
    ) -> Result<DesktopPreferences, StoreError> {
        let settings = settings.validated()?;
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO desktop_preferences VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET use_self_hosted=excluded.use_self_hosted,
                directory=excluded.directory,lan_allowed=excluded.lan_allowed,
                address=excluded.address,port=excluded.port,source=excluded.source,
                share_root=excluded.share_root,allow_shell=excluded.allow_shell,
                download_root=excluded.download_root,quality=excluded.quality,fps=excluded.fps",
            params![
                settings.use_self_hosted,
                settings.directory,
                settings.lan_allowed,
                settings.address,
                settings.port,
                settings.source,
                settings.share_root,
                settings.allow_shell,
                settings.download_root,
                settings.quality,
                settings.fps,
            ],
        )?;
        transaction.execute(
            "INSERT INTO desktop_access_policy (id,unattended) VALUES (1,?1)
             ON CONFLICT(id) DO UPDATE SET unattended=excluded.unattended",
            [settings.unattended],
        )?;
        let permissions = settings.permissions;
        transaction.execute(
            "INSERT INTO desktop_permissions VALUES (1,?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET request_control=excluded.request_control,
                request_capabilities=excluded.request_capabilities,
                device_authorization=excluded.device_authorization,allow_control=excluded.allow_control,
                allowed_capabilities=excluded.allowed_capabilities",
            params![
                permissions.request_control,
                permissions.request_capabilities.bits(),
                permissions.device_authorization,
                permissions.allow_control,
                permissions.allowed_capabilities.bits(),
            ],
        )?;
        transaction.commit()?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_legacy_profile_and_remembers_inactive_directory() -> Result<(), StoreError> {
        let store = Store::open_at(Path::new(":memory:"))?;
        assert_eq!(store.desktop_preferences()?, DesktopPreferences::default());
        store.save_host_preferences(&HostPreferences {
            address: "192.168.12.15".into(),
            port: 55000,
            source: "screen:4".into(),
            share_root: String::new(),
            directory: "https://PRIVATE.example.test:443/".into(),
        })?;
        let mut migrated = store.desktop_preferences()?;
        assert!(migrated.use_self_hosted);
        assert_eq!(migrated.directory, "https://private.example.test");
        assert_eq!(migrated.port, 55000);
        assert_eq!(migrated.source, "screen:4");
        migrated.use_self_hosted = false;
        migrated.permissions.device_authorization = true;
        migrated.permissions.allow_control = false;
        migrated.permissions.allowed_capabilities = Capabilities::AUDIO;
        assert_eq!(store.save_desktop_preferences(migrated.clone())?, migrated);
        assert_eq!(store.desktop_preferences()?, migrated);
        assert_eq!(
            migrated
                .active_origin("https://default.example.test")?
                .map(|origin| origin.to_string()),
            Some("https://default.example.test".into())
        );
        assert!(migrated.active_origin("")?.is_none());
        Ok(())
    }

    #[test]
    fn rejects_unrestricted_listeners_and_invalid_settings() {
        for address in ["0.0.0.0", "::", "8.8.8.8", "ff02::1"] {
            assert!(
                DesktopPreferences {
                    address: address.into(),
                    ..DesktopPreferences::default()
                }
                .validated()
                .is_err()
            );
        }
        assert!(
            DesktopPreferences {
                use_self_hosted: true,
                directory: "http://192.168.1.10".into(),
                ..DesktopPreferences::default()
            }
            .validated()
            .is_err()
        );
        let mut settings = DesktopPreferences::default();
        settings.permissions.allowed_capabilities = Capabilities::SHELL;
        assert!(settings.validated().is_err());
    }
}
