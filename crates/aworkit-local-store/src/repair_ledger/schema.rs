//! Dedicated `SQLite` schema for compact long-lived repair evidence metadata.

use rusqlite::Connection;

use super::RepairLedgerError;

pub(super) const SCHEMA_VERSION: i32 = 3;

pub(super) fn configure(connection: &Connection) -> Result<(), RepairLedgerError> {
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA busy_timeout=5000;",
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn ensure(connection: &Connection) -> Result<(), RepairLedgerError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS error_groups (
             fingerprint TEXT PRIMARY KEY,
             ledger_version INTEGER NOT NULL CHECK(ledger_version > 0),
             status TEXT NOT NULL,
             occurrence_count INTEGER NOT NULL CHECK(occurrence_count > 0),
             first_seen_epoch_ms INTEGER NOT NULL CHECK(first_seen_epoch_ms >= 0),
             last_seen_epoch_ms INTEGER NOT NULL CHECK(last_seen_epoch_ms >= 0),
             active_candidate_id TEXT,
             active_candidate_version INTEGER,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL
         ) STRICT;
         CREATE INDEX IF NOT EXISTS error_group_status
             ON error_groups(status, last_seen_epoch_ms DESC);

         CREATE TABLE IF NOT EXISTS error_occurrences (
             occurrence_id TEXT PRIMARY KEY,
             fingerprint TEXT NOT NULL,
             observed_at_epoch_ms INTEGER NOT NULL CHECK(observed_at_epoch_ms >= 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS occurrence_group_time
             ON error_occurrences(fingerprint, observed_at_epoch_ms, occurrence_id);

         CREATE TABLE IF NOT EXISTS diagnoses (
             diagnosis_id TEXT PRIMARY KEY,
             fingerprint TEXT NOT NULL,
             recorded_at_epoch_ms INTEGER NOT NULL CHECK(recorded_at_epoch_ms >= 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS workarounds (
             workaround_id TEXT PRIMARY KEY,
             fingerprint TEXT NOT NULL,
             recorded_at_epoch_ms INTEGER NOT NULL CHECK(recorded_at_epoch_ms >= 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint)
         ) STRICT;

         CREATE TABLE IF NOT EXISTS repair_candidates (
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL CHECK(candidate_version > 0),
             fingerprint TEXT NOT NULL,
             candidate_hash TEXT NOT NULL,
             prepared_at_epoch_ms INTEGER NOT NULL CHECK(prepared_at_epoch_ms >= 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             PRIMARY KEY(candidate_id, candidate_version),
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS candidate_group_version
             ON repair_candidates(fingerprint, candidate_id, candidate_version DESC);

         CREATE TABLE IF NOT EXISTS candidate_disclosures (
             disclosure_id TEXT PRIMARY KEY,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             UNIQUE(candidate_id, candidate_version),
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS candidate_rejections (
             rejection_id TEXT PRIMARY KEY,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS restart_batons (
             baton_id TEXT PRIMARY KEY,
             fingerprint TEXT NOT NULL,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             UNIQUE(candidate_id, candidate_version),
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint),
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS verification_starts (
             verification_id TEXT PRIMARY KEY,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS verifications (
             verification_id TEXT PRIMARY KEY,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(verification_id) REFERENCES verification_starts(verification_id),
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS rollbacks (
             rollback_id TEXT PRIMARY KEY,
             candidate_id TEXT NOT NULL,
             candidate_version INTEGER NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(candidate_id, candidate_version)
                 REFERENCES repair_candidates(candidate_id, candidate_version)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS regressions (
             regression_id TEXT PRIMARY KEY,
             fingerprint TEXT NOT NULL,
             occurrence_id TEXT NOT NULL,
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL,
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint),
             FOREIGN KEY(occurrence_id) REFERENCES error_occurrences(occurrence_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS evidence_tombstones (
             tombstone_id TEXT PRIMARY KEY,
             artifact_id TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             recorded_at_epoch_ms INTEGER NOT NULL CHECK(recorded_at_epoch_ms >= 0),
             record_json TEXT NOT NULL,
             record_hash TEXT NOT NULL
         ) STRICT;
         CREATE INDEX IF NOT EXISTS evidence_tombstone_identity
             ON evidence_tombstones(artifact_id, content_hash, recorded_at_epoch_ms DESC);

         CREATE TABLE IF NOT EXISTS repair_transitions (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             fingerprint TEXT NOT NULL,
             from_status TEXT,
             to_status TEXT NOT NULL,
             kind TEXT NOT NULL,
             occurred_at_epoch_ms INTEGER NOT NULL CHECK(occurred_at_epoch_ms >= 0),
             previous_transition_hash TEXT,
             record_json TEXT NOT NULL,
             transition_hash TEXT NOT NULL,
             FOREIGN KEY(fingerprint) REFERENCES error_groups(fingerprint)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS repair_transition_group
             ON repair_transitions(fingerprint, sequence);

         CREATE TABLE IF NOT EXISTS repair_transition_head (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             sequence INTEGER NOT NULL CHECK(sequence > 0),
             transition_hash TEXT NOT NULL
         ) STRICT;

         CREATE TABLE IF NOT EXISTS repair_operations (
             operation_id TEXT PRIMARY KEY,
             request_hash TEXT NOT NULL,
             response_json TEXT NOT NULL,
             response_hash TEXT NOT NULL,
             created_at_epoch_ms INTEGER NOT NULL CHECK(created_at_epoch_ms >= 0)
         ) STRICT;

         CREATE TABLE IF NOT EXISTS core_event_groups (
             group_id TEXT PRIMARY KEY,
             current_sequence INTEGER NOT NULL CHECK(current_sequence > 0),
             head_event_hash TEXT NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS core_events (
             global_sequence INTEGER PRIMARY KEY CHECK(global_sequence > 0),
             group_id TEXT NOT NULL,
             group_sequence INTEGER NOT NULL CHECK(group_sequence > 0),
             operation_id TEXT NOT NULL,
             event_fingerprint TEXT NOT NULL,
             occurred_at_epoch_ms INTEGER NOT NULL CHECK(occurred_at_epoch_ms >= 0),
             canonical_event_json TEXT NOT NULL,
             event_content_hash TEXT NOT NULL,
             previous_group_event_hash TEXT,
             previous_global_event_hash TEXT,
             event_hash TEXT NOT NULL,
             UNIQUE(group_id, group_sequence),
             FOREIGN KEY(group_id) REFERENCES core_event_groups(group_id)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS core_event_group_sequence
             ON core_events(group_id, group_sequence);
         CREATE TABLE IF NOT EXISTS core_event_meta (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             current_global_version INTEGER NOT NULL CHECK(current_global_version >= 0),
             head_event_hash TEXT
         ) STRICT;
         INSERT INTO core_event_meta(singleton, current_global_version, head_event_hash)
             VALUES (1, 0, NULL) ON CONFLICT(singleton) DO NOTHING;
         CREATE TABLE IF NOT EXISTS core_event_operations (
             operation_id TEXT PRIMARY KEY,
             request_hash TEXT NOT NULL,
             receipt_json TEXT NOT NULL,
             receipt_hash TEXT NOT NULL,
             created_at_epoch_ms INTEGER NOT NULL CHECK(created_at_epoch_ms >= 0)
         ) STRICT;",
    )?;
    migrate_operation_hash(connection)?;
    migrate_core_event_batches(connection)?;
    connection.execute_batch(
        "INSERT INTO repair_transition_head(singleton, sequence, transition_hash)
             SELECT 1, sequence, transition_hash FROM repair_transitions
             ORDER BY sequence DESC LIMIT 1
             ON CONFLICT(singleton) DO NOTHING;
         PRAGMA user_version=3;",
    )?;
    Ok(())
}

fn migrate_core_event_batches(connection: &Connection) -> Result<(), RepairLedgerError> {
    let unique_indexes = {
        let mut statement = connection.prepare("PRAGMA index_list(core_events)")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? != 0))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut singular_only = false;
    for (index, unique) in unique_indexes {
        if !unique {
            continue;
        }
        let sql = format!("PRAGMA index_info('{index}')");
        let mut statement = connection.prepare(&sql)?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<Result<Vec<_>, _>>()?;
        if columns == ["operation_id"] {
            singular_only = true;
            break;
        }
    }
    if singular_only {
        connection.execute_batch(
            "DROP INDEX IF EXISTS core_event_group_sequence;
             ALTER TABLE core_events RENAME TO core_events_singular_legacy;
             CREATE TABLE core_events (
                 global_sequence INTEGER PRIMARY KEY CHECK(global_sequence > 0),
                 group_id TEXT NOT NULL,
                 group_sequence INTEGER NOT NULL CHECK(group_sequence > 0),
                 operation_id TEXT NOT NULL,
                 event_fingerprint TEXT NOT NULL,
                 occurred_at_epoch_ms INTEGER NOT NULL CHECK(occurred_at_epoch_ms >= 0),
                 canonical_event_json TEXT NOT NULL,
                 event_content_hash TEXT NOT NULL,
                 previous_group_event_hash TEXT,
                 previous_global_event_hash TEXT,
                 event_hash TEXT NOT NULL,
                 UNIQUE(group_id, group_sequence),
                 FOREIGN KEY(group_id) REFERENCES core_event_groups(group_id)
             ) STRICT;
             INSERT INTO core_events
                 SELECT * FROM core_events_singular_legacy;
             DROP TABLE core_events_singular_legacy;
             CREATE INDEX core_event_group_sequence
                 ON core_events(group_id, group_sequence);",
        )?;
    }
    Ok(())
}

fn migrate_operation_hash(connection: &Connection) -> Result<(), RepairLedgerError> {
    let has_hash = {
        let mut statement = connection.prepare("PRAGMA table_info(repair_operations)")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "response_hash")
    };
    if !has_hash {
        connection.execute_batch(
            "ALTER TABLE repair_operations
                 ADD COLUMN response_hash TEXT NOT NULL DEFAULT 'legacy-unverified';",
        )?;
    }
    Ok(())
}
