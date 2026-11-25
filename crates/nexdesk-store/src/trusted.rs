use nexdesk_directory::ServerOrigin;
use nexdesk_session::{
    collaboration::Capabilities,
    controller::{ControllerIdentity, ControllerKey},
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::{Store, StoreError, entry, read_secret};

const CONTROLLER_ACCOUNT: &str = "desktop-controller-ed25519-v1";
const APPROVAL_DAYS: i64 = 30;
const MAX_DEVICES: i64 = 32;

#[derive(Debug, Serialize)]
pub struct TrustedDevice {
    pub scope: String,
    pub identity: String,
    pub input: bool,
    pub capabilities: u32,
    pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub struct SessionEvent {
    pub time: i64,
    pub direction: String,
    pub mode: String,
    pub outcome: String,
}

pub fn controller_key() -> Result<ControllerKey, StoreError> {
    let store = Store::open()?;
    let _guard = Transaction::new_unchecked(&store.db, TransactionBehavior::Immediate)?;
    if let Some(secret) = read_secret(CONTROLLER_ACCOUNT)? {
        return Ok(ControllerKey::from_pkcs8(&secret)?);
    }
    let secret = ControllerKey::generate()?;
    entry(CONTROLLER_ACCOUNT)?.set_secret(&secret)?;
    Ok(ControllerKey::from_pkcs8(&secret)?)
}

fn valid_scope(scope: &str) -> bool {
    scope == "lan"
        || scope
            .parse::<ServerOrigin>()
            .is_ok_and(|origin| origin.as_str() == scope)
}

impl Store {
    pub fn unattended_allowed(&self) -> Result<bool, StoreError> {
        Ok(self
            .db
            .query_row(
                "SELECT unattended FROM desktop_access_policy WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn trust_controller(
        &self,
        scope: &str,
        identity: &ControllerIdentity,
        input: bool,
        capabilities: Capabilities,
    ) -> Result<(), StoreError> {
        if !valid_scope(scope) || capabilities.includes(Capabilities::SHELL) {
            return Err(StoreError::Invalid);
        }
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM trusted_controller WHERE expires_at<=unixepoch()",
            [],
        )?;
        let applied = transaction.execute(
            "INSERT INTO trusted_controller (scope,identity,input,capabilities,expires_at)
             SELECT ?1,?2,?3,?4,unixepoch()+86400*?5
             WHERE EXISTS(SELECT 1 FROM desktop_access_policy WHERE id=1 AND unattended=1)
                AND (EXISTS(SELECT 1 FROM trusted_controller WHERE scope=?1 AND identity=?2)
                    OR (SELECT count(*) FROM trusted_controller) < ?6)
             ON CONFLICT(scope,identity) DO UPDATE SET input=excluded.input,
                capabilities=excluded.capabilities,expires_at=excluded.expires_at",
            params![
                scope,
                identity.encoded(),
                input,
                capabilities.bits(),
                APPROVAL_DAYS,
                MAX_DEVICES
            ],
        )?;
        if applied != 1 {
            return Err(StoreError::Invalid);
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn controller_grant(
        &self,
        scope: &str,
        identity: &ControllerIdentity,
    ) -> Result<Option<(bool, Capabilities)>, StoreError> {
        let raw: Option<(bool, u32)> = self.db.query_row(
            "SELECT input,capabilities FROM trusted_controller WHERE scope=?1 AND identity=?2
                AND expires_at>unixepoch() AND EXISTS(SELECT 1 FROM desktop_access_policy WHERE id=1 AND unattended=1)",
            params![scope, identity.encoded()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        Ok(raw.and_then(|(input, bits)| {
            Capabilities::from_bits(bits).map(|caps| (input, caps.without(Capabilities::SHELL)))
        }))
    }

    pub fn trusted_devices(&self) -> Result<Vec<TrustedDevice>, StoreError> {
        let mut statement = self.db.prepare(
            "SELECT scope,identity,input,capabilities,expires_at FROM trusted_controller
             ORDER BY expires_at DESC LIMIT 32",
        )?;
        let devices = statement.query_map([], |row| {
            Ok(TrustedDevice {
                scope: row.get(0)?,
                identity: row.get(1)?,
                input: row.get(2)?,
                capabilities: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;
        devices
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn revoke_controller(
        &self,
        scope: &str,
        identity: &ControllerIdentity,
    ) -> Result<(), StoreError> {
        if !valid_scope(scope) {
            return Err(StoreError::Invalid);
        }
        self.db.execute(
            "DELETE FROM trusted_controller WHERE scope=?1 AND identity=?2",
            params![scope, identity.encoded()],
        )?;
        Ok(())
    }

    pub fn record_session(
        &self,
        direction: &str,
        mode: &str,
        outcome: &str,
    ) -> Result<(), StoreError> {
        if !matches!(direction, "incoming" | "outgoing")
            || !matches!(mode, "lan" | "id")
            || !matches!(
                outcome,
                "connected"
                    | "ended"
                    | "declined"
                    | "unreachable"
                    | "credentials"
                    | "identity"
                    | "transport"
                    | "revoked"
                    | "busy"
            )
        {
            return Err(StoreError::Invalid);
        }
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO session_event (time,direction,mode,outcome) VALUES (unixepoch(),?1,?2,?3)",
            params![direction, mode, outcome],
        )?;
        transaction.execute(
            "DELETE FROM session_event WHERE id <= (SELECT max(id)-128 FROM session_event)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn session_history(&self) -> Result<Vec<SessionEvent>, StoreError> {
        let mut statement = self.db.prepare(
            "SELECT time,direction,mode,outcome FROM session_event ORDER BY id DESC LIMIT 128",
        )?;
        let events = statement.query_map([], |row| {
            Ok(SessionEvent {
                time: row.get(0)?,
                direction: row.get(1)?,
                mode: row.get(2)?,
                outcome: row.get(3)?,
            })
        })?;
        events
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn clear_session_history(&self) -> Result<(), StoreError> {
        self.db.execute("DELETE FROM session_event", [])?;
        Ok(())
    }
}
