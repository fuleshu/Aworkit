//! Replaceable query projections rebuilt only from committed semantic rows.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, params};

use crate::{LocalHistoryStore, StoreError};

/// A read-only semantic timeline row derived from canonical history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEntry {
    /// Stable canonical event identity.
    pub event_id: String,
    /// The source Chat identity.
    pub chat_id: String,
    /// The source branch identity.
    pub branch_id: String,
    /// The canonical sequence, never a wall-clock ordering surrogate.
    pub sequence: u64,
    /// Semantic event classification.
    pub kind: String,
}

/// A disposable SQLite projection database with no canonical write interface.
#[derive(Clone)]
pub struct ProjectionStore {
    connection: Arc<Mutex<Connection>>,
}

impl ProjectionStore {
    /// Opens a replaceable projection database that can be deleted and rebuilt.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS timeline_projection (
               event_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
               sequence INTEGER NOT NULL, kind TEXT NOT NULL,
               UNIQUE(chat_id, branch_id, sequence)
             ) STRICT;",
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Rebuilds one timeline from committed canonical ledger data, idempotently.
    pub fn rebuild_chat(
        &self,
        source: &LocalHistoryStore,
        chat_id: &str,
        branch_id: &str,
    ) -> Result<(), StoreError> {
        let canonical = source.committed_timeline(chat_id, branch_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM timeline_projection WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
        )?;
        for entry in canonical {
            transaction.execute(
                "INSERT INTO timeline_projection(event_id, chat_id, branch_id, sequence, kind) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![entry.event_id, entry.chat_id, entry.branch_id, i64::try_from(entry.sequence).map_err(|_| StoreError::InvalidStoredData)?, entry.kind],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Reads the evictable projection; absence is a rebuild condition, not data loss.
    pub fn timeline(
        &self,
        chat_id: &str,
        branch_id: &str,
    ) -> Result<Vec<TimelineEntry>, StoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)?;
        let mut statement = connection.prepare("SELECT event_id, chat_id, branch_id, sequence, kind FROM timeline_projection WHERE chat_id = ?1 AND branch_id = ?2 ORDER BY sequence")?;
        statement
            .query_map(params![chat_id, branch_id], |row| {
                Ok(TimelineEntry {
                    event_id: row.get(0)?,
                    chat_id: row.get(1)?,
                    branch_id: row.get(2)?,
                    sequence: u64::try_from(row.get::<_, i64>(3)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 0))?,
                    kind: row.get(4)?,
                })
            })?
            .map(|row| Ok(row?))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::*;
    use crate::{CommitBatch, Event};

    #[test]
    fn rebuilds_an_evictable_timeline_from_committed_rows() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aworkit-projections-{nonce}"));
        fs::create_dir_all(&root).expect("root");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        history
            .commit(&CommitBatch {
                chat_id: "chat_01".into(),
                branch_id: "main".into(),
                expected_head: 0,
                events: vec![Event {
                    event_id: "event_01".into(),
                    kind: "input.accepted".into(),
                    payload: json!({"schemaVersion": 1}),
                }],
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: vec![],
            })
            .expect("commit");
        let projection = ProjectionStore::open(root.join("projection.sqlite")).expect("projection");
        projection
            .rebuild_chat(&history, "chat_01", "main")
            .expect("rebuild");
        assert_eq!(
            projection.timeline("chat_01", "main").expect("timeline")[0].event_id,
            "event_01"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
