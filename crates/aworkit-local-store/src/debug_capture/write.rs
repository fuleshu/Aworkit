//! Begin, redacted append, quota degradation, and atomic sealing.

use std::fs;

use rusqlite::{TransactionBehavior, params};

use crate::{bounded_codec::compress, filesystem::write_and_sync_atomic, redaction::RedactionSet};

use super::{
    common::{
        HARD_MAX_CHUNK_BYTES, canonical_hash, from_i64, hash_bytes, load_manifest,
        load_manifest_with_hash, load_recording_limits, load_state, redaction_reason,
        seal_transaction, to_i64, validate_id, validate_policy, validate_request,
    },
    model::{
        CaptureAppendOutcome, CaptureChunkMetadata, CaptureError, CaptureFrame, CaptureManifest,
        CapturePolicy, CaptureRequest, CaptureState,
    },
    store::DebugCaptureStore,
};

impl DebugCaptureStore {
    /// Starts an enabled capture or returns `None` for the default-disabled
    /// policy. Repeating an identical request is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity/policy data, generation mismatch,
    /// conflicting reuse, or a repository failure.
    pub fn begin(
        &self,
        request: &CaptureRequest,
        policy: &CapturePolicy,
        redaction: &RedactionSet,
    ) -> Result<Option<CaptureManifest>, CaptureError> {
        self.require_writable()?;
        validate_request(request)?;
        validate_policy(policy)?;
        if !policy.enabled {
            return Ok(None);
        }
        if policy.generation != redaction.generation() {
            return Err(CaptureError::RedactionGenerationMismatch);
        }
        let request_hash = canonical_hash(&(request, policy, redaction.identity()))?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((existing_hash, manifest)) =
            load_manifest_with_hash(&transaction, &request.capture_id)?
        {
            if existing_hash == request_hash {
                transaction.commit()?;
                return Ok(Some(manifest));
            }
            return Err(CaptureError::IdentityConflict);
        }
        let expires = request
            .created_at_epoch_ms
            .checked_add(policy.ttl_ms)
            .ok_or(CaptureError::InvalidPolicy)?;
        transaction.execute(
            "INSERT INTO capture_manifests(
                capture_id, request_hash, source, chat_id, event_id, invocation_id, attempt_id,
                policy_generation, redaction_set_id, quota_class, created_at_epoch_ms, expires_at_epoch_ms,
                max_capture_bytes, max_chunk_bytes, max_chunks, global_quota_bytes,
                expired_tombstone_ms, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, 'recording')",
            params![
                request.capture_id,
                request_hash,
                request.source.as_str(),
                request.correlation.chat_id,
                request.correlation.event_id,
                request.correlation.invocation_id,
                request.correlation.attempt_id,
                to_i64(policy.generation)?,
                redaction.identity(),
                policy.quota_class,
                to_i64(request.created_at_epoch_ms)?,
                to_i64(expires)?,
                to_i64(policy.max_capture_bytes)?,
                to_i64(policy.max_chunk_bytes)?,
                to_i64(policy.max_chunks)?,
                to_i64(policy.global_quota_bytes)?,
                to_i64(policy.expired_tombstone_ms)?,
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        fs::create_dir_all(self.capture_directory(&request.capture_id))?;
        self.manifest(&request.capture_id).map(Some)
    }

    /// Redacts, bounds, compresses, hashes, and atomically appends one frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture is unavailable, policy generations
    /// differ, or the bounded durable append fails.
    #[allow(clippy::too_many_lines)]
    pub fn append(
        &self,
        frame: &CaptureFrame<'_>,
        redaction: &RedactionSet,
    ) -> Result<CaptureAppendOutcome, CaptureError> {
        self.require_writable()?;
        validate_id(frame.capture_id)?;
        {
            let _lease = self.gate.shared()?;
            let connection = self.lock()?;
            let limits = load_recording_limits(&connection, frame.capture_id)?;
            validate_redaction_identity(&limits, redaction)?;
        }
        let input_bytes =
            u64::try_from(frame.payload.len()).map_err(|_| CaptureError::NumericOverflow)?;
        if input_bytes > HARD_MAX_CHUNK_BYTES {
            return self
                .seal_truncated(
                    frame.capture_id,
                    frame.received_at_epoch_ms,
                    "per_chunk_hard_limit",
                    false,
                )
                .map(CaptureAppendOutcome::Truncated);
        }
        let redacted = match redaction.redact_payload(frame.payload) {
            Ok(redacted) => redacted,
            Err(error) => {
                let manifest = self.seal_truncated(
                    frame.capture_id,
                    frame.received_at_epoch_ms,
                    redaction_reason(&error),
                    true,
                )?;
                return Ok(CaptureAppendOutcome::Truncated(manifest));
            }
        };

        // Raw caller bytes go out of scope before the lock/file path exists;
        // only this redacted owned buffer can cross the persistence boundary.
        let redaction_count = redacted.replacements();
        let payload = redacted.into_bytes();
        let raw_bytes = u64::try_from(payload.len()).map_err(|_| CaptureError::NumericOverflow)?;
        let encoded = compress(&payload);
        let stored_bytes =
            u64::try_from(encoded.len()).map_err(|_| CaptureError::NumericOverflow)?;
        let content_hash = hash_bytes(&payload);

        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let limits = load_recording_limits(&transaction, frame.capture_id)?;
        validate_redaction_identity(&limits, redaction)?;
        let global_stored: i64 = transaction.query_row(
            "SELECT COALESCE(SUM(stored_byte_count), 0) FROM capture_manifests",
            [],
            |row| row.get(0),
        )?;
        let next_raw = limits
            .byte_count
            .checked_add(raw_bytes)
            .ok_or(CaptureError::NumericOverflow)?;
        let next_chunks = limits
            .chunk_count
            .checked_add(1)
            .ok_or(CaptureError::NumericOverflow)?;
        let next_global = from_i64(global_stored)?
            .checked_add(stored_bytes)
            .ok_or(CaptureError::NumericOverflow)?;
        let quota_reason = if raw_bytes > limits.max_chunk_bytes {
            Some("per_chunk_quota")
        } else if next_raw > limits.max_capture_bytes {
            Some("per_capture_byte_quota")
        } else if next_chunks > limits.max_chunks {
            Some("per_capture_chunk_quota")
        } else if next_global > limits.global_quota_bytes {
            Some("global_capture_quota")
        } else {
            None
        };
        if let Some(reason) = quota_reason {
            seal_transaction(
                &transaction,
                frame.capture_id,
                frame.received_at_epoch_ms,
                true,
                Some(reason),
                false,
            )?;
            transaction.commit()?;
            drop(connection);
            return Ok(CaptureAppendOutcome::Truncated(
                self.manifest(frame.capture_id)?,
            ));
        }

        let ordinal = limits.chunk_count;
        let chunk_path = self.chunk_path(frame.capture_id, ordinal);
        write_and_sync_atomic(&chunk_path, &encoded)?;
        let insert = transaction.execute(
            "INSERT INTO capture_chunks(
                capture_id, ordinal, received_at_epoch_ms, byte_count, stored_byte_count,
                content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                frame.capture_id,
                to_i64(ordinal)?,
                to_i64(frame.received_at_epoch_ms)?,
                to_i64(raw_bytes)?,
                to_i64(stored_bytes)?,
                content_hash,
            ],
        );
        if let Err(error) = insert {
            let _ = fs::remove_file(&chunk_path);
            return Err(error.into());
        }
        transaction.execute(
            "UPDATE capture_manifests
             SET chunk_count=?2, byte_count=?3,
                 stored_byte_count=stored_byte_count + ?4,
                 redaction_count=redaction_count + ?5
             WHERE capture_id=?1 AND state='recording'",
            params![
                frame.capture_id,
                to_i64(next_chunks)?,
                to_i64(next_raw)?,
                to_i64(stored_bytes)?,
                to_i64(redaction_count)?,
            ],
        )?;
        transaction.commit()?;
        Ok(CaptureAppendOutcome::Appended(CaptureChunkMetadata {
            ordinal,
            received_at_epoch_ms: frame.received_at_epoch_ms,
            byte_count: raw_bytes,
            stored_byte_count: stored_bytes,
            content_hash,
            redaction_count,
        }))
    }

    /// Atomically seals a normal capture and publishes its aggregate hash.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid/unavailable captures or when the seal
    /// cannot be committed durably.
    pub fn seal(
        &self,
        capture_id: &str,
        sealed_at_epoch_ms: u64,
    ) -> Result<CaptureManifest, CaptureError> {
        self.require_writable()?;
        validate_id(capture_id)?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match load_state(&transaction, capture_id)? {
            CaptureState::Recording => seal_transaction(
                &transaction,
                capture_id,
                sealed_at_epoch_ms,
                false,
                None,
                false,
            )?,
            CaptureState::Available => {}
            other => return Err(CaptureError::Unavailable(other)),
        }
        transaction.commit()?;
        drop(connection);
        self.manifest(capture_id)
    }

    pub(super) fn seal_truncated(
        &self,
        capture_id: &str,
        sealed_at_epoch_ms: u64,
        reason: &str,
        redaction_omission: bool,
    ) -> Result<CaptureManifest, CaptureError> {
        self.require_writable()?;
        let _lease = self.gate.shared()?;
        self.seal_truncated_with_maintenance_held(
            capture_id,
            sealed_at_epoch_ms,
            reason,
            redaction_omission,
        )
    }

    pub(super) fn seal_truncated_with_maintenance_held(
        &self,
        capture_id: &str,
        sealed_at_epoch_ms: u64,
        reason: &str,
        redaction_omission: bool,
    ) -> Result<CaptureManifest, CaptureError> {
        validate_id(capture_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        seal_transaction(
            &transaction,
            capture_id,
            sealed_at_epoch_ms,
            true,
            Some(reason),
            redaction_omission,
        )?;
        transaction.commit()?;
        load_manifest(&connection, capture_id)?.ok_or(CaptureError::UnknownCapture)
    }
}

fn validate_redaction_identity(
    limits: &super::common::RecordingLimits,
    redaction: &RedactionSet,
) -> Result<(), CaptureError> {
    if limits.policy_generation != redaction.generation() {
        return Err(CaptureError::RedactionGenerationMismatch);
    }
    if limits.redaction_set_id != redaction.identity() {
        return Err(CaptureError::RedactionIdentityMismatch);
    }
    Ok(())
}
