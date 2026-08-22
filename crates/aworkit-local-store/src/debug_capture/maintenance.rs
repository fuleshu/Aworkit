//! Integrity recovery and independent TTL/quota retention.

use std::{collections::BTreeSet, fs};

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    common::{aggregate_hash, parse_chunk_name, remove_directory_if_present, to_i64, validate_id},
    model::{CaptureError, CaptureManifest, CaptureState, RetentionReport},
    store::DebugCaptureStore,
};

impl DebugCaptureStore {
    /// Verifies every durable chunk and aggregate hash. A failure atomically
    /// marks the capture corrupt before its bytes are discarded.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture is unavailable or repository access
    /// fails; integrity mismatches are persisted as a corrupt tombstone.
    pub fn verify(&self, capture_id: &str) -> Result<CaptureManifest, CaptureError> {
        self.require_writable()?;
        validate_id(capture_id)?;
        let manifest = self.manifest(capture_id)?;
        if !matches!(
            manifest.state,
            CaptureState::Available | CaptureState::Expired
        ) {
            return Err(CaptureError::Unavailable(manifest.state));
        }
        let rows = self.chunk_rows(capture_id, 0, u32::MAX)?;
        let verification = rows
            .iter()
            .try_for_each(|row| self.read_verified_chunk(capture_id, row).map(|_| ()));
        let expected = aggregate_hash(rows.iter().map(|row| row.content_hash.as_str()));
        if verification.is_err() || manifest.content_hash.as_deref() != Some(expected.as_str()) {
            self.mark_corrupt(capture_id)?;
            return self.manifest(capture_id);
        }
        Ok(manifest)
    }

    /// Seals interrupted recordings as explicitly truncated and removes chunk
    /// files that were published before an interrupted metadata transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if recovery cannot inspect, verify, or durably update
    /// the capture repository.
    pub fn recover_interrupted(&self, recovered_at_epoch_ms: u64) -> Result<u64, CaptureError> {
        self.require_writable()?;
        let _lease = self.gate.exclusive()?;
        let captures = {
            let connection = self.lock()?;
            let mut statement = connection
                .prepare("SELECT capture_id, state FROM capture_manifests ORDER BY capture_id")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut repaired = 0_u64;
        for (capture_id, state) in captures {
            validate_id(&capture_id)?;
            let state = CaptureState::parse(&state)?;
            let rows = self.chunk_rows(&capture_id, 0, u32::MAX)?;
            let known = rows.iter().map(|row| row.ordinal).collect::<BTreeSet<_>>();
            let directory = self.capture_directory(&capture_id);
            if directory.exists() {
                for entry in fs::read_dir(&directory)? {
                    let entry = entry?;
                    let Some(ordinal) = parse_chunk_name(&entry.file_name()) else {
                        continue;
                    };
                    if !known.contains(&ordinal) {
                        fs::remove_file(entry.path())?;
                        repaired = repaired.saturating_add(1);
                    }
                }
            }
            if state == CaptureState::Recording {
                let corrupt = rows
                    .iter()
                    .any(|row| self.read_verified_chunk(&capture_id, row).is_err());
                if corrupt {
                    self.mark_corrupt_with_maintenance_held(&capture_id)?;
                } else {
                    self.seal_truncated_with_maintenance_held(
                        &capture_id,
                        recovered_at_epoch_ms,
                        "crash_recovery",
                        false,
                    )?;
                }
                repaired = repaired.saturating_add(1);
            }
        }
        Ok(repaired)
    }

    /// Applies TTL and least-recently-sealed global quota selection, then
    /// removes only tombstones whose grace elapsed and whose readers released.
    ///
    /// # Errors
    ///
    /// Returns an error if retention cannot acquire maintenance access or
    /// durably update metadata and bytes.
    #[allow(clippy::too_many_lines)]
    pub fn enforce_retention(&self, now_epoch_ms: u64) -> Result<RetentionReport, CaptureError> {
        self.require_writable()?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM capture_reader_leases WHERE expires_at_epoch_ms <= ?1",
            [to_i64(now_epoch_ms)?],
        )?;
        let mut expired = transaction.execute(
            "UPDATE capture_manifests
             SET state='expired',
                 sealed_at_epoch_ms=COALESCE(sealed_at_epoch_ms, ?1),
                 expired_at_epoch_ms=?1,
                 purge_after_epoch_ms=?1 + expired_tombstone_ms,
                 truncated=CASE WHEN state='recording' THEN 1 ELSE truncated END,
                 truncation_reason=CASE WHEN state='recording'
                     THEN COALESCE(truncation_reason, 'stale_recording_ttl')
                     ELSE truncation_reason END
             WHERE state IN ('recording', 'available') AND expires_at_epoch_ms <= ?1",
            [to_i64(now_epoch_ms)?],
        )?;

        let quota: Option<i64> = transaction
            .query_row(
                "SELECT MIN(global_quota_bytes) FROM capture_manifests
                 WHERE state != 'purged' AND stored_byte_count > 0",
                [],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(quota) = quota {
            let mut total: i64 = transaction.query_row(
                "SELECT COALESCE(SUM(stored_byte_count), 0) FROM capture_manifests",
                [],
                |row| row.get(0),
            )?;
            if total > quota {
                let candidates = {
                    let mut statement = transaction.prepare(
                        "SELECT capture_id, stored_byte_count, expired_tombstone_ms
                         FROM capture_manifests WHERE state IN ('recording', 'available')
                         ORDER BY COALESCE(sealed_at_epoch_ms, created_at_epoch_ms), capture_id",
                    )?;
                    statement
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for (capture_id, bytes, tombstone_ms) in candidates {
                    if total <= quota {
                        break;
                    }
                    let purge_after = to_i64(now_epoch_ms)?
                        .checked_add(tombstone_ms)
                        .ok_or(CaptureError::NumericOverflow)?;
                    transaction.execute(
                        "UPDATE capture_manifests
                         SET state='expired', expired_at_epoch_ms=?2,
                             sealed_at_epoch_ms=COALESCE(sealed_at_epoch_ms, ?2),
                             purge_after_epoch_ms=?3,
                             truncated=CASE WHEN state='recording' THEN 1 ELSE truncated END,
                             truncation_reason=COALESCE(truncation_reason, 'global_retention_quota')
                         WHERE capture_id=?1 AND state IN ('recording', 'available')",
                        params![capture_id, to_i64(now_epoch_ms)?, purge_after],
                    )?;
                    total = total.saturating_sub(bytes);
                    expired = expired.saturating_add(1);
                }
            }
        }

        let purge_ids = {
            let mut statement = transaction.prepare(
                "SELECT m.capture_id FROM capture_manifests m
                 WHERE m.state IN ('expired', 'corrupt_discarded')
                   AND m.purge_after_epoch_ms <= ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM capture_reader_leases l
                       WHERE l.capture_id=m.capture_id AND l.expires_at_epoch_ms > ?1
                   ) ORDER BY m.capture_id",
            )?;
            statement
                .query_map([to_i64(now_epoch_ms)?], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for capture_id in &purge_ids {
            validate_id(capture_id)?;
        }
        transaction.commit()?;
        drop(connection);
        for capture_id in &purge_ids {
            remove_directory_if_present(&self.capture_directory(capture_id))?;
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for capture_id in &purge_ids {
            transaction.execute(
                "DELETE FROM capture_chunks WHERE capture_id=?1",
                [capture_id],
            )?;
            transaction.execute(
                "UPDATE capture_manifests
                 SET state='purged', stored_byte_count=0 WHERE capture_id=?1
                   AND state IN ('expired', 'corrupt_discarded')",
                [capture_id],
            )?;
        }
        transaction.commit()?;
        Ok(RetentionReport {
            expired: u64::try_from(expired).unwrap_or(u64::MAX),
            purged: u64::try_from(purge_ids.len()).unwrap_or(u64::MAX),
        })
    }

    pub(super) fn mark_corrupt(&self, capture_id: &str) -> Result<(), CaptureError> {
        self.require_writable()?;
        let _lease = self.gate.shared()?;
        self.mark_corrupt_with_maintenance_held(capture_id)
    }

    pub(super) fn mark_corrupt_with_maintenance_held(
        &self,
        capture_id: &str,
    ) -> Result<(), CaptureError> {
        validate_id(capture_id)?;
        let connection = self.lock()?;
        connection.execute(
            "UPDATE capture_manifests
             SET state='corrupt_discarded',
                 purge_after_epoch_ms=0,
                 truncation_reason=COALESCE(truncation_reason, 'integrity_failure')
             WHERE capture_id=?1 AND state IN ('recording', 'available', 'expired')",
            [capture_id],
        )?;
        drop(connection);
        remove_directory_if_present(&self.capture_directory(capture_id))
    }
}
