//! SurrealDB snapshot adapter over a host-owned, bounded query transport.
//! Calls are synchronous: async hosts must use a blocking worker, never block their executor.
use crate::{Error, Snapshot, Store};
use serde_json::{json, Value};

/// Executes one complete SurrealQL batch in the host-selected namespace/database.
/// Bind the supplied object as `$params`, preserving strings as strings.
/// Return successful statement values in order, including RETURN values.
/// Check EVERY statement (including COMMIT) before returning success. Map
/// BB_STALE_REVISION and aborted database write conflicts to Error::Conflict;
/// unknown/transport outcomes to Error::Storage. Never retry changed intent.
/// The host owns authentication, TLS, timeouts, encoded request/response budgets,
/// bounded workspace provisioning, and database backup/durability configuration.
pub trait SurrealQuery {
    fn query(&self, sql: &str, params: Value) -> Result<Vec<Value>, Error>;
}

pub struct SurrealStore<Q> {
    query: Q,
    snapshot_bytes: usize,
}

const READ: &str = "SELECT layout_version, revision, payload FROM type::record('bb_workspaces', $params.workspace);";
const CAS: &str = r#"
BEGIN TRANSACTION;
LET $key = type::record('bb_workspaces', $params.workspace);
LET $current = SELECT * FROM ONLY $key;
IF $params.exists {
    IF $current = NONE OR $current.layout_version != 1 OR $current.revision != $params.expected OR $current.payload != $params.previous {
        THROW 'BB_STALE_REVISION';
    };
} ELSE {
    IF $current != NONE { THROW 'BB_STALE_REVISION'; };
};
UPSERT $key CONTENT { layout_version: 1, revision: $params.revision, payload: $params.payload };
RETURN true;
COMMIT TRANSACTION;
"#;

impl<Q: SurrealQuery> SurrealStore<Q> {
    /// Reserve bb_workspaces for this adapter. No schema migration or workspace
    /// count limit is inferred. Configure byte limits to fit the actual transport:
    /// CAS sends both prior and next payload, plus JSON escaping overhead.
    pub fn new(query: Q, snapshot_bytes: usize) -> Result<Self, Error> {
        if snapshot_bytes == 0 {
            return Err(Error::Invalid("storage budgets"));
        }
        Ok(Self {
            query,
            snapshot_bytes,
        })
    }

    pub fn into_transport(self) -> Q {
        self.query
    }

    fn read(&self, workspace: &str) -> Result<Option<(Snapshot, String)>, Error> {
        identifier(workspace)?;
        let values = self.query.query(READ, json!({ "workspace": workspace }))?;
        let rows = values
            .last()
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Storage("invalid workspace query response".into()))?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != 1 {
            return Err(Error::Invalid("workspace rows"));
        }
        let row = &rows[0];
        if row["layout_version"].as_u64() != Some(1) {
            return Err(Error::Invalid("database schema"));
        }
        let payload = row["payload"]
            .as_str()
            .ok_or(Error::Invalid("snapshot encoding"))?;
        if payload.len() > self.snapshot_bytes {
            return Err(Error::Capacity("snapshot bytes"));
        }
        let snapshot: Snapshot =
            serde_json::from_str(payload).map_err(|_| Error::Invalid("snapshot encoding"))?;
        if snapshot.schema_version != 2 {
            return Err(Error::Invalid("snapshot schema"));
        }
        if row["revision"].as_str() != Some(snapshot.revision.to_string().as_str()) {
            return Err(Error::Invalid("snapshot revision"));
        }
        Ok(Some((snapshot, payload.to_owned())))
    }
}

impl<Q: SurrealQuery> Store for SurrealStore<Q> {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        Ok(self
            .read(workspace)?
            .map(|(snapshot, _)| snapshot)
            .unwrap_or_default())
    }

    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        identifier(workspace)?;
        if next.schema_version != 2 {
            return Err(Error::Invalid("snapshot schema"));
        }
        if next.revision != expected.checked_add(1).ok_or(Error::Conflict)? {
            return Err(Error::Conflict);
        }
        let payload =
            serde_json::to_string(&next).map_err(|_| Error::Invalid("snapshot encoding"))?;
        if payload.len() > self.snapshot_bytes {
            return Err(Error::Capacity("snapshot bytes"));
        }
        let current = self.read(workspace)?;
        if current
            .as_ref()
            .map_or(0, |(snapshot, _)| snapshot.revision)
            != expected
        {
            return Err(Error::Conflict);
        }
        let values = self.query.query(
            CAS,
            json!({
                "workspace": workspace,
                "exists": current.is_some(),
                "expected": expected.to_string(),
                "previous": current.map(|(_, payload)| payload),
                "revision": next.revision.to_string(),
                "payload": payload,
            }),
        )?;
        if !values.contains(&Value::Bool(true)) {
            return Err(Error::Storage("missing transaction confirmation".into()));
        }
        Ok(())
    }
}

fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
    {
        return Err(Error::Invalid("workspace identifier"));
    }
    Ok(())
}
