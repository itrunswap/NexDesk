//! SQLite metadata and OS-protected LAN credentials.
#![forbid(unsafe_code)]

mod directory;
mod preferences;
mod trusted;
pub use directory::RecentDirectory;
pub use preferences::{DesktopPermissions, DesktopPreferences};
pub use trusted::{TrustedDevice, controller_key};

use std::{fs, net::SocketAddr, path::Path, time::Duration};

use nexdesk_platform::DisplayMode;
use nexdesk_session::pairing::{AccessCode, HostIdentity, HostPairing, PairingError};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const CREDENTIAL_SERVICE: &str = "dev.nexdesk.lan";
const HOST_ACCOUNT: &str = "local-host-v2";
const DIRECTORY_HOST_ACCOUNT: &str = "local-id-host-v1";
const PROFILE_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the local data directory is unavailable")]
    DataDirectory,
    #[error("the local database is unavailable")]
    Database(#[from] rusqlite::Error),
    #[error("the system credential store is unavailable")]
    Credentials(#[from] keyring::Error),
    #[error("the saved device identity or credential is invalid")]
    Invalid,
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    Controller(#[from] nexdesk_session::controller::ControllerError),
}

#[derive(Serialize)]
pub struct RecentDevice {
    pub endpoint: String,
    pub address: String,
    pub port: u16,
    pub label: String,
    pub identity: String,
    pub remembered: bool,
    pub connected_at: i64,
}

#[derive(Deserialize, Serialize)]
pub struct HostPreferences {
    pub address: String,
    pub port: u16,
    pub source: String,
    #[serde(default)]
    pub share_root: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub directory: String,
}

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open() -> Result<Self, StoreError> {
        if let Some(path) = std::env::var_os("NEXDESK_DATA_DIR") {
            let dir = std::path::PathBuf::from(path);
            if !dir.is_absolute() {
                return Err(StoreError::DataDirectory);
            }
            fs::create_dir_all(&dir).map_err(|_| StoreError::DataDirectory)?;
            return Self::open_at(&dir.join("desktop.sqlite3"));
        }
        let dirs = directories::ProjectDirs::from("dev", "NexDesk", "NexDesk")
            .ok_or(StoreError::DataDirectory)?;
        let dir = dirs.data_local_dir();
        fs::create_dir_all(dir).map_err(|_| StoreError::DataDirectory)?;
        Self::open_at(&dir.join("desktop.sqlite3"))
    }

    fn open_at(path: &Path) -> Result<Self, StoreError> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS lan_recent (
                endpoint TEXT PRIMARY KEY, label TEXT NOT NULL, identity TEXT NOT NULL,
                remembered INTEGER NOT NULL, connected_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS lan_listener (
                id INTEGER PRIMARY KEY CHECK (id=1), address TEXT NOT NULL,
                port INTEGER NOT NULL, source TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS lan_display_modes (
                identity TEXT NOT NULL, display TEXT NOT NULL, width INTEGER NOT NULL,
                height INTEGER NOT NULL, refresh_millihz INTEGER NOT NULL,
                PRIMARY KEY(identity, display)
             );
             CREATE TABLE IF NOT EXISTS lan_share_root (
                id INTEGER PRIMARY KEY CHECK (id=1), path TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS id_host_preferences (
                id INTEGER PRIMARY KEY CHECK (id=1), origin TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS id_recent (
                origin TEXT NOT NULL, device TEXT NOT NULL, label TEXT NOT NULL,
                identity TEXT NOT NULL, remembered INTEGER NOT NULL, connected_at INTEGER NOT NULL,
                PRIMARY KEY(origin, device)
             );
             CREATE TABLE IF NOT EXISTS desktop_preferences (
                id INTEGER PRIMARY KEY CHECK (id=1), use_self_hosted INTEGER NOT NULL,
                directory TEXT NOT NULL, lan_allowed INTEGER NOT NULL, address TEXT NOT NULL,
                port INTEGER NOT NULL, source TEXT NOT NULL, share_root TEXT NOT NULL,
                allow_shell INTEGER NOT NULL, download_root TEXT NOT NULL,
                quality TEXT NOT NULL, fps INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS desktop_access_policy (
                id INTEGER PRIMARY KEY CHECK (id=1), unattended INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS desktop_permissions (
                id INTEGER PRIMARY KEY CHECK (id=1), request_control INTEGER NOT NULL,
                request_capabilities INTEGER NOT NULL CHECK (request_capabilities BETWEEN 0 AND 63),
                device_authorization INTEGER NOT NULL, allow_control INTEGER NOT NULL,
                allowed_capabilities INTEGER NOT NULL CHECK (allowed_capabilities BETWEEN 0 AND 63)
             );
             CREATE TABLE IF NOT EXISTS trusted_controller (
                scope TEXT NOT NULL, identity TEXT NOT NULL, input INTEGER NOT NULL,
                capabilities INTEGER NOT NULL, expires_at INTEGER NOT NULL,
                PRIMARY KEY(scope, identity)
             );
             CREATE TABLE IF NOT EXISTS session_event (
                id INTEGER PRIMARY KEY, time INTEGER NOT NULL, direction TEXT NOT NULL,
                mode TEXT NOT NULL, outcome TEXT NOT NULL
             );",
        )?;
        Ok(Self { db })
    }

    pub fn recent(&self) -> Result<Vec<RecentDevice>, StoreError> {
        let mut statement = self.db.prepare(
            "SELECT endpoint, label, identity, remembered, connected_at
             FROM lan_recent ORDER BY connected_at DESC, endpoint LIMIT 32",
        )?;
        let rows = statement.query_map([], |row| {
            let endpoint: String = row.get(0)?;
            let parsed = endpoint.parse::<SocketAddr>().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(RecentDevice {
                endpoint,
                address: parsed.ip().to_string(),
                port: parsed.port(),
                label: row.get(1)?,
                identity: row.get(2)?,
                remembered: row.get(3)?,
                connected_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn pin(&self, addr: SocketAddr) -> Result<Option<HostIdentity>, StoreError> {
        self.lan_row(addr)?
            .map(|(identity, _)| identity.parse().map_err(StoreError::from))
            .transpose()
    }

    fn lan_row(&self, addr: SocketAddr) -> Result<Option<(String, bool)>, StoreError> {
        Ok(self
            .db
            .query_row(
                "SELECT identity,remembered FROM lan_recent WHERE endpoint=?1",
                [addr.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn remembered(
        &self,
        addr: SocketAddr,
    ) -> Result<Option<(HostIdentity, AccessCode)>, StoreError> {
        let Some((identity, true)) = self.lan_row(addr)? else {
            return Ok(None);
        };
        let pin = identity.parse()?;
        let Ok(secret) = read_secret(&remote_account(addr, pin)) else {
            return Ok(None);
        };
        secret
            .map(|value| Ok((pin, AccessCode::parse(&value)?)))
            .transpose()
    }

    /// Record a session only after successful authentication and a decoded media frame.
    pub fn connected(
        &self,
        addr: SocketAddr,
        pin: HostIdentity,
        remember: Option<&AccessCode>,
    ) -> Result<(), StoreError> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let previous = self.lan_row(addr)?;
        if let Some((identity, _)) = &previous
            && identity.parse::<HostIdentity>()? != pin
        {
            return Err(StoreError::Invalid);
        }
        let had_password = previous.is_some_and(|(_, remembered)| remembered);
        let account = remote_account(addr, pin);
        let credential_result = match remember {
            Some(code) => entry(&account)
                .and_then(|entry| entry.set_secret(code.as_bytes()).map_err(StoreError::from)),
            None if had_password => delete_secret(&account),
            None => Ok(()),
        };
        let remembered = if credential_result.is_ok() {
            remember.is_some()
        } else {
            had_password
        };
        transaction.execute(
            "INSERT INTO lan_recent (endpoint, label, identity, remembered, connected_at)
             VALUES (?1, ?1, ?2, ?3, unixepoch())
             ON CONFLICT(endpoint) DO UPDATE SET remembered=excluded.remembered,
             connected_at=excluded.connected_at",
            params![addr.to_string(), pin.to_string(), remembered],
        )?;
        transaction.commit()?;
        credential_result
    }

    pub fn rename(&self, addr: SocketAddr, label: &str) -> Result<(), StoreError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
            return Err(StoreError::Invalid);
        }
        self.db.execute(
            "UPDATE lan_recent SET label=?2 WHERE endpoint=?1",
            params![addr.to_string(), label],
        )?;
        Ok(())
    }

    pub fn forget(&self, addr: SocketAddr, remove_recent: bool) -> Result<(), StoreError> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some((pin, true)) = self.lan_row(addr)? {
            delete_secret(&remote_account(addr, pin.parse()?))?;
        }
        transaction.execute(
            if remove_recent {
                "DELETE FROM lan_recent WHERE endpoint=?1"
            } else {
                "UPDATE lan_recent SET remembered=0 WHERE endpoint=?1"
            },
            [addr.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn host_preferences(&self) -> Result<Option<HostPreferences>, StoreError> {
        self.db
            .query_row(
                "SELECT l.address,l.port,l.source,COALESCE(r.path,''),COALESCE(d.origin,'')
                 FROM lan_listener l LEFT JOIN lan_share_root r ON r.id=l.id
                 LEFT JOIN id_host_preferences d ON d.id=l.id WHERE l.id=1",
                [],
                |row| {
                    Ok(HostPreferences {
                        address: row.get(0)?,
                        port: row.get(1)?,
                        source: row.get(2)?,
                        share_root: row.get(3)?,
                        directory: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn display_mode(
        &self,
        identity: HostIdentity,
        display: &str,
    ) -> Result<Option<DisplayMode>, StoreError> {
        Ok(self.db.query_row(
            "SELECT width,height,refresh_millihz FROM lan_display_modes WHERE identity=?1 AND display=?2",
            params![identity.to_string(), display],
            |row| Ok(DisplayMode { width: row.get(0)?, height: row.get(1)?, refresh_millihz: row.get(2)? }),
        ).optional()?.filter(|mode| mode.valid()))
    }

    pub fn confirm_display_mode(
        &self,
        identity: HostIdentity,
        display: &str,
        mode: DisplayMode,
    ) -> Result<(), StoreError> {
        if display.is_empty() || display.len() > 64 || !mode.valid() {
            return Err(StoreError::Invalid);
        }
        self.db.execute(
            "INSERT INTO lan_display_modes (identity,display,width,height,refresh_millihz) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(identity,display) DO UPDATE SET width=excluded.width,height=excluded.height,refresh_millihz=excluded.refresh_millihz",
            params![identity.to_string(), display, mode.width, mode.height, mode.refresh_millihz],
        )?;
        Ok(())
    }

    pub fn save_host_preferences(&self, settings: &HostPreferences) -> Result<(), StoreError> {
        let origin = settings.directory.trim();
        if !origin.is_empty() {
            origin
                .parse::<nexdesk_directory::ServerOrigin>()
                .map_err(|_| StoreError::Invalid)?;
        }
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO lan_listener (id,address,port,source) VALUES (1,?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET address=excluded.address,port=excluded.port,source=excluded.source",
            params![settings.address, settings.port, settings.source],
        )?;
        transaction.execute(
            "INSERT INTO lan_share_root (id,path) VALUES (1,?1)
             ON CONFLICT(id) DO UPDATE SET path=excluded.path",
            [&settings.share_root],
        )?;
        transaction.execute(
            "INSERT INTO id_host_preferences (id,origin) VALUES (1,?1)
             ON CONFLICT(id) DO UPDATE SET origin=excluded.origin",
            [origin],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

pub fn local_host() -> Result<(AccessCode, HostPairing), StoreError> {
    host_credential(HOST_ACCOUNT)
}

pub fn directory_host() -> Result<(AccessCode, HostPairing), StoreError> {
    host_credential(DIRECTORY_HOST_ACCOUNT)
}

fn host_credential(account: &str) -> Result<(AccessCode, HostPairing), StoreError> {
    let store = Store::open()?;
    let _lock = Transaction::new_unchecked(&store.db, TransactionBehavior::Immediate)?;
    if let Some(bytes) = read_secret(account)? {
        let (version, payload) = bytes.split_first().ok_or(StoreError::Invalid)?;
        if *version != PROFILE_VERSION || payload.len() <= 6 {
            return Err(StoreError::Invalid);
        }
        let (code, setup) = payload.split_at(6);
        let code = AccessCode::parse(code)?;
        let pairing = HostPairing::from_setup(&code, setup)?;
        return Ok((code, pairing));
    }
    let code = AccessCode::generate()?;
    let pairing = HostPairing::enroll(&code)?;
    save_host_credential(account, &code, &pairing)?;
    Ok((code, pairing))
}

pub fn save_local_host(code: &AccessCode, pairing: &HostPairing) -> Result<(), StoreError> {
    save_host_credential(HOST_ACCOUNT, code, pairing)
}

pub fn save_directory_host(code: &AccessCode, pairing: &HostPairing) -> Result<(), StoreError> {
    save_host_credential(DIRECTORY_HOST_ACCOUNT, code, pairing)
}

fn save_host_credential(
    account: &str,
    code: &AccessCode,
    pairing: &HostPairing,
) -> Result<(), StoreError> {
    let setup = pairing.export_setup();
    let mut payload = Zeroizing::new(Vec::with_capacity(7 + setup.len()));
    payload.push(PROFILE_VERSION);
    payload.extend_from_slice(code.as_bytes());
    payload.extend_from_slice(&setup);
    entry(account)?.set_secret(&payload)?;
    Ok(())
}

fn remote_account(addr: SocketAddr, identity: HostIdentity) -> String {
    format!("remote:{addr}:{identity}")
}

fn entry(account: &str) -> Result<keyring::Entry, StoreError> {
    let service = if let Some(namespace) = std::env::var_os("NEXDESK_CREDENTIAL_NAMESPACE") {
        let namespace = namespace.to_str().ok_or(StoreError::Invalid)?;
        if namespace.is_empty()
            || namespace.len() > 48
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(StoreError::Invalid);
        }
        format!("{CREDENTIAL_SERVICE}.{namespace}")
    } else {
        CREDENTIAL_SERVICE.to_owned()
    };
    keyring::Entry::new(&service, account).map_err(StoreError::from)
}

fn read_secret(account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
    match entry(account)?.get_secret() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn delete_secret(account: &str) -> Result<(), StoreError> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
