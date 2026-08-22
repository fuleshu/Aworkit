//! Canonical record hashing, idempotency, and shared `SQLite` codecs.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use super::{ErrorGroup, EvidenceAvailability, EvidenceReference, RepairLedgerError};

pub(super) fn load_group(
    connection: &Connection,
    fingerprint: &str,
) -> Result<Option<ErrorGroup>, RepairLedgerError> {
    let stored = connection
        .query_row(
            "SELECT record_json, record_hash FROM error_groups WHERE fingerprint=?1",
            [fingerprint],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    stored
        .map(|(json, hash)| decode_verified(&json, &hash))
        .transpose()
}

pub(super) fn persist_group(
    transaction: &Transaction<'_>,
    group: &ErrorGroup,
) -> Result<(), RepairLedgerError> {
    let json = canonical_json(group)?;
    let hash = hash_bytes(json.as_bytes());
    transaction.execute(
        "INSERT INTO error_groups(
            fingerprint, ledger_version, status, occurrence_count, first_seen_epoch_ms,
            last_seen_epoch_ms, active_candidate_id, active_candidate_version,
            record_json, record_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(fingerprint) DO UPDATE SET
            ledger_version=excluded.ledger_version,
            status=excluded.status,
            occurrence_count=excluded.occurrence_count,
            first_seen_epoch_ms=excluded.first_seen_epoch_ms,
            last_seen_epoch_ms=excluded.last_seen_epoch_ms,
            active_candidate_id=excluded.active_candidate_id,
            active_candidate_version=excluded.active_candidate_version,
            record_json=excluded.record_json,
            record_hash=excluded.record_hash",
        params![
            group.fingerprint,
            to_i64(group.ledger_version)?,
            group.status.as_str(),
            to_i64(group.occurrence_count)?,
            to_i64(group.first_seen_epoch_ms)?,
            to_i64(group.last_seen_epoch_ms)?,
            group.active_candidate_id,
            group.active_candidate_version.map(to_i64).transpose()?,
            json,
            hash,
        ],
    )?;
    Ok(())
}

pub(super) fn prior_operation<T: DeserializeOwned>(
    transaction: &Transaction<'_>,
    operation_id: &str,
    request_hash: &str,
) -> Result<Option<T>, RepairLedgerError> {
    let prior = transaction
        .query_row(
            "SELECT request_hash, response_json, response_hash
             FROM repair_operations WHERE operation_id=?1",
            [operation_id],
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
        Some((hash, _, _)) if hash != request_hash => Err(RepairLedgerError::OperationConflict),
        Some((_, response, response_hash)) => Ok(Some(decode_verified(&response, &response_hash)?)),
    }
}

pub(super) fn store_operation<T: Serialize>(
    transaction: &Transaction<'_>,
    operation_id: &str,
    request_hash: &str,
    created_at_epoch_ms: u64,
    response: &T,
) -> Result<(), RepairLedgerError> {
    let response_json = canonical_json(response)?;
    let response_hash = hash_bytes(response_json.as_bytes());
    transaction.execute(
        "INSERT INTO repair_operations(
            operation_id, request_hash, response_json, response_hash, created_at_epoch_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            operation_id,
            request_hash,
            response_json,
            response_hash,
            to_i64(created_at_epoch_ms)?,
        ],
    )?;
    Ok(())
}

pub(super) fn ensure_version(group: &ErrorGroup, expected: u64) -> Result<(), RepairLedgerError> {
    if group.ledger_version == expected {
        Ok(())
    } else {
        Err(RepairLedgerError::VersionConflict {
            expected,
            actual: group.ledger_version,
        })
    }
}

pub(super) fn immutable_exists(
    transaction: &Transaction<'_>,
    table: &str,
    id_column: &str,
    id: &str,
) -> Result<bool, RepairLedgerError> {
    let sql = format!("SELECT 1 FROM {table} WHERE {id_column}=?1");
    Ok(transaction
        .query_row(&sql, [id], |_| Ok(()))
        .optional()?
        .is_some())
}

pub(super) enum SqlValue<'a> {
    Text(&'a str),
    Integer(u64),
}

pub(super) fn insert_immutable<T: Serialize>(
    transaction: &Transaction<'_>,
    table: &str,
    columns: &[(&str, SqlValue<'_>)],
    record: &T,
) -> Result<(), RepairLedgerError> {
    let json = canonical_json(record)?;
    let hash = hash_bytes(json.as_bytes());
    let names = columns
        .iter()
        .map(|(name, _)| *name)
        .chain(["record_json", "record_hash"])
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=columns.len() + 2)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO {table}({names}) VALUES ({placeholders})");
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(columns.len() + 2);
    for (_, value) in columns {
        values.push(match value {
            SqlValue::Text(value) => rusqlite::types::Value::Text((*value).to_owned()),
            SqlValue::Integer(value) => rusqlite::types::Value::Integer(to_i64(*value)?),
        });
    }
    values.push(rusqlite::types::Value::Text(json));
    values.push(rusqlite::types::Value::Text(hash));
    transaction.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(())
}

pub(super) fn canonical_json(value: &impl Serialize) -> Result<String, RepairLedgerError> {
    String::from_utf8(serde_jcs::to_vec(value)?).map_err(|_| RepairLedgerError::Corrupt)
}

pub(super) fn canonical_hash(value: &impl Serialize) -> Result<String, RepairLedgerError> {
    Ok(hash_bytes(canonical_json(value)?.as_bytes()))
}

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(super) fn decode_verified<T: DeserializeOwned>(
    json: &str,
    expected_hash: &str,
) -> Result<T, RepairLedgerError> {
    if hash_bytes(json.as_bytes()) != expected_hash {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(serde_json::from_str(json)?)
}

pub(super) fn evidence_available(
    connection: &Connection,
    reference: &EvidenceReference,
) -> Result<bool, RepairLedgerError> {
    if reference.availability != EvidenceAvailability::Available {
        return Ok(false);
    }
    let tombstone: Option<String> = connection
        .query_row(
            "SELECT record_json FROM evidence_tombstones
             WHERE artifact_id=?1 AND content_hash=?2
             ORDER BY recorded_at_epoch_ms DESC LIMIT 1",
            params![reference.artifact_id, reference.content_hash],
            |row| row.get(0),
        )
        .optional()?;
    Ok(tombstone.is_none())
}

pub(super) fn to_i64(value: u64) -> Result<i64, RepairLedgerError> {
    i64::try_from(value).map_err(|_| RepairLedgerError::NumericOverflow)
}

pub(super) fn from_i64(value: i64) -> Result<u64, RepairLedgerError> {
    u64::try_from(value).map_err(|_| RepairLedgerError::Corrupt)
}
