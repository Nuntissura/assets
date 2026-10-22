//! Opt-in local-file durability. Hosts own file permissions and authorization.
use crate::{Error, Snapshot, Store};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{path::Path, time::Duration};

pub struct SqliteStore {
    connection: Connection,
    workspaces: usize,
    snapshot_bytes: usize,
}

impl SqliteStore {
    /// Open a dedicated CRM database on a local filesystem. Parent must exist.
    /// The budgets apply on every load/write and are not inferred from database contents.
    pub fn open(
        path: impl AsRef<Path>,
        workspaces: usize,
        snapshot_bytes: usize,
    ) -> Result<Self, Error> {
        if workspaces == 0 || snapshot_bytes == 0 {
            return Err(Error::Invalid("storage budgets"));
        }
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err(Error::Invalid("durable database path"));
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(storage)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(storage)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let version: u32 = transaction
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(storage)?;
        let application: u32 = transaction
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(storage)?;
        if version == 0 && application == 0 {
            let tables: i64 = transaction
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            if tables != 0 {
                return Err(Error::Invalid("non-CRM database"));
            }
            transaction.execute_batch(
                "CREATE TABLE bb_workspaces (workspace TEXT PRIMARY KEY NOT NULL, revision TEXT NOT NULL, snapshot BLOB NOT NULL);
                 PRAGMA application_id = 1111642674;
                 PRAGMA user_version = 1;"
            ).map_err(storage)?;
        } else if version != 1 || application != 1111642674 {
            return Err(Error::Invalid("database schema"));
        }
        transaction.commit().map_err(storage)?;
        Ok(Self {
            connection,
            workspaces,
            snapshot_bytes,
        })
    }
}

fn storage(error: rusqlite::Error) -> Error {
    Error::Storage(error.to_string())
}

fn read(connection: &Connection, workspace: &str, budget: usize) -> Result<Snapshot, Error> {
    // Inspect size before retrieving/allocating the blob; read both from one statement.
    let row = connection.query_row(
        "SELECT revision, length(snapshot), CASE WHEN length(snapshot) <= ?2 THEN snapshot ELSE NULL END FROM bb_workspaces WHERE workspace = ?1",
        params![workspace, i64::try_from(budget).unwrap_or(i64::MAX)],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<Vec<u8>>>(2)?)),
    ).optional().map_err(storage)?;
    let Some((revision, length, bytes)) = row else {
        return Ok(Snapshot::default());
    };
    if length < 0 || length as u64 > budget as u64 {
        return Err(Error::Capacity("snapshot bytes"));
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes.ok_or(Error::Invalid("snapshot blob"))?)
        .map_err(|_| Error::Invalid("snapshot encoding"))?;
    if snapshot.schema_version != 2 {
        return Err(Error::Invalid("snapshot schema"));
    }
    if snapshot.revision.to_string() != revision {
        return Err(Error::Invalid("snapshot revision"));
    }
    Ok(snapshot)
}

impl Store for SqliteStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        read(&self.connection, workspace, self.snapshot_bytes)
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        if next.schema_version != 2 {
            return Err(Error::Invalid("snapshot schema"));
        }
        if next.revision != expected.checked_add(1).ok_or(Error::Conflict)? {
            return Err(Error::Conflict);
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| Error::Invalid("snapshot encoding"))?;
        if bytes.len() > self.snapshot_bytes {
            return Err(Error::Capacity("snapshot bytes"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        if read(&tx, workspace, self.snapshot_bytes)?.revision != expected {
            return Err(Error::Conflict);
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bb_workspaces WHERE workspace=?1)",
                [workspace],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !exists {
            let count: i64 = tx
                .query_row("SELECT count(*) FROM bb_workspaces", [], |r| r.get(0))
                .map_err(storage)?;
            if count as u64 >= self.workspaces as u64 {
                return Err(Error::Capacity("workspaces"));
            }
        }
        tx.execute(
            "INSERT INTO bb_workspaces(workspace,revision,snapshot) VALUES (?1,?2,?3) ON CONFLICT(workspace) DO UPDATE SET revision=excluded.revision,snapshot=excluded.snapshot",
            params![workspace, next.revision.to_string(), bytes],
        ).map_err(storage)?;
        tx.commit().map_err(storage)
    }
}
