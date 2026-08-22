//! Durable opaque trusted-core events with group and global ordering.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    CoreEventAppendBatchReceipt, CoreEventAppendBatchRequest, CoreEventAppendReceipt,
    CoreEventAppendRequest, CoreEventInput, CoreEventVersions, RepairEvidenceLedger,
    RepairLedgerError, StoredCoreEvent,
    common::{canonical_hash, canonical_json, decode_verified, from_i64, hash_bytes, to_i64},
    validation::{validate_hash, validate_id, validate_page, validate_redacted},
};
use crate::RedactionSet;

const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_BATCH_EVENTS: usize = 128;

impl RepairEvidenceLedger {
    /// Appends one opaque core event after validating its redaction boundary,
    /// operation idempotency, and exact per-group compare-and-swap version.
    pub fn append_core_event(
        &self,
        request: &CoreEventAppendRequest,
        redaction: &RedactionSet,
    ) -> Result<CoreEventAppendReceipt, RepairLedgerError> {
        let batch = CoreEventAppendBatchRequest {
            operation_id: request.operation_id.clone(),
            group_id: request.group_id.clone(),
            expected_group_sequence: request.expected_group_sequence,
            events: vec![CoreEventInput {
                event_fingerprint: request.event_fingerprint.clone(),
                occurred_at_epoch_ms: request.occurred_at_epoch_ms,
                event: request.event.clone(),
            }],
        };
        let receipt = self.append_core_events(&batch, redaction)?;
        let event = receipt
            .events
            .into_iter()
            .next()
            .ok_or(RepairLedgerError::Integrity)?;
        Ok(CoreEventAppendReceipt {
            event,
            current_group_sequence: receipt.current_group_sequence,
            current_global_version: receipt.current_global_version,
            duplicate: receipt.duplicate,
        })
    }

    /// Atomically appends one non-empty batch to a single group stream. Every
    /// event receives contiguous group/global versions, while one operation
    /// receipt makes retries idempotent before compare-and-swap validation.
    pub fn append_core_events(
        &self,
        request: &CoreEventAppendBatchRequest,
        redaction: &RedactionSet,
    ) -> Result<CoreEventAppendBatchReceipt, RepairLedgerError> {
        let canonical_events = validate_batch_request(request, redaction)?;
        self.require_writable()?;
        let request_hash = canonical_hash(request)?;
        let _lease = self.gate.shared()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(receipt) = prior_append(&transaction, request, &request_hash)? {
            transaction.commit()?;
            return Ok(receipt);
        }

        let group = load_group_head(&transaction, &request.group_id)?;
        let actual_group_sequence = group.as_ref().map_or(0, |(sequence, _)| *sequence);
        if request.expected_group_sequence != actual_group_sequence {
            return Err(RepairLedgerError::CoreEventVersionConflict {
                expected: request.expected_group_sequence,
                actual: actual_group_sequence,
            });
        }
        if let Some((sequence, head)) = &group {
            verify_group_tail(&transaction, &request.group_id, *sequence, head)?;
        }

        let (global_version, previous_global_event_hash) = load_global_head(&transaction)?;
        if global_version > 0 {
            verify_global_tail(
                &transaction,
                global_version,
                previous_global_event_hash
                    .as_deref()
                    .ok_or(RepairLedgerError::Integrity)?,
            )?;
        }
        let mut group_sequence = actual_group_sequence;
        let mut global_sequence = global_version;
        let mut previous_group_event_hash = group.map(|(_, hash)| hash);
        let mut previous_global_event_hash = previous_global_event_hash;
        let mut events = Vec::with_capacity(request.events.len());
        for (input, canonical_event_json) in request.events.iter().zip(canonical_events) {
            group_sequence = group_sequence
                .checked_add(1)
                .ok_or(RepairLedgerError::NumericOverflow)?;
            global_sequence = global_sequence
                .checked_add(1)
                .ok_or(RepairLedgerError::NumericOverflow)?;
            let event_content_hash = hash_bytes(canonical_event_json.as_bytes());
            let event_hash = canonical_hash(&(
                global_sequence,
                &request.group_id,
                group_sequence,
                &request.operation_id,
                &input.event_fingerprint,
                input.occurred_at_epoch_ms,
                &canonical_event_json,
                &event_content_hash,
                &previous_group_event_hash,
                &previous_global_event_hash,
            ))?;
            let event = StoredCoreEvent {
                global_sequence,
                group_id: request.group_id.clone(),
                group_sequence,
                operation_id: request.operation_id.clone(),
                event_fingerprint: input.event_fingerprint.clone(),
                occurred_at_epoch_ms: input.occurred_at_epoch_ms,
                canonical_event_json,
                event_content_hash,
                previous_group_event_hash: previous_group_event_hash.clone(),
                previous_global_event_hash: previous_global_event_hash.clone(),
                event_hash: event_hash.clone(),
            };
            previous_group_event_hash = Some(event_hash.clone());
            previous_global_event_hash = Some(event_hash);
            events.push(event);
        }
        let tail = events.last().ok_or(RepairLedgerError::InvalidRecord)?;

        transaction.execute(
            "INSERT INTO core_event_groups(group_id, current_sequence, head_event_hash)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(group_id) DO UPDATE SET
                current_sequence=excluded.current_sequence,
                head_event_hash=excluded.head_event_hash",
            params![request.group_id, to_i64(group_sequence)?, tail.event_hash],
        )?;
        for event in &events {
            insert_event(&transaction, event)?;
        }
        transaction.execute(
            "UPDATE core_event_meta
             SET current_global_version=?1, head_event_hash=?2 WHERE singleton=1",
            params![to_i64(global_sequence)?, tail.event_hash],
        )?;
        let receipt = CoreEventAppendBatchReceipt {
            events,
            current_group_sequence: group_sequence,
            current_global_version: global_sequence,
            duplicate: false,
        };
        store_append(&transaction, request, &request_hash, &receipt)?;
        transaction.commit()?;
        Ok(receipt)
    }

    /// Returns current per-group and global replay versions. Unknown groups
    /// have sequence zero.
    pub fn core_event_versions(
        &self,
        group_id: &str,
    ) -> Result<CoreEventVersions, RepairLedgerError> {
        validate_id(group_id)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let (current_global_version, _) = load_global_head(&connection)?;
        let current_group_sequence = match load_group_head(&connection, group_id)? {
            Some((sequence, head)) => {
                verify_group_tail(&connection, group_id, sequence, &head)?;
                sequence
            }
            None => 0,
        };
        if current_global_version > 0 {
            let (_, head) = load_global_head(&connection)?;
            verify_global_tail(
                &connection,
                current_global_version,
                head.as_deref().ok_or(RepairLedgerError::Integrity)?,
            )?;
        }
        Ok(CoreEventVersions {
            group_id: group_id.to_owned(),
            current_group_sequence,
            current_global_version,
        })
    }

    /// Pages group identifiers in stable lexical order.
    pub fn core_event_group_ids(
        &self,
        after_group_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, RepairLedgerError> {
        validate_page(limit)?;
        if let Some(cursor) = after_group_id {
            validate_id(cursor)?;
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT group_id, current_sequence, head_event_hash
             FROM core_event_groups WHERE group_id > COALESCE(?1, '')
             ORDER BY group_id LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after_group_id, i64::from(limit)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut ids = Vec::with_capacity(rows.len());
        for (group_id, sequence, head) in rows {
            validate_id(&group_id).map_err(|_| RepairLedgerError::Integrity)?;
            let sequence = from_i64(sequence)?;
            validate_hash(&head).map_err(|_| RepairLedgerError::Integrity)?;
            verify_group_tail(&connection, &group_id, sequence, &head)?;
            ids.push(group_id);
        }
        Ok(ids)
    }

    /// Replays a single group's events after an exact group-sequence cursor.
    pub fn load_core_events(
        &self,
        group_id: &str,
        after_group_sequence: u64,
        limit: u32,
    ) -> Result<Vec<StoredCoreEvent>, RepairLedgerError> {
        validate_id(group_id)?;
        validate_page(limit)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let group = load_group_head(&connection, group_id)?;
        let Some((current, head)) = group else {
            return if after_group_sequence == 0 {
                Ok(Vec::new())
            } else {
                Err(RepairLedgerError::Integrity)
            };
        };
        verify_group_tail(&connection, group_id, current, &head)?;
        if after_group_sequence > current {
            return Err(RepairLedgerError::Integrity);
        }
        let predecessor = if after_group_sequence == 0 {
            None
        } else {
            Some(load_group_event_exact(
                &connection,
                group_id,
                after_group_sequence,
            )?)
        };
        let mut statement = connection.prepare(
            "SELECT global_sequence, group_id, group_sequence, operation_id,
                    event_fingerprint, occurred_at_epoch_ms, canonical_event_json,
                    event_content_hash, previous_group_event_hash,
                    previous_global_event_hash, event_hash
             FROM core_events WHERE group_id=?1 AND group_sequence>?2
             ORDER BY group_sequence LIMIT ?3",
        )?;
        let rows = event_rows(
            &mut statement,
            params![group_id, to_i64(after_group_sequence)?, i64::from(limit)],
        )?;
        verify_group_page(
            group_id,
            after_group_sequence,
            predecessor.as_ref(),
            current,
            limit,
            &rows,
        )?;
        Ok(rows)
    }

    /// Replays all opaque core events after an exact global-version cursor.
    pub fn load_all_core_events_after(
        &self,
        after_global_sequence: u64,
        limit: u32,
    ) -> Result<Vec<StoredCoreEvent>, RepairLedgerError> {
        validate_page(limit)?;
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let (current, head) = load_global_head(&connection)?;
        if current > 0 {
            verify_global_tail(
                &connection,
                current,
                head.as_deref().ok_or(RepairLedgerError::Integrity)?,
            )?;
        }
        if after_global_sequence > current {
            return Err(RepairLedgerError::Integrity);
        }
        let predecessor = if after_global_sequence == 0 {
            None
        } else {
            Some(load_global_event_exact(&connection, after_global_sequence)?)
        };
        let mut statement = connection.prepare(
            "SELECT global_sequence, group_id, group_sequence, operation_id,
                    event_fingerprint, occurred_at_epoch_ms, canonical_event_json,
                    event_content_hash, previous_group_event_hash,
                    previous_global_event_hash, event_hash
             FROM core_events WHERE global_sequence>?1
             ORDER BY global_sequence LIMIT ?2",
        )?;
        let rows = event_rows(
            &mut statement,
            params![to_i64(after_global_sequence)?, i64::from(limit)],
        )?;
        verify_global_page(
            after_global_sequence,
            predecessor.as_ref(),
            current,
            limit,
            &rows,
        )?;
        Ok(rows)
    }
}

fn validate_batch_request(
    request: &CoreEventAppendBatchRequest,
    redaction: &RedactionSet,
) -> Result<Vec<String>, RepairLedgerError> {
    validate_id(&request.operation_id)?;
    validate_id(&request.group_id)?;
    if request.events.is_empty() || request.events.len() > MAX_BATCH_EVENTS {
        return Err(RepairLedgerError::InvalidRecord);
    }
    let mut total = 0_usize;
    let mut canonical = Vec::with_capacity(request.events.len());
    for event in &request.events {
        validate_id(&event.event_fingerprint)?;
        let json = canonical_json(&event.event)?;
        if json.len() > MAX_EVENT_BYTES {
            return Err(RepairLedgerError::InvalidRecord);
        }
        total = total
            .checked_add(json.len())
            .ok_or(RepairLedgerError::NumericOverflow)?;
        if total > MAX_BATCH_BYTES {
            return Err(RepairLedgerError::InvalidRecord);
        }
        canonical.push(json);
    }
    validate_redacted(request, redaction)?;
    Ok(canonical)
}

fn prior_append(
    transaction: &Transaction<'_>,
    request: &CoreEventAppendBatchRequest,
    request_hash: &str,
) -> Result<Option<CoreEventAppendBatchReceipt>, RepairLedgerError> {
    let prior = transaction
        .query_row(
            "SELECT request_hash, receipt_json, receipt_hash
             FROM core_event_operations WHERE operation_id=?1",
            [&request.operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    match prior {
        None => Ok(None),
        Some((stored, _, _)) if stored != request_hash => Err(RepairLedgerError::OperationConflict),
        Some((_, json, hash)) => {
            let mut receipt: CoreEventAppendBatchReceipt = decode_verified(&json, &hash)?;
            if receipt.events.is_empty()
                || receipt.events.iter().any(|event| {
                    event.operation_id != request.operation_id || event.group_id != request.group_id
                })
                || receipt.current_group_sequence
                    != receipt
                        .events
                        .last()
                        .map_or(0, |event| event.group_sequence)
                || receipt.current_global_version
                    != receipt
                        .events
                        .last()
                        .map_or(0, |event| event.global_sequence)
            {
                return Err(RepairLedgerError::Integrity);
            }
            for event in &receipt.events {
                validate_stored_event(event)?;
                if load_global_event_exact(transaction, event.global_sequence)? != *event {
                    return Err(RepairLedgerError::Integrity);
                }
            }
            let stored_event_count: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM core_events WHERE operation_id=?1",
                [&request.operation_id],
                |row| row.get(0),
            )?;
            if u64::try_from(stored_event_count).map_err(|_| RepairLedgerError::Integrity)?
                != u64::try_from(receipt.events.len())
                    .map_err(|_| RepairLedgerError::NumericOverflow)?
            {
                return Err(RepairLedgerError::Integrity);
            }
            verify_receipt_continuity(&receipt.events, request.expected_group_sequence)?;
            receipt.duplicate = true;
            Ok(Some(receipt))
        }
    }
}

fn store_append(
    transaction: &Transaction<'_>,
    request: &CoreEventAppendBatchRequest,
    request_hash: &str,
    receipt: &CoreEventAppendBatchReceipt,
) -> Result<(), RepairLedgerError> {
    let receipt_json = canonical_json(receipt)?;
    let receipt_hash = hash_bytes(receipt_json.as_bytes());
    transaction.execute(
        "INSERT INTO core_event_operations(
            operation_id, request_hash, receipt_json, receipt_hash, created_at_epoch_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            request.operation_id,
            request_hash,
            receipt_json,
            receipt_hash,
            to_i64(
                request
                    .events
                    .last()
                    .ok_or(RepairLedgerError::InvalidRecord)?
                    .occurred_at_epoch_ms,
            )?,
        ],
    )?;
    Ok(())
}

fn verify_receipt_continuity(
    events: &[StoredCoreEvent],
    expected_group_sequence: u64,
) -> Result<(), RepairLedgerError> {
    if events.first().map(|event| event.group_sequence) != expected_group_sequence.checked_add(1) {
        return Err(RepairLedgerError::Integrity);
    }
    for pair in events.windows(2) {
        let previous = &pair[0];
        let event = &pair[1];
        if event.group_sequence != previous.group_sequence.saturating_add(1)
            || event.global_sequence != previous.global_sequence.saturating_add(1)
            || event.previous_group_event_hash.as_deref() != Some(&previous.event_hash)
            || event.previous_global_event_hash.as_deref() != Some(&previous.event_hash)
        {
            return Err(RepairLedgerError::Integrity);
        }
    }
    Ok(())
}

fn insert_event(
    transaction: &Transaction<'_>,
    event: &StoredCoreEvent,
) -> Result<(), RepairLedgerError> {
    transaction.execute(
        "INSERT INTO core_events(
            global_sequence, group_id, group_sequence, operation_id,
            event_fingerprint, occurred_at_epoch_ms, canonical_event_json,
            event_content_hash, previous_group_event_hash,
            previous_global_event_hash, event_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            to_i64(event.global_sequence)?,
            event.group_id,
            to_i64(event.group_sequence)?,
            event.operation_id,
            event.event_fingerprint,
            to_i64(event.occurred_at_epoch_ms)?,
            event.canonical_event_json,
            event.event_content_hash,
            event.previous_group_event_hash,
            event.previous_global_event_hash,
            event.event_hash,
        ],
    )?;
    Ok(())
}

fn load_group_head(
    connection: &Connection,
    group_id: &str,
) -> Result<Option<(u64, String)>, RepairLedgerError> {
    connection
        .query_row(
            "SELECT current_sequence, head_event_hash
             FROM core_event_groups WHERE group_id=?1",
            [group_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(sequence, hash)| {
            validate_hash(&hash).map_err(|_| RepairLedgerError::Integrity)?;
            Ok((from_i64(sequence)?, hash))
        })
        .transpose()
}

fn load_global_head(connection: &Connection) -> Result<(u64, Option<String>), RepairLedgerError> {
    let (version, head) = connection.query_row(
        "SELECT current_global_version, head_event_hash
         FROM core_event_meta WHERE singleton=1",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let version = from_i64(version)?;
    if (version == 0) != head.is_none()
        || head
            .as_deref()
            .is_some_and(|hash| validate_hash(hash).is_err())
    {
        return Err(RepairLedgerError::Integrity);
    }
    Ok((version, head))
}

fn verify_group_tail(
    connection: &Connection,
    group_id: &str,
    sequence: u64,
    expected_hash: &str,
) -> Result<(), RepairLedgerError> {
    let event = load_group_event_exact(connection, group_id, sequence)?;
    if event.event_hash != expected_hash {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}

fn verify_global_tail(
    connection: &Connection,
    sequence: u64,
    expected_hash: &str,
) -> Result<(), RepairLedgerError> {
    let event = load_global_event_exact(connection, sequence)?;
    if event.event_hash != expected_hash {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}

fn load_group_event_exact(
    connection: &Connection,
    group_id: &str,
    sequence: u64,
) -> Result<StoredCoreEvent, RepairLedgerError> {
    load_event_exact(
        connection,
        "group_id=?1 AND group_sequence=?2",
        params![group_id, to_i64(sequence)?],
    )
}

fn load_global_event_exact(
    connection: &Connection,
    sequence: u64,
) -> Result<StoredCoreEvent, RepairLedgerError> {
    load_event_exact(connection, "global_sequence=?1", params![to_i64(sequence)?])
}

fn load_event_exact<P: rusqlite::Params>(
    connection: &Connection,
    predicate: &str,
    parameters: P,
) -> Result<StoredCoreEvent, RepairLedgerError> {
    let sql = format!(
        "SELECT global_sequence, group_id, group_sequence, operation_id,
                event_fingerprint, occurred_at_epoch_ms, canonical_event_json,
                event_content_hash, previous_group_event_hash,
                previous_global_event_hash, event_hash
         FROM core_events WHERE {predicate}"
    );
    let raw = connection
        .query_row(&sql, parameters, read_raw_event)
        .optional()?
        .ok_or(RepairLedgerError::Integrity)?;
    decode_event(raw)
}

type RawEvent = (
    i64,
    String,
    i64,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

fn read_raw_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn decode_event(raw: RawEvent) -> Result<StoredCoreEvent, RepairLedgerError> {
    let event = StoredCoreEvent {
        global_sequence: from_i64(raw.0)?,
        group_id: raw.1,
        group_sequence: from_i64(raw.2)?,
        operation_id: raw.3,
        event_fingerprint: raw.4,
        occurred_at_epoch_ms: from_i64(raw.5)?,
        canonical_event_json: raw.6,
        event_content_hash: raw.7,
        previous_group_event_hash: raw.8,
        previous_global_event_hash: raw.9,
        event_hash: raw.10,
    };
    validate_stored_event(&event)?;
    Ok(event)
}

fn event_rows<P: rusqlite::Params>(
    statement: &mut rusqlite::Statement<'_>,
    parameters: P,
) -> Result<Vec<StoredCoreEvent>, RepairLedgerError> {
    statement
        .query_map(parameters, read_raw_event)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(decode_event)
        .collect()
}

fn validate_stored_event(event: &StoredCoreEvent) -> Result<(), RepairLedgerError> {
    validate_id(&event.group_id).map_err(|_| RepairLedgerError::Integrity)?;
    validate_id(&event.operation_id).map_err(|_| RepairLedgerError::Integrity)?;
    validate_id(&event.event_fingerprint).map_err(|_| RepairLedgerError::Integrity)?;
    validate_hash(&event.event_content_hash).map_err(|_| RepairLedgerError::Integrity)?;
    validate_hash(&event.event_hash).map_err(|_| RepairLedgerError::Integrity)?;
    for hash in [
        event.previous_group_event_hash.as_deref(),
        event.previous_global_event_hash.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_hash(hash).map_err(|_| RepairLedgerError::Integrity)?;
    }
    if event.global_sequence == 0
        || event.group_sequence == 0
        || event.canonical_event_json.len() > MAX_EVENT_BYTES
        || hash_bytes(event.canonical_event_json.as_bytes()) != event.event_content_hash
    {
        return Err(RepairLedgerError::Integrity);
    }
    let value: serde_json::Value = serde_json::from_str(&event.canonical_event_json)?;
    if canonical_json(&value)? != event.canonical_event_json {
        return Err(RepairLedgerError::Integrity);
    }
    let expected = canonical_hash(&(
        event.global_sequence,
        &event.group_id,
        event.group_sequence,
        &event.operation_id,
        &event.event_fingerprint,
        event.occurred_at_epoch_ms,
        &event.canonical_event_json,
        &event.event_content_hash,
        &event.previous_group_event_hash,
        &event.previous_global_event_hash,
    ))?;
    if expected != event.event_hash {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}

fn verify_group_page(
    group_id: &str,
    after: u64,
    predecessor: Option<&StoredCoreEvent>,
    current: u64,
    limit: u32,
    events: &[StoredCoreEvent],
) -> Result<(), RepairLedgerError> {
    let mut expected_sequence = after
        .checked_add(1)
        .ok_or(RepairLedgerError::NumericOverflow)?;
    let mut previous = predecessor.map(|event| event.event_hash.as_str());
    for event in events {
        if event.group_id != group_id
            || event.group_sequence != expected_sequence
            || event.previous_group_event_hash.as_deref() != previous
        {
            return Err(RepairLedgerError::Integrity);
        }
        previous = Some(&event.event_hash);
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
    }
    let consumed = after
        .checked_add(u64::try_from(events.len()).map_err(|_| RepairLedgerError::NumericOverflow)?)
        .ok_or(RepairLedgerError::NumericOverflow)?;
    if events.len() < usize::try_from(limit).map_err(|_| RepairLedgerError::NumericOverflow)?
        && consumed != current
    {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}

fn verify_global_page(
    after: u64,
    predecessor: Option<&StoredCoreEvent>,
    current: u64,
    limit: u32,
    events: &[StoredCoreEvent],
) -> Result<(), RepairLedgerError> {
    let mut expected_sequence = after
        .checked_add(1)
        .ok_or(RepairLedgerError::NumericOverflow)?;
    let mut previous = predecessor.map(|event| event.event_hash.as_str());
    for event in events {
        if event.global_sequence != expected_sequence
            || event.previous_global_event_hash.as_deref() != previous
        {
            return Err(RepairLedgerError::Integrity);
        }
        previous = Some(&event.event_hash);
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
    }
    let consumed = after
        .checked_add(u64::try_from(events.len()).map_err(|_| RepairLedgerError::NumericOverflow)?)
        .ok_or(RepairLedgerError::NumericOverflow)?;
    if events.len() < usize::try_from(limit).map_err(|_| RepairLedgerError::NumericOverflow)?
        && consumed != current
    {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn check_integrity(
    connection: &Connection,
    errors: &mut Vec<String>,
) -> Result<(), RepairLedgerError> {
    let mut statement = connection.prepare(
        "SELECT global_sequence, group_id, group_sequence, operation_id,
                event_fingerprint, occurred_at_epoch_ms, canonical_event_json,
                event_content_hash, previous_group_event_hash,
                previous_global_event_hash, event_hash
         FROM core_events ORDER BY global_sequence",
    )?;
    let raw = statement
        .query_map([], read_raw_event)?
        .collect::<Result<Vec<_>, _>>()?;
    let mut events = Vec::with_capacity(raw.len());
    let mut expected_global = 1_u64;
    let mut previous_global: Option<String> = None;
    let mut groups = std::collections::BTreeMap::<String, (u64, String)>::new();
    let mut operations = std::collections::BTreeMap::<String, Vec<StoredCoreEvent>>::new();
    for raw_event in raw {
        let sequence_label = raw_event.0;
        match decode_event(raw_event) {
            Ok(event) => {
                let expected_group = groups
                    .get(&event.group_id)
                    .map_or(1, |(sequence, _)| sequence.saturating_add(1));
                let expected_group_previous =
                    groups.get(&event.group_id).map(|(_, hash)| hash.as_str());
                if event.global_sequence != expected_global
                    || event.previous_global_event_hash != previous_global
                    || event.group_sequence != expected_group
                    || event.previous_group_event_hash.as_deref() != expected_group_previous
                {
                    errors.push(format!("core_event:{}:chain", event.global_sequence));
                }
                expected_global = event.global_sequence.saturating_add(1);
                previous_global = Some(event.event_hash.clone());
                groups.insert(
                    event.group_id.clone(),
                    (event.group_sequence, event.event_hash.clone()),
                );
                operations
                    .entry(event.operation_id.clone())
                    .or_default()
                    .push(event.clone());
                events.push(event);
            }
            Err(_) => errors.push(format!("core_event:{sequence_label}:record")),
        }
    }

    let meta = connection.query_row(
        "SELECT current_global_version, head_event_hash
         FROM core_event_meta WHERE singleton=1",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let expected_meta = events.last().map_or((0, None), |event| {
        (event.global_sequence, Some(event.event_hash.as_str()))
    });
    if from_i64(meta.0).ok() != Some(expected_meta.0) || meta.1.as_deref() != expected_meta.1 {
        errors.push("core_event_meta:mismatch".to_owned());
    }

    let mut group_statement = connection.prepare(
        "SELECT group_id, current_sequence, head_event_hash
         FROM core_event_groups ORDER BY group_id",
    )?;
    let stored_groups = group_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if stored_groups.len() != groups.len() {
        errors.push("core_event_groups:count".to_owned());
    }
    for (group_id, sequence, hash) in stored_groups {
        let valid = groups.get(&group_id).is_some_and(|expected| {
            from_i64(sequence).ok() == Some(expected.0) && hash == expected.1
        });
        if !valid {
            errors.push(format!("core_event_group:{group_id}:head"));
        }
    }

    let mut operation_statement = connection.prepare(
        "SELECT operation_id, request_hash, receipt_json, receipt_hash
         FROM core_event_operations ORDER BY operation_id",
    )?;
    let stored_operations = operation_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if stored_operations.len() != operations.len() {
        errors.push("core_event_operations:count".to_owned());
    }
    for (operation_id, request_hash, receipt_json, receipt_hash) in stored_operations {
        let expected_events = operations.get(&operation_id);
        let receipt = decode_verified::<CoreEventAppendBatchReceipt>(&receipt_json, &receipt_hash);
        let valid = receipt.as_ref().is_ok_and(|receipt| {
            let Some(expected_events) = expected_events else {
                return false;
            };
            if receipt.duplicate
                || receipt.events != *expected_events
                || receipt.current_group_sequence
                    != expected_events
                        .last()
                        .map_or(0, |event| event.group_sequence)
                || receipt.current_global_version
                    != expected_events
                        .last()
                        .map_or(0, |event| event.global_sequence)
                || canonical_json(receipt).as_deref().ok() != Some(receipt_json.as_str())
            {
                return false;
            }
            let Some(first) = expected_events.first() else {
                return false;
            };
            let event_inputs = expected_events
                .iter()
                .map(|event| {
                    serde_json::from_str(&event.canonical_event_json).map(|value| CoreEventInput {
                        event_fingerprint: event.event_fingerprint.clone(),
                        occurred_at_epoch_ms: event.occurred_at_epoch_ms,
                        event: value,
                    })
                })
                .collect::<Result<Vec<_>, _>>();
            let Ok(event_inputs) = event_inputs else {
                return false;
            };
            let request = CoreEventAppendBatchRequest {
                operation_id: operation_id.clone(),
                group_id: first.group_id.clone(),
                expected_group_sequence: first.group_sequence.saturating_sub(1),
                events: event_inputs,
            };
            canonical_hash(&request).as_deref().ok() == Some(request_hash.as_str())
        });
        if !valid
            || validate_id(&operation_id).is_err()
            || validate_hash(&request_hash).is_err()
            || validate_hash(&receipt_hash).is_err()
        {
            errors.push(format!("core_event_operation:{operation_id}"));
        }
    }
    Ok(())
}
