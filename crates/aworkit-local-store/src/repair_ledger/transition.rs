//! Global hash-chained lifecycle transition append.

use rusqlite::{OptionalExtension, Transaction, params};

use super::{
    ErrorGroupStatus, RepairLedgerError, RepairTransition,
    common::{canonical_hash, canonical_json, from_i64, to_i64},
};

pub(super) fn append_transition(
    transaction: &Transaction<'_>,
    fingerprint: &str,
    from: Option<ErrorGroupStatus>,
    to: ErrorGroupStatus,
    kind: &str,
    occurred_at_epoch_ms: u64,
) -> Result<RepairTransition, RepairLedgerError> {
    let sequence_i64: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM repair_transitions",
        [],
        |row| row.get(0),
    )?;
    let sequence = from_i64(sequence_i64)?;
    let previous_transition_hash: Option<String> = transaction
        .query_row(
            "SELECT transition_hash FROM repair_transitions ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let body = (
        sequence,
        fingerprint,
        from,
        to,
        kind,
        occurred_at_epoch_ms,
        &previous_transition_hash,
    );
    let transition_hash = canonical_hash(&body)?;
    let transition = RepairTransition {
        sequence,
        fingerprint: fingerprint.to_owned(),
        from,
        to,
        kind: kind.to_owned(),
        occurred_at_epoch_ms,
        previous_transition_hash: previous_transition_hash.clone(),
        transition_hash: transition_hash.clone(),
    };
    transaction.execute(
        "INSERT INTO repair_transitions(
            sequence, fingerprint, from_status, to_status, kind, occurred_at_epoch_ms,
            previous_transition_hash, record_json, transition_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            to_i64(sequence)?,
            fingerprint,
            from.map(ErrorGroupStatus::as_str),
            to.as_str(),
            kind,
            to_i64(occurred_at_epoch_ms)?,
            previous_transition_hash,
            canonical_json(&transition)?,
            transition_hash,
        ],
    )?;
    transaction.execute(
        "INSERT INTO repair_transition_head(singleton, sequence, transition_hash)
         VALUES (1, ?1, ?2)
         ON CONFLICT(singleton) DO UPDATE SET
            sequence=excluded.sequence,
            transition_hash=excluded.transition_hash",
        params![to_i64(sequence)?, transition_hash],
    )?;
    Ok(transition)
}
