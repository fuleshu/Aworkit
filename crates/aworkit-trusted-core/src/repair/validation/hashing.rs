//! Canonical hashes for self-describing repair and bootstrap records.

use aworkit_protocol::{StableId, is_canonical_sha256};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::super::{
    BootstrapAcceptedAdmissionV1, BootstrapResultV1, BuildProvenanceV1, CoreQuiescenceFactsV1,
    FocusedVerificationEvidenceV1, FocusedVerificationPlanV1, InvestigationExecutionReceiptV1,
    PlatformCapabilityReportV1, RepairActivationBatonV1, RepairCandidateV1, RepairDisclosureV1,
};

/// Canonical encoding is the only fallible part of the public hash helpers.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RepairValidationError {
    #[error("repair record could not be encoded canonically")]
    Encoding,
    #[error("repair fingerprint is not a canonical SHA-256 identity")]
    InvalidFingerprint,
}

/// Resolves the sole stable ledger group for a normalized fingerprint.
pub fn repair_group_id_for_fingerprint_v1(
    fingerprint: &str,
) -> Result<StableId, RepairValidationError> {
    if !is_canonical_sha256(fingerprint) {
        return Err(RepairValidationError::InvalidFingerprint);
    }
    StableId::parse(format!("repair.group.{}", &fingerprint[7..]))
        .map_err(|_| RepairValidationError::InvalidFingerprint)
}

pub(crate) fn canonical_hash<T: Serialize>(value: &T) -> Result<String, RepairValidationError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| RepairValidationError::Encoding)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Authority manifests predate the protocol-wide `sha256:` identity format.
/// Preserve their existing raw digest representation at this compatibility seam.
pub(crate) fn authority_manifest_hash_v1<T: Serialize>(
    value: &T,
) -> Result<String, RepairValidationError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| RepairValidationError::Encoding)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Computes the provenance hash without its self-referential field.
pub fn build_provenance_hash_v1(
    provenance: &BuildProvenanceV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        &provenance.source_revision,
        &provenance.source_tree_hash,
        &provenance.workspace_identity_hash,
        &provenance.toolchain_hash,
        &provenance.build_manifest_hash,
    ))
}

pub fn focused_verification_plan_hash_v1(
    plan: &FocusedVerificationPlanV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(&plan.plan_id, &plan.checks))
}

pub fn focused_verification_evidence_hash_v1(
    evidence: &FocusedVerificationEvidenceV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(&evidence.plan_id, &evidence.plan_hash, &evidence.results))
}

pub fn repair_disclosure_hash_v1(
    disclosure: &RepairDisclosureV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        &disclosure.source_diff,
        &disclosure.configuration_diff,
        &disclosure.tests,
        &disclosure.benchmarks,
        &disclosure.consequences,
        &disclosure.removed_behaviors,
        &disclosure.disabled_behaviors,
        &disclosure.broadened_behaviors,
        &disclosure.replaced_behaviors,
        &disclosure.uncertainties,
        &disclosure.data_compatibility,
        &disclosure.rollback_point,
        &disclosure.verification_plan,
    ))
}

pub fn repair_candidate_hash_v1(
    candidate: &RepairCandidateV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        &candidate.candidate_id,
        &candidate.group_id,
        candidate.candidate_version,
        &candidate.summary,
        &candidate.build_bundle,
        &candidate.provenance,
        &candidate.built_under_authority_manifest_hash,
        &candidate.disclosure,
    ))
}

/// Seals the exact Run, authority, execution set, budget/usage, and candidate.
pub fn investigation_execution_receipt_hash_v1(
    receipt: &InvestigationExecutionReceiptV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        receipt.schema_version,
        &receipt.receipt_id,
        &receipt.investigation_id,
        &receipt.group_id,
        &receipt.management_chat_id,
        &receipt.management_run_id,
        &receipt.candidate_id,
        receipt.candidate_version,
        &receipt.candidate_hash,
        &receipt.authority_manifest_id,
        &receipt.authority_manifest_hash,
        &receipt.frozen_capability_ids,
        &receipt.executed_capability_ids,
        &receipt.frozen_budget,
        &receipt.observed_usage,
        receipt.completed_at_epoch_ms,
    ))
}

pub fn capability_report_digest_v1(
    report: &PlatformCapabilityReportV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        report.schema_version,
        &report.report_id,
        &report.candidate_id,
        report.candidate_version,
        &report.candidate_hash,
        report.capability_generation,
        &report.build_origin,
        report.eligibility,
        &report.reason,
        &report.current_build,
        &report.previous_working_build,
        report.valid_from_epoch_ms,
        report.expires_at_epoch_ms,
    ))
}

pub fn repair_activation_baton_hash_v1(
    baton: &RepairActivationBatonV1,
) -> Result<String, RepairValidationError> {
    let mut canonical = baton.clone();
    canonical.baton_hash.clear();
    canonical_hash(&canonical)
}

pub fn bootstrap_admission_hash_v1(
    admission: &BootstrapAcceptedAdmissionV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        &admission.admission_id,
        &admission.activation_id,
        &admission.baton_hash,
        admission.candidate_process_generation,
        admission.rollback_process_generation,
    ))
}

pub fn core_quiescence_facts_hash_v1(
    facts: &CoreQuiescenceFactsV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        &facts.quiescence_id,
        &facts.activation_id,
        facts.process_generation,
        facts.worker_trees_stopped,
        facts.host_trees_stopped,
        facts.sidecar_trees_stopped,
        facts.timed_out,
        facts.orphan_risk,
    ))
}

pub fn bootstrap_result_hash_v1(
    result: &BootstrapResultV1,
) -> Result<String, RepairValidationError> {
    canonical_hash(&(
        result.schema_version,
        &result.receipt_id,
        &result.activation_id,
        &result.baton_hash,
        &result.management_checkpoint_id,
        result.recipient_process_generation,
        result.sealed_at_epoch_ms,
        &result.result,
    ))
}
