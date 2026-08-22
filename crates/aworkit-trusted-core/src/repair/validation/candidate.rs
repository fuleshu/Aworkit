//! Candidate, evidence, authority-subset, and investigation validation.

use std::collections::BTreeSet;

use aworkit_protocol::{StableId, is_canonical_sha256};

use crate::AuthorityManifestV1;

use super::{
    super::{
        AuthenticatedInvestigationExecutionReceiptV1, BuildBundleRefV1, BuildProvenanceV1,
        DataCompatibilityV1, DisclosureItemsV1, ErrorOccurrenceV1, FocusedVerificationPlanV1,
        FrozenRepairAuthorityV1, REPAIR_SCHEMA_VERSION_V1, RepairArtifactRefV1, RepairCandidateV1,
        RepairDisclosureV1, RepairEvidenceDisclosureV1, RepairInvestigationV1,
    },
    hashing::{
        authority_manifest_hash_v1, build_provenance_hash_v1, focused_verification_plan_hash_v1,
        investigation_execution_receipt_hash_v1, repair_candidate_hash_v1,
        repair_disclosure_hash_v1,
    },
};

pub(crate) const MAX_SUMMARY_BYTES: usize = 8 * 1024;
pub(crate) const MAX_SHORT_TEXT_BYTES: usize = 512;
const MAX_DISCLOSURE_ITEMS: usize = 128;
pub(crate) const MAX_EVIDENCE_ARTIFACTS: usize = 256;
pub(crate) const MAX_VERIFICATION_CHECKS: usize = 64;
pub(crate) const MAX_PHASE_DEADLINE_MS: u64 = 60 * 60 * 1_000;
/// Hard ceiling for one user-approved bounded repair investigation.
pub const MAX_REPAIR_INVESTIGATION_TOKENS_V1: u64 = 1_000_000;

pub(crate) fn validate_occurrence(value: &ErrorOccurrenceV1) -> Result<(), &'static str> {
    if !is_canonical_sha256(&value.fingerprint)
        || !valid_text(&value.summary, MAX_SUMMARY_BYTES)
        || value.observed_at_epoch_ms == 0
        || value.evidence.len() > MAX_EVIDENCE_ARTIFACTS
        || value
            .evidence
            .iter()
            .any(|artifact| validate_artifact(artifact).is_err())
    {
        Err("invalid recurring-error occurrence")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_investigation(value: &RepairInvestigationV1) -> Result<(), &'static str> {
    let budget = &value.budget;
    if budget.max_attempts == 0
        || budget.max_attempts > 10_000
        || budget.max_tool_calls == 0
        || budget.max_tool_calls > 100_000
        || budget.max_tokens == 0
        || budget.max_tokens > MAX_REPAIR_INVESTIGATION_TOKENS_V1
        || budget.deadline_ms == 0
        || budget.deadline_ms > MAX_PHASE_DEADLINE_MS
        || value.authority.capability_ids.is_empty()
        || value.authority.capability_ids.len() > 256
        || !is_legacy_authority_hash(&value.authority.authority_manifest_hash)
        || contains_duplicate_ids(&value.authority.capability_ids)
    {
        Err("invalid bounded investigation")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_authenticated_investigation_execution(
    value: &AuthenticatedInvestigationExecutionReceiptV1,
    investigation: &RepairInvestigationV1,
    candidate: &RepairCandidateV1,
) -> Result<(), &'static str> {
    let receipt = &value.receipt;
    let canonical_frozen = receipt
        .frozen_capability_ids
        .windows(2)
        .all(|pair| pair[0] < pair[1]);
    let canonical_executed = receipt
        .executed_capability_ids
        .windows(2)
        .all(|pair| pair[0] < pair[1]);
    let usage = &receipt.observed_usage;
    if receipt.schema_version != REPAIR_SCHEMA_VERSION_V1
        || receipt.candidate_version == 0
        || receipt.completed_at_epoch_ms == 0
        || !is_canonical_sha256(&receipt.candidate_hash)
        || !is_legacy_authority_hash(&receipt.authority_manifest_hash)
        || receipt.frozen_capability_ids.is_empty()
        || receipt.executed_capability_ids.is_empty()
        || usage.attempts == 0
        || usage.attempts > receipt.frozen_budget.max_attempts
        || usage.tool_calls < receipt.executed_capability_ids.len() as u32
        || usage.tool_calls > receipt.frozen_budget.max_tool_calls
        || usage.tokens > receipt.frozen_budget.max_tokens
        || usage.elapsed_ms == 0
        || usage.elapsed_ms > receipt.frozen_budget.deadline_ms
        || contains_duplicate_ids(&receipt.frozen_capability_ids)
        || contains_duplicate_ids(&receipt.executed_capability_ids)
        || !canonical_frozen
        || !canonical_executed
        || investigation_execution_receipt_hash_v1(receipt)
            .map_err(|_| "invalid investigation execution receipt hash")?
            != receipt.receipt_hash
        || !value.peer.same_user_authenticated
        || !is_canonical_sha256(&value.peer.ownership_hash)
        || !is_canonical_sha256(&value.peer.channel_binding_hash)
    {
        return Err("invalid authenticated investigation execution receipt");
    }
    if receipt.investigation_id != investigation.investigation_id
        || receipt.group_id != investigation.group_id
        || receipt.management_chat_id != investigation.management_chat_id
        || receipt.management_run_id != investigation.management_run_id
        || receipt.candidate_id != candidate.candidate_id
        || receipt.candidate_version != candidate.candidate_version
        || receipt.candidate_hash != candidate.candidate_hash
        || receipt.authority_manifest_id != investigation.authority.authority_manifest_id
        || receipt.authority_manifest_hash != investigation.authority.authority_manifest_hash
        || receipt.frozen_capability_ids != investigation.authority.capability_ids
        || receipt.executed_capability_ids != investigation.authority.capability_ids
        || receipt.frozen_budget != investigation.budget
        || candidate
            .disclosure
            .verification_plan
            .checks
            .iter()
            .any(|check| {
                !receipt
                    .executed_capability_ids
                    .contains(&check.capability_id)
            })
    {
        Err("investigation execution receipt does not exactly bind candidate authority")
    } else {
        Ok(())
    }
}

pub(crate) fn freeze_investigation_authority(
    manifest: &AuthorityManifestV1,
    requested: &[StableId],
) -> Result<FrozenRepairAuthorityV1, &'static str> {
    if requested.is_empty() || requested.len() > 256 || contains_duplicate_ids(requested) {
        return Err("invalid requested investigation authority");
    }
    let computed = authority_manifest_hash_v1(&manifest.capability_bindings)
        .map_err(|_| "invalid frozen authority manifest")?;
    let expected_id = format!("manifest.{}", &computed[..32]);
    if computed != manifest.manifest_hash
        || manifest.manifest_id.as_str() != expected_id
        || requested.iter().any(|requested_id| {
            !manifest.capability_bindings.iter().any(|binding| {
                binding.capability_id == *requested_id && binding.enabled && binding.compatible
            })
        })
    {
        return Err("investigation authority exceeds or corrupts the frozen manifest");
    }
    let mut capability_ids = requested.to_vec();
    capability_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(FrozenRepairAuthorityV1 {
        authority_manifest_id: manifest.manifest_id.clone(),
        authority_manifest_hash: manifest.manifest_hash.clone(),
        capability_ids,
    })
}

pub(crate) fn validate_candidate(value: &RepairCandidateV1) -> Result<(), &'static str> {
    if value.candidate_version == 0
        || !valid_text(&value.summary, MAX_SUMMARY_BYTES)
        || validate_build_bundle(&value.build_bundle).is_err()
        || validate_provenance(&value.provenance).is_err()
        || !is_legacy_authority_hash(&value.built_under_authority_manifest_hash)
        || validate_disclosure(&value.disclosure).is_err()
        || repair_candidate_hash_v1(value).map_err(|_| "invalid candidate hash")?
            != value.candidate_hash
    {
        Err("invalid or incomplete repair candidate")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_artifact(value: &RepairArtifactRefV1) -> Result<(), &'static str> {
    if !is_canonical_sha256(&value.content_hash)
        || value.byte_size == 0
        || !valid_text(&value.media_type, 128)
        || !valid_text(&value.logical_name, MAX_SHORT_TEXT_BYTES)
    {
        Err("invalid repair evidence artifact")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_build_bundle(value: &BuildBundleRefV1) -> Result<(), &'static str> {
    if validate_artifact(&value.artifact).is_err()
        || !valid_relative_entry(&value.manifest_relative_entry)
    {
        Err("invalid whole-build bundle reference")
    } else {
        Ok(())
    }
}

fn validate_provenance(value: &BuildProvenanceV1) -> Result<(), &'static str> {
    if !valid_text(&value.source_revision, MAX_SHORT_TEXT_BYTES)
        || !is_canonical_sha256(&value.source_tree_hash)
        || !is_canonical_sha256(&value.workspace_identity_hash)
        || !is_canonical_sha256(&value.toolchain_hash)
        || !is_canonical_sha256(&value.build_manifest_hash)
        || build_provenance_hash_v1(value).map_err(|_| "invalid provenance hash")?
            != value.provenance_hash
    {
        Err("invalid build provenance")
    } else {
        Ok(())
    }
}

fn validate_disclosure(value: &RepairDisclosureV1) -> Result<(), &'static str> {
    if validate_evidence_disclosure(&value.source_diff, true).is_err()
        || validate_evidence_disclosure(&value.configuration_diff, false).is_err()
        || validate_evidence_disclosure(&value.tests, true).is_err()
        || validate_evidence_disclosure(&value.benchmarks, false).is_err()
        || validate_disclosure_items(&value.consequences).is_err()
        || validate_disclosure_items(&value.removed_behaviors).is_err()
        || validate_disclosure_items(&value.disabled_behaviors).is_err()
        || validate_disclosure_items(&value.broadened_behaviors).is_err()
        || validate_disclosure_items(&value.replaced_behaviors).is_err()
        || validate_disclosure_items(&value.uncertainties).is_err()
        || matches!(
            &value.data_compatibility,
            DataCompatibilityV1::DeferredUntilVerified { explanation }
                | DataCompatibilityV1::ForwardOnlyMigrationRequired { explanation }
                if !valid_text(explanation, MAX_SUMMARY_BYTES)
        )
        || validate_build_bundle(&value.rollback_point).is_err()
        || validate_focused_verification_plan(&value.verification_plan).is_err()
        || repair_disclosure_hash_v1(value).map_err(|_| "invalid disclosure hash")?
            != value.disclosure_hash
    {
        Err("incomplete candidate disclosure")
    } else {
        Ok(())
    }
}

fn validate_evidence_disclosure(
    value: &RepairEvidenceDisclosureV1,
    evidence_required: bool,
) -> Result<(), &'static str> {
    match value {
        RepairEvidenceDisclosureV1::Evidence { summary, artifacts }
            if valid_text(summary, MAX_SUMMARY_BYTES)
                && !artifacts.is_empty()
                && artifacts.len() <= MAX_EVIDENCE_ARTIFACTS
                && artifacts
                    .iter()
                    .all(|artifact| validate_artifact(artifact).is_ok()) =>
        {
            Ok(())
        }
        RepairEvidenceDisclosureV1::NoneDeclared { explanation }
        | RepairEvidenceDisclosureV1::NotPerformed { explanation }
            if !evidence_required && valid_text(explanation, MAX_SUMMARY_BYTES) =>
        {
            Ok(())
        }
        _ => Err("required evidence disclosure is incomplete"),
    }
}

fn validate_disclosure_items(value: &DisclosureItemsV1) -> Result<(), &'static str> {
    if value.items.len() > MAX_DISCLOSURE_ITEMS
        || value.none_declared == !value.items.is_empty()
        || contains_duplicate_ids(
            &value
                .items
                .iter()
                .map(|item| item.item_id.clone())
                .collect::<Vec<_>>(),
        )
        || value.items.iter().any(|item| {
            !valid_text(&item.label, MAX_SHORT_TEXT_BYTES)
                || !valid_text(&item.detail, MAX_SUMMARY_BYTES)
        })
    {
        Err("disclosure list must contain unique detailed items or explicitly declare none")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_focused_verification_plan(
    value: &FocusedVerificationPlanV1,
) -> Result<(), &'static str> {
    if value.checks.is_empty()
        || value.checks.len() > MAX_VERIFICATION_CHECKS
        || contains_duplicate_ids(
            &value
                .checks
                .iter()
                .map(|check| check.check_id.clone())
                .collect::<Vec<_>>(),
        )
        || value.checks.iter().any(|check| {
            !valid_text(&check.label, MAX_SHORT_TEXT_BYTES)
                || check.timeout_ms == 0
                || check.timeout_ms > MAX_PHASE_DEADLINE_MS
        })
        || focused_verification_plan_hash_v1(value).map_err(|_| "invalid verification plan hash")?
            != value.plan_hash
    {
        Err("invalid focused-verification plan")
    } else {
        Ok(())
    }
}

fn valid_relative_entry(value: &str) -> bool {
    valid_text(value, 1_024)
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && !value.contains(':')
        && value
            .split(['/', '\\'])
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

pub(crate) fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(|character| {
            character == '\0' || (character.is_control() && character != '\n' && character != '\t')
        })
}

pub(crate) fn contains_duplicate_ids(values: &[StableId]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value.as_str()))
}

fn is_legacy_authority_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
