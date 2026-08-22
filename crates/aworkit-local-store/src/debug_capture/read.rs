//! Manifest queries, leased pagination, and verified decompression.

use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::bounded_codec::decompress;

use super::{
    common::{
        ChunkRow, MAX_PAGE_SIZE, MAX_READER_LEASE_MS, load_manifest, read_manifest_row, to_i64,
        validate_id,
    },
    model::{
        CaptureChunk, CaptureError, CaptureManifest, CapturePage, CaptureState, CaptureStoreMode,
    },
    store::DebugCaptureStore,
};

static LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl DebugCaptureStore {
    /// Loads metadata in every lifecycle state, including terminal tombstones.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid/unknown identity or repository failure.
    pub fn manifest(&self, capture_id: &str) -> Result<CaptureManifest, CaptureError> {
        validate_id(capture_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        load_manifest(&connection, capture_id)?.ok_or(CaptureError::UnknownCapture)
    }

    /// Lists manifests without touching payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid cursor/page or repository failure.
    pub fn list_manifests(
        &self,
        after_capture_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CaptureManifest>, CaptureError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(CaptureError::InvalidPage);
        }
        if let Some(cursor) = after_capture_id {
            validate_id(cursor)?;
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT capture_id, source, chat_id, event_id, invocation_id, attempt_id,
                    policy_generation, redaction_set_id, quota_class, created_at_epoch_ms, sealed_at_epoch_ms,
                    expires_at_epoch_ms, expired_at_epoch_ms, purge_after_epoch_ms, state,
                    chunk_count, byte_count, stored_byte_count, content_hash, truncated,
                    truncation_reason, redaction_count, redaction_omissions
             FROM capture_manifests
             WHERE capture_id > COALESCE(?1, '') ORDER BY capture_id LIMIT ?2",
        )?;
        statement
            .query_map(
                params![after_capture_id, i64::from(limit)],
                read_manifest_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(CaptureError::from)
    }

    /// Creates an expiring cross-process reader lease. Retention can mark the
    /// capture expired, but cannot purge its bytes until this lease is released.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture is unavailable, the lease is invalid,
    /// or the lease cannot be committed.
    pub fn acquire_reader(
        &self,
        capture_id: &str,
        now_epoch_ms: u64,
        lease_ms: u64,
    ) -> Result<CaptureReader, CaptureError> {
        self.require_writable()?;
        validate_id(capture_id)?;
        if lease_ms == 0 || lease_ms > MAX_READER_LEASE_MS {
            return Err(CaptureError::InvalidLease);
        }
        let expires = now_epoch_ms
            .checked_add(lease_ms)
            .ok_or(CaptureError::InvalidLease)?;
        let lease_id = format!(
            "reader_{}_{}",
            std::process::id(),
            LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let _maintenance = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = super::common::load_state(&transaction, capture_id)?;
        if state != CaptureState::Available {
            return Err(CaptureError::Unavailable(state));
        }
        transaction.execute(
            "DELETE FROM capture_reader_leases WHERE expires_at_epoch_ms <= ?1",
            [to_i64(now_epoch_ms)?],
        )?;
        transaction.execute(
            "INSERT INTO capture_reader_leases(
                lease_id, capture_id, expires_at_epoch_ms
             ) VALUES (?1, ?2, ?3)",
            params![lease_id, capture_id, to_i64(expires)?],
        )?;
        transaction.commit()?;
        Ok(CaptureReader {
            store: self.clone(),
            capture_id: capture_id.to_owned(),
            lease_id,
        })
    }

    pub(super) fn read_page(
        &self,
        capture_id: &str,
        lease_id: &str,
        start_ordinal: u64,
        limit: u32,
    ) -> Result<CapturePage, CaptureError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(CaptureError::InvalidPage);
        }
        let _lease = self.gate.shared()?;
        let manifest = self.manifest(capture_id)?;
        if !matches!(
            manifest.state,
            CaptureState::Available | CaptureState::Expired
        ) {
            return Err(CaptureError::Unavailable(manifest.state));
        }
        let active = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT 1 FROM capture_reader_leases
                     WHERE lease_id=?1 AND capture_id=?2",
                    params![lease_id, capture_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
        };
        if !active {
            return Err(CaptureError::UnknownReaderLease);
        }
        let rows = self.chunk_rows(capture_id, start_ordinal, limit)?;
        let chunks = match rows
            .iter()
            .map(|row| self.read_verified_chunk(capture_id, row))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(chunks) => chunks,
            Err(error) => {
                if self.mode == CaptureStoreMode::ReadWrite {
                    self.mark_corrupt(capture_id)?;
                }
                return Err(error);
            }
        };
        let next_ordinal = chunks
            .last()
            .map(|chunk| chunk.ordinal.saturating_add(1))
            .filter(|next| *next < manifest.chunk_count);
        Ok(CapturePage {
            manifest,
            chunks,
            next_ordinal,
        })
    }

    pub(super) fn chunk_rows(
        &self,
        capture_id: &str,
        start_ordinal: u64,
        limit: u32,
    ) -> Result<Vec<ChunkRow>, CaptureError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT ordinal, received_at_epoch_ms, byte_count, stored_byte_count, content_hash
             FROM capture_chunks WHERE capture_id=?1 AND ordinal >= ?2
             ORDER BY ordinal LIMIT ?3",
        )?;
        Ok(statement
            .query_map(
                params![capture_id, to_i64(start_ordinal)?, i64::from(limit),],
                |row| {
                    Ok(ChunkRow {
                        ordinal: checked_u64(row.get(0)?, 0)?,
                        received_at_epoch_ms: checked_u64(row.get(1)?, 1)?,
                        byte_count: checked_u64(row.get(2)?, 2)?,
                        stored_byte_count: checked_u64(row.get(3)?, 3)?,
                        content_hash: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub(super) fn read_verified_chunk(
        &self,
        capture_id: &str,
        row: &ChunkRow,
    ) -> Result<CaptureChunk, CaptureError> {
        let encoded = fs::read(self.chunk_path(capture_id, row.ordinal))?;
        if u64::try_from(encoded.len()).map_err(|_| CaptureError::NumericOverflow)?
            != row.stored_byte_count
        {
            return Err(CaptureError::CorruptChunk);
        }
        let max = usize::try_from(row.byte_count).map_err(|_| CaptureError::NumericOverflow)?;
        let payload = decompress(&encoded, max)?;
        if payload.len() != max || super::common::hash_bytes(&payload) != row.content_hash {
            return Err(CaptureError::CorruptChunk);
        }
        Ok(CaptureChunk {
            ordinal: row.ordinal,
            received_at_epoch_ms: row.received_at_epoch_ms,
            payload,
            content_hash: row.content_hash.clone(),
        })
    }

    fn release_reader(&self, lease_id: &str) -> Result<(), CaptureError> {
        if self.mode != CaptureStoreMode::ReadWrite {
            return Ok(());
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        connection.execute(
            "DELETE FROM capture_reader_leases WHERE lease_id=?1",
            [lease_id],
        )?;
        Ok(())
    }
}

/// A reader fence that protects expired bytes from garbage collection.
pub struct CaptureReader {
    store: DebugCaptureStore,
    capture_id: String,
    lease_id: String,
}

impl CaptureReader {
    /// Reads a verified page of decompressed chunks while this lease is live.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease/page is invalid or a chunk is missing,
    /// corrupt, or expands beyond its recorded bound.
    pub fn read_page(&self, start_ordinal: u64, limit: u32) -> Result<CapturePage, CaptureError> {
        self.store
            .read_page(&self.capture_id, &self.lease_id, start_ordinal, limit)
    }
}

impl Drop for CaptureReader {
    fn drop(&mut self) {
        let _ = self.store.release_reader(&self.lease_id);
    }
}

fn checked_u64(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}
