//! Versioned SQLite schema shared by semantic history and artifact metadata.

use std::{fs, path::Path};

use rusqlite::{Connection, OptionalExtension};

use crate::StoreError;

pub(crate) const HISTORY_SCHEMA_VERSION: i32 = 3;

pub(crate) fn open_history_database(path: &Path) -> Result<Connection, StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;",
    )?;
    ensure_history_schema(&connection)?;
    Ok(connection)
}

pub(crate) fn ensure_history_schema(connection: &Connection) -> Result<(), StoreError> {
    let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > HISTORY_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedStorageVersion {
            found: u32::try_from(found).unwrap_or(u32::MAX),
            supported: u32::try_from(HISTORY_SCHEMA_VERSION).expect("positive schema version"),
        });
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_streams (
             chat_id TEXT NOT NULL, branch_id TEXT NOT NULL, run_id TEXT NOT NULL DEFAULT '',
             head_sequence INTEGER NOT NULL, aggregate_version INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS semantic_events (
             event_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             sequence INTEGER NOT NULL, schema_version INTEGER NOT NULL DEFAULT 1,
             kind TEXT NOT NULL, payload TEXT NOT NULL,
             UNIQUE (chat_id, branch_id, sequence),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS attempts (
             attempt_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             operation_id TEXT NOT NULL, ordinal INTEGER NOT NULL, outcome_class TEXT NOT NULL,
             UNIQUE (chat_id, branch_id, operation_id, ordinal),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS checkpoints (
             chat_id TEXT NOT NULL, branch_id TEXT NOT NULL, committed_sequence INTEGER NOT NULL,
             reducer_version TEXT NOT NULL, state_hash TEXT NOT NULL, frozen_snapshot_ref TEXT,
             PRIMARY KEY (chat_id, branch_id, committed_sequence),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS deduplication (
             key_type TEXT NOT NULL, key TEXT NOT NULL, request_hash TEXT NOT NULL,
             chat_id TEXT NOT NULL DEFAULT '', branch_id TEXT NOT NULL DEFAULT '',
             receipt TEXT NOT NULL, PRIMARY KEY (key_type, key)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS delivery_outbox (
             outbox_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
             commit_sequence INTEGER NOT NULL, delivery_cursor INTEGER NOT NULL DEFAULT 0,
             destination TEXT NOT NULL, schema_version INTEGER NOT NULL DEFAULT 1,
             payload TEXT NOT NULL, payload_hash TEXT NOT NULL DEFAULT '',
             delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN (0, 1)),
             FOREIGN KEY (chat_id, branch_id) REFERENCES chat_streams(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS prepared_artifacts (
             token_id TEXT PRIMARY KEY, artifact_id TEXT NOT NULL, content_hash TEXT NOT NULL,
             byte_size INTEGER NOT NULL, media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
             staging_generation INTEGER NOT NULL DEFAULT 1,
             prepared_at_epoch_ms INTEGER NOT NULL DEFAULT 0, finalized_event_id TEXT
         ) STRICT;
         CREATE TABLE IF NOT EXISTS artifacts (
             artifact_id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, byte_size INTEGER NOT NULL,
             media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
             created_generation INTEGER NOT NULL DEFAULT 1,
             created_at_epoch_ms INTEGER NOT NULL DEFAULT 0,
             retention_class TEXT NOT NULL DEFAULT 'chat',
             availability TEXT NOT NULL DEFAULT 'available'
         ) STRICT;
         CREATE TABLE IF NOT EXISTS artifact_references (
             artifact_id TEXT NOT NULL, origin_event_id TEXT NOT NULL,
             PRIMARY KEY (artifact_id, origin_event_id),
             FOREIGN KEY (artifact_id) REFERENCES artifacts(artifact_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS store_state (
             key TEXT PRIMARY KEY, value TEXT NOT NULL
         ) STRICT;",
    )?;

    migrate_legacy_artifact_tables(connection)?;

    add_column_if_missing(
        connection,
        "chat_streams",
        "run_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        connection,
        "chat_streams",
        "aggregate_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        connection,
        "semantic_events",
        "schema_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        connection,
        "deduplication",
        "chat_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        connection,
        "deduplication",
        "branch_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        connection,
        "delivery_outbox",
        "delivery_cursor",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        connection,
        "delivery_outbox",
        "schema_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        connection,
        "delivery_outbox",
        "payload_hash",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        connection,
        "prepared_artifacts",
        "prepared_at_epoch_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        connection,
        "artifacts",
        "created_at_epoch_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    connection.execute_batch(
        "UPDATE chat_streams SET run_id = chat_id WHERE run_id = '';
         UPDATE chat_streams SET aggregate_version = head_sequence WHERE aggregate_version = 0;
         UPDATE delivery_outbox SET delivery_cursor = rowid WHERE delivery_cursor = 0;
         CREATE UNIQUE INDEX IF NOT EXISTS delivery_outbox_cursor
             ON delivery_outbox(delivery_cursor);
         CREATE INDEX IF NOT EXISTS delivery_outbox_pending
             ON delivery_outbox(delivered, delivery_cursor);
         CREATE INDEX IF NOT EXISTS semantic_events_stream
             ON semantic_events(chat_id, branch_id, sequence);
         CREATE INDEX IF NOT EXISTS prepared_artifacts_age
             ON prepared_artifacts(finalized_event_id, prepared_at_epoch_ms);
         PRAGMA user_version = 3;",
    )?;
    Ok(())
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == column) {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn migrate_legacy_artifact_tables(connection: &Connection) -> Result<(), StoreError> {
    let artifact_columns = table_columns(connection, "artifacts")?;
    if artifact_columns
        .iter()
        .any(|column| column == "origin_event_id")
    {
        connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE IF EXISTS artifact_references;
             ALTER TABLE artifacts RENAME TO artifacts_legacy;
             CREATE TABLE artifacts (
               artifact_id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, byte_size INTEGER NOT NULL,
               media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
               created_generation INTEGER NOT NULL DEFAULT 1,
               retention_class TEXT NOT NULL DEFAULT 'chat',
               availability TEXT NOT NULL DEFAULT 'available'
             ) STRICT;
             INSERT INTO artifacts(artifact_id, content_hash, byte_size, media_type, logical_name)
               SELECT artifact_id, content_hash, byte_size, media_type, logical_name
               FROM artifacts_legacy;
             CREATE TABLE artifact_references (
               artifact_id TEXT NOT NULL, origin_event_id TEXT NOT NULL,
               PRIMARY KEY (artifact_id, origin_event_id),
               FOREIGN KEY (artifact_id) REFERENCES artifacts(artifact_id)
             ) STRICT;
             INSERT OR IGNORE INTO artifact_references(artifact_id, origin_event_id)
               SELECT artifact_id, origin_event_id FROM artifacts_legacy
               WHERE origin_event_id IS NOT NULL;
             DROP TABLE artifacts_legacy;
             PRAGMA foreign_keys = ON;",
        )?;
    }

    let prepared_columns = table_columns(connection, "prepared_artifacts")?;
    if !prepared_columns
        .iter()
        .any(|column| column == "staging_generation")
    {
        connection.execute_batch(
            "ALTER TABLE prepared_artifacts RENAME TO prepared_artifacts_legacy;
             CREATE TABLE prepared_artifacts (
               token_id TEXT PRIMARY KEY, artifact_id TEXT NOT NULL, content_hash TEXT NOT NULL,
               byte_size INTEGER NOT NULL, media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
               staging_generation INTEGER NOT NULL DEFAULT 1, finalized_event_id TEXT
             ) STRICT;
             INSERT INTO prepared_artifacts(
               token_id, artifact_id, content_hash, byte_size, media_type, logical_name,
               finalized_event_id
             ) SELECT token_id, artifact_id, content_hash, byte_size, media_type, logical_name,
                      finalized_event_id
                 FROM prepared_artifacts_legacy;
             DROP TABLE prepared_artifacts_legacy;",
        )?;
    }
    Ok(())
}

pub(crate) fn sqlite_integrity(connection: &Connection) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn schema_version(connection: &Connection) -> Result<i32, StoreError> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

pub(crate) fn quarantine_reason(connection: &Connection) -> Result<Option<String>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT value FROM store_state WHERE key = 'quarantine_reason'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn quarantine(connection: &Connection, reason: &str) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO store_state(key, value) VALUES ('quarantine_reason', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [reason],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aworkit-{label}-{nonce}.sqlite"))
    }

    #[test]
    fn migrates_legacy_history_columns_without_losing_semantic_rows() {
        let path = path("legacy-history");
        let legacy = Connection::open(&path).expect("legacy");
        legacy
            .execute_batch(
                "CREATE TABLE chat_streams (
                   chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
                   head_sequence INTEGER NOT NULL,
                   PRIMARY KEY(chat_id, branch_id)
                 ) STRICT;
                 CREATE TABLE semantic_events (
                   event_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
                   UNIQUE(chat_id, branch_id, sequence)
                 ) STRICT;
                 INSERT INTO chat_streams VALUES ('chat_01', 'main', 1);
                 INSERT INTO semantic_events VALUES (
                   'event_01', 'chat_01', 'main', 1, 'input.accepted',
                   '{\"schemaVersion\":1}'
                 );
                 PRAGMA user_version = 1;",
            )
            .expect("legacy schema");
        drop(legacy);

        let migrated = open_history_database(&path).expect("migration");
        let stream: (String, i64, i64) = migrated
            .query_row(
                "SELECT run_id, head_sequence, aggregate_version FROM chat_streams",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("stream");
        assert_eq!(stream, ("chat_01".into(), 1, 1));
        let event: (String, i64) = migrated
            .query_row(
                "SELECT event_id, schema_version FROM semantic_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("event");
        assert_eq!(event, ("event_01".into(), 1));
        assert_eq!(
            schema_version(&migrated).expect("version"),
            HISTORY_SCHEMA_VERSION
        );
        drop(migrated);
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn refuses_newer_history_schema_without_downgrading_it() {
        let path = path("future-history");
        let future = Connection::open(&path).expect("future");
        future
            .execute_batch("PRAGMA user_version = 99;")
            .expect("future version");
        drop(future);
        assert!(matches!(
            open_history_database(&path),
            Err(StoreError::UnsupportedStorageVersion {
                found: 99,
                supported: 3
            })
        ));
        let unchanged = Connection::open(&path).expect("unchanged");
        assert_eq!(schema_version(&unchanged).expect("version"), 99);
        drop(unchanged);
        fs::remove_file(path).expect("cleanup");
    }
}
