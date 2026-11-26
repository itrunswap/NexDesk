use nexdesk_core::DeviceId;
use nexdesk_directory::ServerOrigin;
use nexdesk_session::pairing::{AccessCode, HostIdentity};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use super::{Store, StoreError, delete_secret, entry, read_secret};

#[derive(Serialize)]
pub struct RecentDirectory {
    pub origin: String,
    pub device: String,
    pub label: String,
    pub group: String,
    pub identity: String,
    pub remembered: bool,
    pub connected_at: i64,
}

impl Store {
    pub fn directory_recent(&self) -> Result<Vec<RecentDirectory>, StoreError> {
        let mut statement = self.db.prepare(
            "SELECT r.origin, device, label, identity, remembered, connected_at, COALESCE(g.name,'')
             FROM id_recent r LEFT JOIN recent_group g ON g.origin=r.origin AND g.target=r.device
             ORDER BY connected_at DESC, r.origin, device LIMIT 32",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(RecentDirectory {
                origin: row.get(0)?,
                device: row.get(1)?,
                label: row.get(2)?,
                identity: row.get(3)?,
                remembered: row.get(4)?,
                connected_at: row.get(5)?,
                group: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn directory_pin(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
    ) -> Result<Option<HostIdentity>, StoreError> {
        self.directory_row(origin, device)?
            .map(|(identity, _)| identity.parse().map_err(StoreError::from))
            .transpose()
    }

    pub fn directory_code(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
    ) -> Result<Option<(HostIdentity, AccessCode)>, StoreError> {
        let Some((identity, true)) = self.directory_row(origin, device)? else {
            return Ok(None);
        };
        let pin = identity.parse()?;
        let secret = read_secret(&account(origin, device, pin))?;
        secret
            .map(|code| Ok((pin, AccessCode::parse(&code)?)))
            .transpose()
    }

    pub fn directory_connected(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
        pin: HostIdentity,
        remember: Option<&AccessCode>,
    ) -> Result<(), StoreError> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let previous = self.directory_row(origin, device)?;
        if let Some((identity, _)) = &previous
            && identity.parse::<HostIdentity>()? != pin
        {
            return Err(StoreError::Invalid);
        }
        let had_password = previous.is_some_and(|(_, remembered)| remembered);
        let account = account(origin, device, pin);
        let credential = match remember {
            Some(code) => entry(&account)
                .and_then(|entry| entry.set_secret(code.as_bytes()).map_err(StoreError::from)),
            None if had_password => delete_secret(&account),
            None => Ok(()),
        };
        let remembered = if credential.is_ok() {
            remember.is_some()
        } else {
            had_password
        };
        transaction.execute(
            "INSERT INTO id_recent (origin, device, label, identity, remembered, connected_at)
             VALUES (?1, ?2, ?2, ?3, ?4, unixepoch())
             ON CONFLICT(origin, device) DO UPDATE SET remembered=excluded.remembered,
             connected_at=excluded.connected_at",
            params![
                origin.as_str(),
                device.to_string(),
                pin.to_string(),
                remembered
            ],
        )?;
        transaction.commit()?;
        credential
    }

    pub fn directory_rename(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
        label: &str,
    ) -> Result<(), StoreError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
            return Err(StoreError::Invalid);
        }
        self.db.execute(
            "UPDATE id_recent SET label=?3 WHERE origin=?1 AND device=?2",
            params![origin.as_str(), device.to_string(), label],
        )?;
        Ok(())
    }

    pub fn directory_forget(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
        remove: bool,
    ) -> Result<(), StoreError> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some((pin, true)) = self.directory_row(origin, device)? {
            delete_secret(&account(origin, device, pin.parse()?))?;
        }
        transaction.execute(
            if remove {
                "DELETE FROM id_recent WHERE origin=?1 AND device=?2"
            } else {
                "UPDATE id_recent SET remembered=0 WHERE origin=?1 AND device=?2"
            },
            params![origin.as_str(), device.to_string()],
        )?;
        if remove {
            self.set_recent_group(origin.as_str(), &device.to_string(), "")?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn directory_row(
        &self,
        origin: &ServerOrigin,
        device: &DeviceId,
    ) -> Result<Option<(String, bool)>, StoreError> {
        Ok(self
            .db
            .query_row(
                "SELECT identity,remembered FROM id_recent WHERE origin=?1 AND device=?2",
                params![origin.as_str(), device.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }
}

fn account(origin: &ServerOrigin, device: &DeviceId, pin: HostIdentity) -> String {
    format!("id-v1/{origin}/{device}/{pin}")
}
