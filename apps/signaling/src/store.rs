use std::{error::Error, io, path::Path, sync::Mutex};

use nexdesk_core::DeviceId;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub(crate) struct Registry(Mutex<Connection>);

pub(crate) enum Enrollment {
    Registered(DeviceId),
    Denied,
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        let database = Connection::open(path)?;
        database.busy_timeout(std::time::Duration::from_secs(4))?;
        database.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS device (
                id TEXT PRIMARY KEY,
                registration_key TEXT NOT NULL UNIQUE,
                fingerprint TEXT NOT NULL,
                revoked INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        Ok(Self(Mutex::new(database)))
    }

    pub fn register(&self, key: &str, identity: &str, can_enroll: bool) -> Result<Enrollment> {
        let mut database = self
            .0
            .lock()
            .map_err(|_| io::Error::other("registry unavailable"))?;
        let transaction =
            database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let saved = transaction
            .query_row(
                "SELECT id, fingerprint, revoked FROM device WHERE registration_key = ?1",
                [key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((device, fingerprint, revoked)) = saved {
            return if fingerprint == identity && !revoked {
                Ok(Enrollment::Registered(device.parse()?))
            } else {
                Ok(Enrollment::Denied)
            };
        }
        let size: u32 =
            transaction.query_row("SELECT COUNT(*) FROM device", [], |row| row.get(0))?;
        if !can_enroll || size >= 10_000 {
            return Ok(Enrollment::Denied);
        }
        let random = SystemRandom::new();
        for _ in 0..16 {
            let mut bytes = [0; 8];
            random
                .fill(&mut bytes)
                .map_err(|_| io::Error::other("randomness unavailable"))?;
            let id = format!("{:09}", u64::from_le_bytes(bytes) % 1_000_000_000);
            let inserted = transaction.execute(
                "INSERT INTO device (id, registration_key, fingerprint) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO NOTHING",
                params![id, key, identity],
            )?;
            if inserted == 1 {
                transaction.commit()?;
                return Ok(Enrollment::Registered(id.parse()?));
            }
        }
        Err(io::Error::other("failed to allocate a device ID").into())
    }

    pub fn allowed(&self, device: &DeviceId) -> Result<bool> {
        let database = self
            .0
            .lock()
            .map_err(|_| io::Error::other("registry unavailable"))?;
        let allowed = database
            .query_row(
                "SELECT revoked = 0 FROM device WHERE id = ?1",
                [device.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        Ok(allowed)
    }

    pub fn revoke(&self, device: &DeviceId) -> Result<bool> {
        let database = self
            .0
            .lock()
            .map_err(|_| io::Error::other("registry unavailable"))?;
        let changed = database.execute(
            "UPDATE device SET revoked = 1 WHERE id = ?1",
            [device.to_string()],
        )?;
        Ok(changed == 1)
    }

    pub fn backup(&self, to: &Path) -> Result<()> {
        let path = to
            .to_str()
            .ok_or_else(|| io::Error::other("backup path is not UTF-8"))?;
        let database = self
            .0
            .lock()
            .map_err(|_| io::Error::other("registry unavailable"))?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        if let Err(error) = database.execute("VACUUM main INTO ?1", [path]) {
            let _ = std::fs::remove_file(to);
            return Err(error.into());
        }
        Ok(())
    }
}
