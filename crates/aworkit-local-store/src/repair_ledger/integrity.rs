//! Read-only integrity and transition-ledger inspection.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    ErrorGroup, ErrorGroupStatus, RepairEvidenceLedger, RepairIntegrityReport, RepairLedgerError,
    RepairTransition,
    common::{canonical_hash, canonical_json, decode_verified, from_i64, hash_bytes, to_i64},
    validation::{validate_hash, validate_id},
};

const MAX_PAGE: u32 = 512;

impl RepairEvidenceLedger {
    /// Reads a bounded, fully verified page of transition records.
    pub fn transitions(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<RepairTransition>, RepairLedgerError> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(RepairLedgerError::InvalidPage);
        }
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let head = transition_head(&connection)?;
        let Some((head_sequence, head_hash)) = head else {
            return if after_sequence == 0 {
                Ok(Vec::new())
            } else {
                Err(RepairLedgerError::Integrity)
            };
        };
        let tail = load_transition_exact(&connection, head_sequence)?;
        if tail.transition_hash != head_hash || after_sequence > head_sequence {
            return Err(RepairLedgerError::Integrity);
        }
        let predecessor = if after_sequence == 0 {
            None
        } else {
            Some(load_transition_exact(&connection, after_sequence)?)
        };
        let mut statement = connection.prepare(
            "SELECT sequence, fingerprint, from_status, to_status, kind,
                    occurred_at_epoch_ms, previous_transition_hash, record_json,
                    transition_hash
             FROM repair_transitions WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let raw = statement
            .query_map(params![to_i64(after_sequence)?, i64::from(limit)], read_raw)?
            .collect::<Result<Vec<_>, _>>()?;
        let transitions = raw
            .into_iter()
            .map(decode_transition)
            .collect::<Result<Vec<_>, _>>()?;
        verify_transition_page(
            after_sequence,
            predecessor.as_ref(),
            head_sequence,
            limit,
            &transitions,
        )?;
        Ok(transitions)
    }

    /// Verifies `SQLite`, immutable records and receipts, the durable transition
    /// head, the complete global chain, and each group's full lifecycle replay.
    pub fn verify_integrity(&self) -> Result<RepairIntegrityReport, RepairLedgerError> {
        let _lease = self.gate.shared()?;
        let connection = self.lock()?;
        let mut report = RepairIntegrityReport::default();

        check_sqlite(&connection, &mut report)?;
        check_immutable_records(&connection, &mut report)?;
        check_operations(&connection, &mut report)?;

        let groups = load_groups_for_replay(&connection, &mut report)?;
        let transitions = load_all_transitions(&connection, &mut report)?;
        check_transition_head(&connection, &transitions, &mut report)?;
        replay_groups(&groups, &transitions, &mut report);
        check_batons(&connection, &mut report)?;
        super::core_events::check_integrity(&connection, &mut report.errors)?;
        Ok(report)
    }
}

fn check_sqlite(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<(), RepairLedgerError> {
    let mut integrity = connection.prepare("PRAGMA integrity_check")?;
    for row in integrity
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
    {
        if row != "ok" {
            report.errors.push(format!("sqlite:{row}"));
        }
    }
    let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
    for (table, rowid, parent, foreign_key) in foreign_keys
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
    {
        report.errors.push(format!(
            "foreign_key:{table}:{}:{parent}:{foreign_key}",
            rowid.map_or_else(|| "none".to_owned(), |value| value.to_string())
        ));
    }
    Ok(())
}

fn check_immutable_records(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<(), RepairLedgerError> {
    for table in [
        "error_groups",
        "error_occurrences",
        "diagnoses",
        "workarounds",
        "repair_candidates",
        "candidate_disclosures",
        "candidate_rejections",
        "restart_batons",
        "verification_starts",
        "verifications",
        "rollbacks",
        "regressions",
        "evidence_tombstones",
    ] {
        let sql = format!("SELECT rowid, record_json, record_hash FROM {table}");
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (rowid, json, hash) in rows {
            report.immutable_records_checked = report.immutable_records_checked.saturating_add(1);
            let parsed = serde_json::from_str::<serde_json::Value>(&json);
            let canonical = parsed
                .as_ref()
                .ok()
                .and_then(|value| canonical_json(value).ok());
            if validate_hash(&hash).is_err()
                || hash_bytes(json.as_bytes()) != hash
                || canonical.as_deref() != Some(json.as_str())
            {
                report.errors.push(format!("{table}:{rowid}:record"));
            }
        }
    }
    Ok(())
}

fn check_operations(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<(), RepairLedgerError> {
    let mut statement = connection.prepare(
        "SELECT operation_id, request_hash, response_json, response_hash
         FROM repair_operations ORDER BY operation_id",
    )?;
    for (operation_id, request_hash, response_json, response_hash) in statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
    {
        let parsed = serde_json::from_str::<serde_json::Value>(&response_json);
        let canonical = parsed
            .as_ref()
            .ok()
            .and_then(|value| canonical_json(value).ok());
        if validate_id(&operation_id).is_err()
            || validate_hash(&request_hash).is_err()
            || validate_hash(&response_hash).is_err()
            || hash_bytes(response_json.as_bytes()) != response_hash
            || canonical.as_deref() != Some(response_json.as_str())
        {
            report.errors.push(format!("operation:{operation_id}"));
        }
    }
    Ok(())
}

fn load_groups_for_replay(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<BTreeMap<String, ErrorGroup>, RepairLedgerError> {
    let mut statement = connection.prepare(
        "SELECT fingerprint, ledger_version, status, record_json, record_hash
         FROM error_groups ORDER BY fingerprint",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut groups = BTreeMap::new();
    for (fingerprint, version, status, json, hash) in rows {
        report.groups_checked = report.groups_checked.saturating_add(1);
        let decoded = decode_verified::<ErrorGroup>(&json, &hash);
        let valid = decoded.as_ref().is_ok_and(|group| {
            group.fingerprint == fingerprint
                && i64::try_from(group.ledger_version).ok() == Some(version)
                && group.status.as_str() == status
                && canonical_json(group).as_deref().ok() == Some(json.as_str())
        });
        if !valid {
            report.errors.push(format!("group:{fingerprint}:columns"));
        }
        if let Ok(group) = decoded {
            groups.insert(fingerprint, group);
        }
    }
    Ok(groups)
}

fn load_all_transitions(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<Vec<RepairTransition>, RepairLedgerError> {
    let mut statement = connection.prepare(
        "SELECT sequence, fingerprint, from_status, to_status, kind,
                occurred_at_epoch_ms, previous_transition_hash, record_json,
                transition_hash
         FROM repair_transitions ORDER BY sequence",
    )?;
    let rows = statement
        .query_map([], read_raw)?
        .collect::<Result<Vec<_>, _>>()?;
    let mut transitions = Vec::with_capacity(rows.len());
    let mut expected_sequence = 1_u64;
    let mut previous: Option<String> = None;
    for raw in rows {
        report.transitions_checked = report.transitions_checked.saturating_add(1);
        let sequence_label = raw.sequence;
        match decode_transition(raw) {
            Ok(transition)
                if transition.sequence == expected_sequence
                    && transition.previous_transition_hash == previous =>
            {
                previous = Some(transition.transition_hash.clone());
                transitions.push(transition);
            }
            Ok(transition) => {
                report
                    .errors
                    .push(format!("transition:{}:chain", transition.sequence));
                previous = Some(transition.transition_hash.clone());
                expected_sequence = transition.sequence;
                transitions.push(transition);
            }
            Err(_) => report
                .errors
                .push(format!("transition:{sequence_label}:record")),
        }
        expected_sequence = expected_sequence.saturating_add(1);
    }
    Ok(transitions)
}

fn check_transition_head(
    connection: &Connection,
    transitions: &[RepairTransition],
    report: &mut RepairIntegrityReport,
) -> Result<(), RepairLedgerError> {
    let head = transition_head(connection)?;
    let expected = transitions
        .last()
        .map(|transition| (transition.sequence, transition.transition_hash.as_str()));
    if head
        .as_ref()
        .map(|(sequence, hash)| (*sequence, hash.as_str()))
        != expected
    {
        report.errors.push("transition_head:mismatch".to_owned());
    }
    Ok(())
}

fn replay_groups(
    groups: &BTreeMap<String, ErrorGroup>,
    transitions: &[RepairTransition],
    report: &mut RepairIntegrityReport,
) {
    let mut replay = BTreeMap::<String, (u64, Option<ErrorGroupStatus>)>::new();
    for transition in transitions {
        let state = replay.entry(transition.fingerprint.clone()).or_default();
        if transition.from != state.1 {
            report.errors.push(format!(
                "group_replay:{}:{}:from",
                transition.fingerprint, transition.sequence
            ));
        }
        state.0 = state.0.saturating_add(1);
        state.1 = Some(transition.to);
    }
    for (fingerprint, group) in groups {
        match replay.get(fingerprint) {
            Some((count, Some(status)))
                if *count == group.ledger_version && *status == group.status => {}
            _ => report
                .errors
                .push(format!("group_replay:{fingerprint}:terminal")),
        }
    }
    for fingerprint in replay.keys() {
        if !groups.contains_key(fingerprint) {
            report
                .errors
                .push(format!("group_replay:{fingerprint}:missing_group"));
        }
    }
}

fn check_batons(
    connection: &Connection,
    report: &mut RepairIntegrityReport,
) -> Result<(), RepairLedgerError> {
    let mut batons = connection.prepare(
        "SELECT baton_id, candidate_id, candidate_version, record_json, record_hash
         FROM restart_batons ORDER BY baton_id",
    )?;
    for (baton_id, candidate_id, candidate_version, baton_json, baton_hash) in batons
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
    {
        let baton = decode_verified::<super::RestartBaton>(&baton_json, &baton_hash);
        let candidate: Option<(String, String)> = connection
            .query_row(
                "SELECT record_json, record_hash FROM repair_candidates
                 WHERE candidate_id=?1 AND candidate_version=?2",
                params![candidate_id, candidate_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let disclosure: Option<(String, String)> = connection
            .query_row(
                "SELECT record_json, record_hash FROM candidate_disclosures
                 WHERE candidate_id=?1 AND candidate_version=?2",
                params![candidate_id, candidate_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let valid = match (baton, candidate, disclosure) {
            (
                Ok(baton),
                Some((candidate_json, candidate_hash)),
                Some((disclosure_json, disclosure_hash)),
            ) => {
                let candidate =
                    decode_verified::<super::RepairCandidate>(&candidate_json, &candidate_hash);
                let disclosure = decode_verified::<super::CandidateDisclosure>(
                    &disclosure_json,
                    &disclosure_hash,
                );
                candidate.is_ok_and(|candidate| {
                    disclosure.is_ok_and(|disclosure| {
                        baton.candidate_hash == candidate.candidate_hash
                            && baton.rollback_point_id == candidate.rollback_point.rollback_point_id
                            && baton.previous_working_build_hash
                                == candidate.rollback_point.previous_working_build.content_hash
                            && baton.management_checkpoint_id == disclosure.management_checkpoint_id
                    })
                })
            }
            _ => false,
        };
        if !valid || validate_id(&baton_id).is_err() {
            report.errors.push(format!("baton:{baton_id}"));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RawTransition {
    sequence: i64,
    fingerprint: String,
    from: Option<String>,
    to: String,
    kind: String,
    occurred_at_epoch_ms: i64,
    previous_transition_hash: Option<String>,
    record_json: String,
    transition_hash: String,
}

fn read_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawTransition> {
    Ok(RawTransition {
        sequence: row.get(0)?,
        fingerprint: row.get(1)?,
        from: row.get(2)?,
        to: row.get(3)?,
        kind: row.get(4)?,
        occurred_at_epoch_ms: row.get(5)?,
        previous_transition_hash: row.get(6)?,
        record_json: row.get(7)?,
        transition_hash: row.get(8)?,
    })
}

fn decode_transition(raw: RawTransition) -> Result<RepairTransition, RepairLedgerError> {
    let sequence = from_i64(raw.sequence)?;
    let from = raw
        .from
        .as_deref()
        .map(ErrorGroupStatus::parse)
        .transpose()?;
    let to = ErrorGroupStatus::parse(&raw.to)?;
    let occurred_at_epoch_ms = from_i64(raw.occurred_at_epoch_ms)?;
    validate_id(&raw.fingerprint).map_err(|_| RepairLedgerError::Integrity)?;
    validate_id(&raw.kind).map_err(|_| RepairLedgerError::Integrity)?;
    validate_hash(&raw.transition_hash).map_err(|_| RepairLedgerError::Integrity)?;
    if raw
        .previous_transition_hash
        .as_deref()
        .is_some_and(|hash| validate_hash(hash).is_err())
    {
        return Err(RepairLedgerError::Integrity);
    }
    let expected_hash = canonical_hash(&(
        sequence,
        &raw.fingerprint,
        from,
        to,
        &raw.kind,
        occurred_at_epoch_ms,
        &raw.previous_transition_hash,
    ))?;
    if expected_hash != raw.transition_hash {
        return Err(RepairLedgerError::Integrity);
    }
    let parsed: RepairTransition = serde_json::from_str(&raw.record_json)?;
    let reconstructed = RepairTransition {
        sequence,
        fingerprint: raw.fingerprint,
        from,
        to,
        kind: raw.kind,
        occurred_at_epoch_ms,
        previous_transition_hash: raw.previous_transition_hash,
        transition_hash: raw.transition_hash,
    };
    if parsed != reconstructed || canonical_json(&parsed)? != raw.record_json {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(parsed)
}

fn transition_head(connection: &Connection) -> Result<Option<(u64, String)>, RepairLedgerError> {
    connection
        .query_row(
            "SELECT sequence, transition_hash FROM repair_transition_head WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(sequence, hash)| {
            validate_hash(&hash).map_err(|_| RepairLedgerError::Integrity)?;
            Ok((from_i64(sequence)?, hash))
        })
        .transpose()
}

fn load_transition_exact(
    connection: &Connection,
    sequence: u64,
) -> Result<RepairTransition, RepairLedgerError> {
    let raw = connection
        .query_row(
            "SELECT sequence, fingerprint, from_status, to_status, kind,
                    occurred_at_epoch_ms, previous_transition_hash, record_json,
                    transition_hash
             FROM repair_transitions WHERE sequence=?1",
            [to_i64(sequence)?],
            read_raw,
        )
        .optional()?
        .ok_or(RepairLedgerError::Integrity)?;
    decode_transition(raw)
}

fn verify_transition_page(
    after: u64,
    predecessor: Option<&RepairTransition>,
    head_sequence: u64,
    limit: u32,
    transitions: &[RepairTransition],
) -> Result<(), RepairLedgerError> {
    let mut expected_sequence = after
        .checked_add(1)
        .ok_or(RepairLedgerError::NumericOverflow)?;
    let mut previous = predecessor.map(|transition| transition.transition_hash.as_str());
    for transition in transitions {
        if transition.sequence != expected_sequence
            || transition.previous_transition_hash.as_deref() != previous
        {
            return Err(RepairLedgerError::Integrity);
        }
        previous = Some(&transition.transition_hash);
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(RepairLedgerError::NumericOverflow)?;
    }
    let consumed = after
        .checked_add(
            u64::try_from(transitions.len()).map_err(|_| RepairLedgerError::NumericOverflow)?,
        )
        .ok_or(RepairLedgerError::NumericOverflow)?;
    if transitions.len() < usize::try_from(limit).map_err(|_| RepairLedgerError::NumericOverflow)?
        && consumed != head_sequence
    {
        return Err(RepairLedgerError::Integrity);
    }
    Ok(())
}
