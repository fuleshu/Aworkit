//! Bounded identifiers, evidence hashes, and secret-material rejection.

use serde::Serialize;

use super::{
    DiagnosisRecord, ErrorOccurrence, EvidenceAvailability, EvidenceReference, EvidenceTombstone,
    RepairLedgerError, WorkaroundRecord,
};
use crate::RedactionSet;

const MAX_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_EVIDENCE_REFS: usize = 128;
pub(super) const MAX_PAGE: u32 = 512;

pub(super) fn validate_id(value: &str) -> Result<(), RepairLedgerError> {
    if value.is_empty()
        || value.len() > 192
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(RepairLedgerError::InvalidId);
    }
    Ok(())
}

pub(super) fn validate_hash(value: &str) -> Result<(), RepairLedgerError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(RepairLedgerError::InvalidHash);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(RepairLedgerError::InvalidHash);
    }
    Ok(())
}

pub(super) fn validate_text(value: &str) -> Result<(), RepairLedgerError> {
    if value.is_empty() || value.len() > MAX_SUMMARY_BYTES || value.contains('\0') {
        return Err(RepairLedgerError::InvalidRecord);
    }
    let lower = value.to_ascii_lowercase();
    if [
        "authorization:",
        "proxy-authorization:",
        "password=",
        "api_key=",
        "access_token=",
        "refresh_token=",
        "secret_lease=",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err(RepairLedgerError::ForbiddenSecretMaterial);
    }
    Ok(())
}

pub(super) fn validate_evidence(reference: &EvidenceReference) -> Result<(), RepairLedgerError> {
    validate_id(&reference.artifact_id)?;
    validate_hash(&reference.content_hash)
}

pub(super) fn validate_occurrence(occurrence: &ErrorOccurrence) -> Result<(), RepairLedgerError> {
    validate_id(&occurrence.occurrence_id)?;
    validate_id(&occurrence.fingerprint)?;
    validate_text(&occurrence.summary)?;
    for id in [
        occurrence.semantic_event_id.as_deref(),
        occurrence.attempt_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_id(id)?;
    }
    if occurrence.diagnostics.len() > MAX_EVIDENCE_REFS
        || occurrence.evidence.len() > MAX_EVIDENCE_REFS
    {
        return Err(RepairLedgerError::InvalidRecord);
    }
    for diagnostic in &occurrence.diagnostics {
        validate_id(&diagnostic.diagnostic_record_id)?;
    }
    occurrence.evidence.iter().try_for_each(validate_evidence)
}

pub(super) fn validate_diagnosis(record: &DiagnosisRecord) -> Result<(), RepairLedgerError> {
    validate_id(&record.diagnosis_id)?;
    validate_id(&record.fingerprint)?;
    validate_text(&record.summary)?;
    if record.evidence.len() > MAX_EVIDENCE_REFS {
        return Err(RepairLedgerError::InvalidRecord);
    }
    record.evidence.iter().try_for_each(validate_evidence)
}

pub(super) fn validate_workaround(record: &WorkaroundRecord) -> Result<(), RepairLedgerError> {
    validate_id(&record.workaround_id)?;
    validate_id(&record.fingerprint)?;
    validate_text(&record.summary)?;
    validate_text(&record.consequence_summary)?;
    if record.evidence.len() > MAX_EVIDENCE_REFS {
        return Err(RepairLedgerError::InvalidRecord);
    }
    record.evidence.iter().try_for_each(validate_evidence)
}

pub(super) fn validate_tombstone(tombstone: &EvidenceTombstone) -> Result<(), RepairLedgerError> {
    validate_id(&tombstone.tombstone_id)?;
    validate_id(&tombstone.artifact_id)?;
    validate_hash(&tombstone.content_hash)?;
    validate_text(&tombstone.reason)?;
    if tombstone.availability == EvidenceAvailability::Available {
        return Err(RepairLedgerError::InvalidRecord);
    }
    Ok(())
}

pub(super) fn validate_page(limit: u32) -> Result<(), RepairLedgerError> {
    if limit == 0 || limit > MAX_PAGE {
        Err(RepairLedgerError::InvalidPage)
    } else {
        Ok(())
    }
}

pub(super) fn validate_redacted<T: Serialize>(
    value: &T,
    redaction: &RedactionSet,
) -> Result<(), RepairLedgerError> {
    let canonical = serde_jcs::to_vec(value).map_err(|_| RepairLedgerError::InvalidRecord)?;
    let inspected = redaction
        .redact_payload(&canonical)
        .map_err(|_| RepairLedgerError::ForbiddenSecretMaterial)?;
    if inspected.replacements() != 0 || inspected.as_bytes() != canonical {
        return Err(RepairLedgerError::ForbiddenSecretMaterial);
    }
    Ok(())
}
