//! Disposable, paginated query projections rebuilt from canonical local history.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::{LocalHistoryStore, StoreError, maintenance::MaintenanceGate};

const PROJECTION_SCHEMA_VERSION: i32 = 2;
const MAX_PAGE_SIZE: u32 = 512;
const REBUILD_PAGE_SIZE: i64 = 256;
const MAX_SEARCH_TEXT_BYTES: usize = 32 * 1024;

/// Cursor fenced to one projection generation. Rebuilds invalidate old cursors
/// instead of silently mixing rows from different projection snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionCursor {
    pub generation: u64,
    pub position: u64,
}

/// One bounded result page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<ProjectionCursor>,
}

/// A read-only semantic timeline row derived from canonical history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEntry {
    pub event_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub sequence: u64,
    pub kind: String,
}

/// Chat/run/branch summary projection for history navigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatSummary {
    pub chat_id: String,
    pub run_id: String,
    pub branch_id: String,
    pub title: String,
    pub head_sequence: u64,
    pub aggregate_version: u64,
}

/// Stable evidence locator back to one canonical semantic event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceLocator {
    pub evidence_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub event_id: String,
    pub sequence: u64,
    pub evidence_kind: String,
    pub summary: String,
}

/// Artifact metadata joined to its semantic origin event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactProjection {
    pub artifact_id: String,
    pub origin_event_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub media_type: String,
    pub logical_name: String,
    pub availability: String,
}

/// One full-text search result with a canonical event locator.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub event_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub sequence: u64,
    pub kind: String,
    pub snippet: String,
    pub rank: f64,
}

/// Explicit disposable-projection health facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionHealth {
    pub healthy: bool,
    pub generation: u64,
    pub reason: Option<String>,
}

/// A replaceable SQLite query index with no canonical mutation surface.
#[derive(Clone)]
pub struct ProjectionStore {
    gate: MaintenanceGate,
    connection: Arc<Mutex<Connection>>,
}

impl ProjectionStore {
    /// Opens a projection database. Corrupt or incompatible projection bytes are
    /// retained under a `.corrupt-*` name and replaced with an empty index.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = absolute(path.as_ref())?;
        let root = path
            .parent()
            .ok_or(StoreError::InvalidStorePath)?
            .to_path_buf();
        fs::create_dir_all(&root)?;
        let gate = MaintenanceGate::for_root(&root)?;
        let _lease = gate.shared()?;
        let connection = match open_projection(&path) {
            Ok(connection) => connection,
            Err(first_error) if path.exists() => {
                let quarantine = path.with_extension(format!("sqlite.corrupt-{}", now_nanos()?));
                fs::rename(&path, quarantine)?;
                for suffix in ["-wal", "-shm"] {
                    let companion = PathBuf::from(format!("{}{suffix}", path.display()));
                    if companion.exists() {
                        let _ = fs::remove_file(companion);
                    }
                }
                open_projection(&path).map_err(|_| first_error)?
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            gate,
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Rebuilds one chat/branch from a stable read transaction, then atomically
    /// publishes all Chat, timeline, evidence, artifact, and FTS rows together.
    pub fn rebuild_chat(
        &self,
        source: &LocalHistoryStore,
        chat_id: &str,
        branch_id: &str,
    ) -> Result<(), StoreError> {
        self.rebuild_chat_inner(source, chat_id, branch_id, true)
    }

    fn rebuild_chat_inner(
        &self,
        source: &LocalHistoryStore,
        chat_id: &str,
        branch_id: &str,
        record_health: bool,
    ) -> Result<(), StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        let _lease = self.gate.shared()?;
        let mut source_connection = Connection::open_with_flags(
            source.database_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source_connection.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;")?;
        let source_transaction = source_connection.transaction()?;
        let summary: (String, i64, i64) = source_transaction
            .query_row(
                "SELECT run_id, head_sequence, aggregate_version FROM chat_streams
                 WHERE chat_id = ?1 AND branch_id = ?2",
                params![chat_id, branch_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::UnknownHistoryStream)?;

        let mut target_connection = self.lock_connection()?;
        let target = target_connection.transaction()?;
        target.execute(
            "DELETE FROM timeline_projection WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
        )?;
        target.execute(
            "DELETE FROM evidence_projection WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
        )?;
        target.execute(
            "DELETE FROM artifact_projection WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
        )?;
        target.execute(
            "DELETE FROM search_projection WHERE chat_id = ?1 AND branch_id = ?2",
            params![chat_id, branch_id],
        )?;

        let mut sequence_cursor = 0_i64;
        let mut title = None;
        loop {
            let mut statement = source_transaction.prepare(
                "SELECT event_id, sequence, schema_version, kind, payload
                 FROM semantic_events
                 WHERE chat_id = ?1 AND branch_id = ?2 AND sequence > ?3
                 ORDER BY sequence LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![chat_id, branch_id, sequence_cursor, REBUILD_PAGE_SIZE],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            if rows.is_empty() {
                break;
            }
            for (event_id, sequence, schema_version, kind, payload_json) in &rows {
                let payload: Value = serde_json::from_str(payload_json)?;
                let searchable = searchable_text(&kind, &payload);
                let summary_text = truncate(&searchable, 512);
                if title.is_none() && !summary_text.is_empty() {
                    title = Some(summary_text.clone());
                }
                target.execute(
                    "INSERT INTO timeline_projection(
                       event_id, chat_id, branch_id, sequence, schema_version, kind, payload
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        event_id,
                        chat_id,
                        branch_id,
                        sequence,
                        schema_version,
                        kind,
                        payload_json
                    ],
                )?;
                target.execute(
                    "INSERT INTO evidence_projection(
                       evidence_id, chat_id, branch_id, event_id, sequence,
                       evidence_kind, summary
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        event_id,
                        chat_id,
                        branch_id,
                        event_id,
                        sequence,
                        kind,
                        summary_text
                    ],
                )?;
                target.execute(
                    "INSERT INTO search_projection(
                       event_id, chat_id, branch_id, sequence, kind, content
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![event_id, chat_id, branch_id, sequence, kind, searchable],
                )?;
            }
            sequence_cursor = rows.last().expect("non-empty page").1;
            if rows.len() < usize::try_from(REBUILD_PAGE_SIZE).expect("page size fits") {
                break;
            }
        }

        let mut artifact_statement = source_transaction.prepare(
            "SELECT r.artifact_id, r.origin_event_id, a.content_hash, a.byte_size,
                    a.media_type, a.logical_name, a.availability
             FROM artifact_references r
             JOIN artifacts a ON a.artifact_id = r.artifact_id
             JOIN semantic_events e ON e.event_id = r.origin_event_id
             WHERE e.chat_id = ?1 AND e.branch_id = ?2
             ORDER BY e.sequence, r.artifact_id",
        )?;
        let artifacts = artifact_statement
            .query_map(params![chat_id, branch_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(artifact_statement);
        for (artifact_id, event_id, hash, size, media_type, logical_name, availability) in artifacts
        {
            target.execute(
                "INSERT INTO artifact_projection(
                   artifact_id, origin_event_id, chat_id, branch_id, content_hash,
                   byte_size, media_type, logical_name, availability
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    artifact_id,
                    event_id,
                    chat_id,
                    branch_id,
                    hash,
                    size,
                    media_type,
                    logical_name,
                    availability,
                ],
            )?;
        }

        let generation = bump_generation(&target)?;
        target.execute(
            "INSERT INTO chat_projection(
               chat_id, run_id, branch_id, title, head_sequence, aggregate_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(chat_id, branch_id) DO UPDATE SET
               run_id = excluded.run_id, title = excluded.title,
               head_sequence = excluded.head_sequence,
               aggregate_version = excluded.aggregate_version",
            params![
                chat_id,
                summary.0,
                branch_id,
                title.unwrap_or_else(|| chat_id.to_owned()),
                summary.1,
                summary.2,
            ],
        )?;
        target.execute(
            "INSERT INTO projection_cursor(chat_id, branch_id, source_sequence, generation)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, branch_id) DO UPDATE SET
               source_sequence = excluded.source_sequence,
               generation = excluded.generation",
            params![chat_id, branch_id, summary.1, to_i64(generation)?],
        )?;
        if record_health {
            // A single-stream rebuild may advance a healthy projection, but it
            // must not conceal a previously interrupted full rebuild.
            target.execute(
                "INSERT INTO projection_health(id, healthy, reason, checked_generation)
                 VALUES (1, 1, NULL, ?1)
                 ON CONFLICT(id) DO UPDATE SET
                   checked_generation = excluded.checked_generation",
                [to_i64(generation)?],
            )?;
        }
        target.commit()?;
        source_transaction.commit()?;
        Ok(())
    }

    /// Rebuilds every durable stream after evicting stale projection rows.
    pub fn rebuild_all(&self, source: &LocalHistoryStore) -> Result<(), StoreError> {
        self.begin_full_rebuild()?;
        let connection = Connection::open_with_flags(
            source.database_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut statement = connection
            .prepare("SELECT chat_id, branch_id FROM chat_streams ORDER BY chat_id, branch_id")?;
        let streams = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for (chat_id, branch_id) in streams {
            inject_rebuild_interruption()?;
            self.rebuild_chat_inner(source, &chat_id, &branch_id, false)?;
        }
        self.finish_full_rebuild()
    }

    /// Evicts all replaceable rows while preserving the schema and bumping the
    /// cursor generation.
    pub fn evict(&self) -> Result<(), StoreError> {
        let _lease = self.gate.shared()?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM timeline_projection;
             DELETE FROM evidence_projection;
             DELETE FROM artifact_projection;
             DELETE FROM search_projection;
             DELETE FROM chat_projection;
             DELETE FROM projection_cursor;",
        )?;
        let generation = bump_generation(&transaction)?;
        transaction.execute(
            "INSERT INTO projection_health(id, healthy, reason, checked_generation)
             VALUES (1, 1, NULL, ?1)
             ON CONFLICT(id) DO UPDATE SET healthy = 1, reason = NULL,
               checked_generation = excluded.checked_generation",
            [to_i64(generation)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads all timeline rows for compatibility; page-based callers should use
    /// `timeline_page` for bounded memory.
    pub fn timeline(
        &self,
        chat_id: &str,
        branch_id: &str,
    ) -> Result<Vec<TimelineEntry>, StoreError> {
        let mut cursor = None;
        let mut entries = Vec::new();
        loop {
            let page = self.timeline_page(chat_id, branch_id, cursor, MAX_PAGE_SIZE)?;
            entries.extend(page.items);
            let Some(next) = page.next_cursor else { break };
            cursor = Some(next);
        }
        Ok(entries)
    }

    pub fn timeline_page(
        &self,
        chat_id: &str,
        branch_id: &str,
        cursor: Option<ProjectionCursor>,
        limit: u32,
    ) -> Result<ProjectionPage<TimelineEntry>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        self.page_query(cursor, limit, |connection, generation, position, fetch| {
            let mut statement = connection.prepare(
                "SELECT event_id, chat_id, branch_id, sequence, kind
                 FROM timeline_projection
                 WHERE chat_id = ?1 AND branch_id = ?2 AND sequence > ?3
                 ORDER BY sequence LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![chat_id, branch_id, to_i64(position)?, i64::from(fetch)],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let mut mapped = Vec::new();
            for (event_id, chat_id, branch_id, sequence, kind) in rows {
                mapped.push(TimelineEntry {
                    event_id,
                    chat_id,
                    branch_id,
                    sequence: from_i64(sequence)?,
                    kind,
                });
            }
            finish_page(mapped, limit, generation, |entry| entry.sequence)
        })
    }

    pub fn chats_page(
        &self,
        cursor: Option<ProjectionCursor>,
        limit: u32,
    ) -> Result<ProjectionPage<ChatSummary>, StoreError> {
        self.page_query(cursor, limit, |connection, generation, position, fetch| {
            let mut statement = connection.prepare(
                "SELECT id, chat_id, run_id, branch_id, title, head_sequence,
                        aggregate_version
                 FROM chat_projection WHERE id > ?1 ORDER BY id LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![to_i64(position)?, i64::from(fetch)], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        ChatSummary {
                            chat_id: row.get(1)?,
                            run_id: row.get(2)?,
                            branch_id: row.get(3)?,
                            title: row.get(4)?,
                            head_sequence: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(u64::MAX),
                            aggregate_version: u64::try_from(row.get::<_, i64>(6)?)
                                .unwrap_or(u64::MAX),
                        },
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            finish_keyed_page(rows, limit, generation)
        })
    }

    pub fn evidence_page(
        &self,
        chat_id: &str,
        branch_id: &str,
        cursor: Option<ProjectionCursor>,
        limit: u32,
    ) -> Result<ProjectionPage<EvidenceLocator>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        self.page_query(cursor, limit, |connection, generation, position, fetch| {
            let mut statement = connection.prepare(
                "SELECT id, evidence_id, chat_id, branch_id, event_id, sequence,
                        evidence_kind, summary
                 FROM evidence_projection
                 WHERE chat_id = ?1 AND branch_id = ?2 AND id > ?3
                 ORDER BY id LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![chat_id, branch_id, to_i64(position)?, i64::from(fetch)],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            EvidenceLocator {
                                evidence_id: row.get(1)?,
                                chat_id: row.get(2)?,
                                branch_id: row.get(3)?,
                                event_id: row.get(4)?,
                                sequence: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(u64::MAX),
                                evidence_kind: row.get(6)?,
                                summary: row.get(7)?,
                            },
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            finish_keyed_page(rows, limit, generation)
        })
    }

    pub fn artifacts_page(
        &self,
        chat_id: &str,
        branch_id: &str,
        cursor: Option<ProjectionCursor>,
        limit: u32,
    ) -> Result<ProjectionPage<ArtifactProjection>, StoreError> {
        validate_id(chat_id)?;
        validate_id(branch_id)?;
        self.page_query(cursor, limit, |connection, generation, position, fetch| {
            let mut statement = connection.prepare(
                "SELECT id, artifact_id, origin_event_id, chat_id, branch_id,
                        content_hash, byte_size, media_type, logical_name, availability
                 FROM artifact_projection
                 WHERE chat_id = ?1 AND branch_id = ?2 AND id > ?3
                 ORDER BY id LIMIT ?4",
            )?;
            let rows = statement
                .query_map(
                    params![chat_id, branch_id, to_i64(position)?, i64::from(fetch)],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            ArtifactProjection {
                                artifact_id: row.get(1)?,
                                origin_event_id: row.get(2)?,
                                chat_id: row.get(3)?,
                                branch_id: row.get(4)?,
                                content_hash: row.get(5)?,
                                byte_size: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(u64::MAX),
                                media_type: row.get(7)?,
                                logical_name: row.get(8)?,
                                availability: row.get(9)?,
                            },
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            finish_keyed_page(rows, limit, generation)
        })
    }

    /// Performs bounded literal full-text search. Cursor position is an offset
    /// within the stable projection generation and rank ordering.
    pub fn search(
        &self,
        query: &str,
        cursor: Option<ProjectionCursor>,
        limit: u32,
    ) -> Result<ProjectionPage<SearchHit>, StoreError> {
        let query = literal_fts_query(query)?;
        self.page_query(cursor, limit, |connection, generation, position, fetch| {
            let mut statement = connection.prepare(
                "SELECT event_id, chat_id, branch_id, sequence, kind,
                        snippet(search_projection, 5, '[', ']', '…', 16),
                        bm25(search_projection)
                 FROM search_projection WHERE search_projection MATCH ?1
                 ORDER BY bm25(search_projection), sequence, event_id
                 LIMIT ?2 OFFSET ?3",
            )?;
            let rows = statement
                .query_map(params![query, i64::from(fetch), to_i64(position)?], |row| {
                    Ok(SearchHit {
                        event_id: row.get(0)?,
                        chat_id: row.get(1)?,
                        branch_id: row.get(2)?,
                        sequence: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(u64::MAX),
                        kind: row.get(4)?,
                        snippet: row.get(5)?,
                        rank: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = rows.len() > usize::try_from(limit).expect("u32 fits");
            let mut items = rows;
            items.truncate(usize::try_from(limit).expect("u32 fits"));
            let next_cursor = has_more.then(|| ProjectionCursor {
                generation,
                position: position + u64::from(limit),
            });
            Ok(ProjectionPage { items, next_cursor })
        })
    }

    /// Checks both SQLite integrity and the last rebuild health marker.
    pub fn health(&self) -> Result<ProjectionHealth, StoreError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let generation = generation(&connection)?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let stored: Option<(i64, Option<String>)> = connection
            .query_row(
                "SELECT healthy, reason FROM projection_health WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let healthy = integrity == "ok" && stored.as_ref().is_none_or(|value| value.0 == 1);
        let reason = if integrity != "ok" {
            Some(integrity)
        } else {
            stored.and_then(|value| value.1)
        };
        Ok(ProjectionHealth {
            healthy,
            generation,
            reason,
        })
    }

    fn page_query<T>(
        &self,
        cursor: Option<ProjectionCursor>,
        limit: u32,
        query: impl FnOnce(&Connection, u64, u64, u32) -> Result<ProjectionPage<T>, StoreError>,
    ) -> Result<ProjectionPage<T>, StoreError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock_connection()?;
        let current_generation = generation(&connection)?;
        let position = match cursor {
            Some(cursor) if cursor.generation != current_generation => {
                return Err(StoreError::StaleProjectionCursor);
            }
            Some(cursor) => cursor.position,
            None => 0,
        };
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        query(
            &connection,
            current_generation,
            position,
            limit.saturating_add(1),
        )
    }

    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::PoisonedConnection)
    }

    fn begin_full_rebuild(&self) -> Result<(), StoreError> {
        let _lease = self.gate.shared()?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM timeline_projection;
             DELETE FROM evidence_projection;
             DELETE FROM artifact_projection;
             DELETE FROM search_projection;
             DELETE FROM chat_projection;
             DELETE FROM projection_cursor;",
        )?;
        let generation = bump_generation(&transaction)?;
        transaction.execute(
            "INSERT INTO projection_health(id, healthy, reason, checked_generation)
             VALUES (1, 0, 'full rebuild interrupted or in progress', ?1)
             ON CONFLICT(id) DO UPDATE SET healthy = 0,
               reason = excluded.reason,
               checked_generation = excluded.checked_generation",
            [to_i64(generation)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn finish_full_rebuild(&self) -> Result<(), StoreError> {
        let _lease = self.gate.shared()?;
        let mut connection = self.lock_connection()?;
        let transaction = connection.transaction()?;
        let generation = generation(&transaction)?;
        transaction.execute(
            "INSERT INTO projection_health(id, healthy, reason, checked_generation)
             VALUES (1, 1, NULL, ?1)
             ON CONFLICT(id) DO UPDATE SET healthy = 1, reason = NULL,
               checked_generation = excluded.checked_generation",
            [to_i64(generation)?],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static REBUILD_INTERRUPT_AFTER_STREAMS: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn inject_rebuild_interruption() -> Result<(), StoreError> {
    REBUILD_INTERRUPT_AFTER_STREAMS.with(|remaining| match remaining.get() {
        Some(0) => {
            remaining.set(None);
            Err(StoreError::CorruptProjection(
                "injected full rebuild interruption".into(),
            ))
        }
        Some(value) => {
            remaining.set(Some(value - 1));
            Ok(())
        }
        None => Ok(()),
    })
}

#[cfg(not(test))]
fn inject_rebuild_interruption() -> Result<(), StoreError> {
    Ok(())
}

fn open_projection(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open(path)?;
    let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > PROJECTION_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedStorageVersion {
            found: u32::try_from(found).unwrap_or(u32::MAX),
            supported: u32::try_from(PROJECTION_SCHEMA_VERSION).expect("positive version"),
        });
    }
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;
         CREATE TABLE IF NOT EXISTS projection_state (
           key TEXT PRIMARY KEY, value INTEGER NOT NULL
         ) STRICT;
         INSERT OR IGNORE INTO projection_state(key, value) VALUES ('generation', 0);
         CREATE TABLE IF NOT EXISTS chat_projection (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           chat_id TEXT NOT NULL, run_id TEXT NOT NULL, branch_id TEXT NOT NULL,
           title TEXT NOT NULL, head_sequence INTEGER NOT NULL,
           aggregate_version INTEGER NOT NULL,
           UNIQUE(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS timeline_projection (
           event_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
           sequence INTEGER NOT NULL, schema_version INTEGER NOT NULL,
           kind TEXT NOT NULL, payload TEXT NOT NULL,
           UNIQUE(chat_id, branch_id, sequence)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS evidence_projection (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           evidence_id TEXT NOT NULL UNIQUE, chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
           event_id TEXT NOT NULL, sequence INTEGER NOT NULL,
           evidence_kind TEXT NOT NULL, summary TEXT NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS artifact_projection (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           artifact_id TEXT NOT NULL, origin_event_id TEXT NOT NULL,
           chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
           content_hash TEXT NOT NULL, byte_size INTEGER NOT NULL,
           media_type TEXT NOT NULL, logical_name TEXT NOT NULL,
           availability TEXT NOT NULL,
           UNIQUE(artifact_id, origin_event_id)
         ) STRICT;
         CREATE VIRTUAL TABLE IF NOT EXISTS search_projection USING fts5(
           event_id UNINDEXED, chat_id UNINDEXED, branch_id UNINDEXED,
           sequence UNINDEXED, kind, content, tokenize = 'unicode61'
         );
         CREATE TABLE IF NOT EXISTS projection_cursor (
           chat_id TEXT NOT NULL, branch_id TEXT NOT NULL,
           source_sequence INTEGER NOT NULL, generation INTEGER NOT NULL,
           PRIMARY KEY(chat_id, branch_id)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS projection_health (
           id INTEGER PRIMARY KEY CHECK(id = 1), healthy INTEGER NOT NULL,
           reason TEXT, checked_generation INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX IF NOT EXISTS timeline_projection_stream
           ON timeline_projection(chat_id, branch_id, sequence);
         CREATE INDEX IF NOT EXISTS evidence_projection_stream
           ON evidence_projection(chat_id, branch_id, id);
         CREATE INDEX IF NOT EXISTS artifact_projection_stream
           ON artifact_projection(chat_id, branch_id, id);
         PRAGMA user_version = 2;",
    )?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StoreError::CorruptProjection(integrity));
    }
    Ok(connection)
}

fn generation(connection: &Connection) -> Result<u64, StoreError> {
    let value: i64 = connection.query_row(
        "SELECT value FROM projection_state WHERE key = 'generation'",
        [],
        |row| row.get(0),
    )?;
    from_i64(value)
}

fn bump_generation(connection: &Connection) -> Result<u64, StoreError> {
    connection.execute(
        "UPDATE projection_state SET value = value + 1 WHERE key = 'generation'",
        [],
    )?;
    generation(connection)
}

fn finish_page<T>(
    mut items: Vec<T>,
    limit: u32,
    generation: u64,
    position: impl Fn(&T) -> u64,
) -> Result<ProjectionPage<T>, StoreError> {
    let limit = usize::try_from(limit).expect("u32 fits usize");
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(|item| ProjectionCursor {
            generation,
            position: position(item),
        })
    } else {
        None
    };
    Ok(ProjectionPage { items, next_cursor })
}

fn finish_keyed_page<T>(
    mut rows: Vec<(i64, T)>,
    limit: u32,
    generation: u64,
) -> Result<ProjectionPage<T>, StoreError> {
    let limit = usize::try_from(limit).expect("u32 fits usize");
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last()
            .map(|(id, _)| {
                Ok::<ProjectionCursor, StoreError>(ProjectionCursor {
                    generation,
                    position: from_i64(*id)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(ProjectionPage {
        items: rows.into_iter().map(|(_, item)| item).collect(),
        next_cursor,
    })
}

fn searchable_text(kind: &str, payload: &Value) -> String {
    fn visit(value: &Value, output: &mut String) {
        if output.len() >= MAX_SEARCH_TEXT_BYTES {
            return;
        }
        match value {
            Value::String(text) => {
                output.push(' ');
                output.push_str(&truncate(
                    text,
                    MAX_SEARCH_TEXT_BYTES.saturating_sub(output.len()),
                ));
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, output);
                }
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    output.push(' ');
                    output.push_str(key);
                    visit(value, output);
                }
            }
            Value::Number(number) => {
                output.push(' ');
                output.push_str(&number.to_string());
            }
            Value::Bool(value) => {
                output.push(' ');
                output.push_str(if *value { "true" } else { "false" });
            }
            Value::Null => {}
        }
    }
    let mut output = kind.to_owned();
    visit(payload, &mut output);
    truncate(&output, MAX_SEARCH_TEXT_BYTES)
}

fn truncate(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn literal_fts_query(query: &str) -> Result<String, StoreError> {
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.len() > 512 || trimmed.contains('\0') {
        return Err(StoreError::InvalidSearchQuery);
    }
    Ok(format!("\"{}\"", trimmed.replace('"', "\"\"")))
}

fn validate_id(value: &str) -> Result<(), StoreError> {
    aworkit_protocol::StableId::parse(value)
        .map(|_| ())
        .map_err(|_| StoreError::InvalidId)
}

fn absolute(path: &Path) -> Result<PathBuf, StoreError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn now_nanos() -> Result<u128, StoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidStoredData)?
        .as_nanos())
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
}

fn from_i64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredData)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;
    use crate::{CommitBatch, Event};

    fn root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aworkit-projections-{nonce}"))
    }

    #[test]
    fn rebuilds_all_query_families_idempotently_with_pagination() {
        let root = root();
        fs::create_dir_all(&root).expect("root");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        history
            .commit(&CommitBatch {
                chat_id: "chat_01".into(),
                branch_id: "main".into(),
                expected_head: 0,
                events: vec![
                    Event {
                        event_id: "event_01".into(),
                        kind: "input.accepted".into(),
                        payload: json!({"schemaVersion": 1, "text": "find the aurora"}),
                    },
                    Event {
                        event_id: "event_02".into(),
                        kind: "evidence.recorded".into(),
                        payload: json!({"schemaVersion": 1, "citation": "northern lights"}),
                    },
                ],
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
        projection
            .rebuild_chat(&history, "chat_01", "main")
            .expect("idempotent");

        let first = projection
            .timeline_page("chat_01", "main", None, 1)
            .expect("first");
        assert_eq!(first.items.len(), 1);
        let second = projection
            .timeline_page("chat_01", "main", first.next_cursor, 1)
            .expect("second");
        assert_eq!(second.items[0].event_id, "event_02");
        assert_eq!(
            projection.chats_page(None, 8).expect("chats").items.len(),
            1
        );
        assert_eq!(
            projection
                .evidence_page("chat_01", "main", None, 8)
                .expect("evidence")
                .items
                .len(),
            2
        );
        let hits = projection
            .search("northern lights", None, 8)
            .expect("search");
        assert_eq!(hits.items[0].event_id, "event_02");
        assert!(projection.health().expect("health").healthy);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rebuild_invalidates_old_cursors_and_evict_is_recoverable() {
        let root = root();
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
                    payload: json!({"schemaVersion": 1, "text": "hello"}),
                }],
                attempt: None,
                checkpoint: None,
                deduplication: None,
                outbox: vec![],
            })
            .expect("commit");
        let projection = ProjectionStore::open(root.join("projection.sqlite")).expect("projection");
        projection.rebuild_all(&history).expect("rebuild");
        let generation = projection.health().expect("health").generation;
        projection.rebuild_all(&history).expect("second rebuild");
        assert!(matches!(
            projection.timeline_page(
                "chat_01",
                "main",
                Some(ProjectionCursor {
                    generation,
                    position: 0
                }),
                8
            ),
            Err(StoreError::StaleProjectionCursor)
        ));
        projection.evict().expect("evict");
        assert!(
            projection
                .timeline("chat_01", "main")
                .expect("empty")
                .is_empty()
        );
        projection.rebuild_all(&history).expect("recover");
        assert_eq!(
            projection
                .timeline("chat_01", "main")
                .expect("timeline")
                .len(),
            1
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn interrupted_full_rebuild_stays_unhealthy_until_complete_retry() {
        let root = root();
        fs::create_dir_all(&root).expect("root");
        let history = LocalHistoryStore::open(root.join("history.sqlite")).expect("history");
        for (chat_id, event_id) in [("chat_01", "event_01"), ("chat_02", "event_02")] {
            history
                .commit(&CommitBatch {
                    chat_id: chat_id.into(),
                    branch_id: "main".into(),
                    expected_head: 0,
                    events: vec![Event {
                        event_id: event_id.into(),
                        kind: "input.accepted".into(),
                        payload: json!({"schemaVersion": 1, "text": chat_id}),
                    }],
                    attempt: None,
                    checkpoint: None,
                    deduplication: None,
                    outbox: vec![],
                })
                .expect("commit");
        }
        let projection = ProjectionStore::open(root.join("projection.sqlite")).expect("projection");
        projection.rebuild_all(&history).expect("initial rebuild");

        REBUILD_INTERRUPT_AFTER_STREAMS.with(|remaining| remaining.set(Some(1)));
        assert!(matches!(
            projection.rebuild_all(&history),
            Err(StoreError::CorruptProjection(_))
        ));
        let interrupted = projection.health().expect("interrupted health");
        assert!(!interrupted.healthy);
        assert!(interrupted.reason.is_some());
        projection
            .rebuild_chat(&history, "chat_01", "main")
            .expect("single stream repair");
        assert!(!projection.health().expect("still incomplete").healthy);

        projection.rebuild_all(&history).expect("complete retry");
        assert!(projection.health().expect("recovered").healthy);
        assert_eq!(
            projection.chats_page(None, 8).expect("chats").items.len(),
            2
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn corrupt_projection_is_quarantined_and_recreated() {
        let root = root();
        fs::create_dir_all(&root).expect("root");
        let path = root.join("projection.sqlite");
        fs::write(&path, b"not sqlite").expect("corrupt bytes");
        let projection = ProjectionStore::open(&path).expect("replaceable open");
        assert!(projection.health().expect("health").healthy);
        assert!(
            fs::read_dir(&root)
                .expect("entries")
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
