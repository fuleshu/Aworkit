//! Activation eligibility, baton, quiescence, and receipt validation.

use aworkit_protocol::is_canonical_sha256;

use super::{
    super::{
        ActivationEligibilityV1, AuthenticatedBootstrapResultV1, BootstrapAcceptedAdmissionV1,
        BootstrapDeadlinesV1, BootstrapResultKindV1, BootstrapResultV1, BuildOriginV1,
        CoreQuiescenceFactsV1, DataCompatibilityV1, EnrollmentPreparedV1,
        FocusedVerificationEvidenceV1, FocusedVerificationPlanV1, ManagedLocalEnrollmentRequestV1,
        ManagementCheckpointRefV1, PlatformCapabilityReportV1, PlatformReasonV1,
        REPAIR_SCHEMA_VERSION_V1, RepairActivationBatonV1, RepairCandidateDecisionV1,
    },
    candidate::{
        MAX_EVIDENCE_ARTIFACTS, MAX_PHASE_DEADLINE_MS, MAX_SUMMARY_BYTES, MAX_VERIFICATION_CHECKS,
        contains_duplicate_ids, valid_text, validate_artifact, validate_build_bundle,
        validate_focused_verification_plan,
    },
    hashing::{
        bootstrap_admission_hash_v1, bootstrap_result_hash_v1, capability_report_digest_v1,
        core_quiescence_facts_hash_v1, focused_verification_evidence_hash_v1,
        repair_activation_baton_hash_v1,
    },
};

const MAX_CAPABILITY_REPORT_VALIDITY_MS: u64 = 15 * 60 * 1_000;
const MAX_TOTAL_ACTIVATION_MS: u64 = 4 * 60 * 60 * 1_000;

pub(crate) fn validate_capability_report_shape(
    value: &PlatformCapabilityReportV1,
) -> Result<(), &'static str> {
    let valid_window = value.valid_from_epoch_ms > 0
        && value.expires_at_epoch_ms > value.valid_from_epoch_ms
        && value.expires_at_epoch_ms - value.valid_from_epoch_ms
            <= MAX_CAPABILITY_REPORT_VALIDITY_MS;
    let origin_matches = matches!(
        (&value.build_origin, value.eligibility),
        (
            BuildOriginV1::ManagedLocal { .. },
            ActivationEligibilityV1::SupportedManagedLocal
        ) | (
            BuildOriginV1::SourceCheckout { .. },
            ActivationEligibilityV1::EnrollmentRequired
        ) | (
            BuildOriginV1::PackagedDistribution { .. },
            ActivationEligibilityV1::PackagedDistribution
        ) | (
            BuildOriginV1::Unknown,
            ActivationEligibilityV1::UnknownOrigin
        ) | (
            BuildOriginV1::Conflicting { .. },
            ActivationEligibilityV1::ConflictingOrigin
        ) | (
            BuildOriginV1::Mismatched { .. },
            ActivationEligibilityV1::MismatchedEnrollment
        )
    ) || value.eligibility == ActivationEligibilityV1::Unsupported;
    let previous_valid = value
        .previous_working_build
        .as_ref()
        .is_none_or(|build| validate_build_bundle(build).is_ok());
    if value.schema_version != REPAIR_SCHEMA_VERSION_V1
        || value.candidate_version == 0
        || value.capability_generation == 0
        || !is_canonical_sha256(&value.candidate_hash)
        || !valid_platform_reason(&value.reason)
        || !valid_build_origin(&value.build_origin)
        || validate_build_bundle(&value.current_build).is_err()
        || !previous_valid
        || (value.eligibility == ActivationEligibilityV1::SupportedManagedLocal
            && value.previous_working_build.is_none())
        || !valid_window
        || !origin_matches
        || capability_report_digest_v1(value).map_err(|_| "invalid capability report digest")?
            != value.capability_digest
    {
        Err("invalid activation capability report")
    } else {
        Ok(())
    }
}

fn valid_build_origin(value: &BuildOriginV1) -> bool {
    match value {
        BuildOriginV1::ManagedLocal {
            enrollment_digest,
            active_slot_hash,
        } => is_canonical_sha256(enrollment_digest) && is_canonical_sha256(active_slot_hash),
        BuildOriginV1::SourceCheckout {
            projected_provenance_hash,
        } => is_canonical_sha256(projected_provenance_hash),
        BuildOriginV1::PackagedDistribution { owner } => valid_text(owner, MAX_SUMMARY_BYTES),
        BuildOriginV1::Unknown => true,
        BuildOriginV1::Conflicting { detail } | BuildOriginV1::Mismatched { detail } => {
            valid_text(detail, MAX_SUMMARY_BYTES)
        }
    }
}

pub(crate) fn validate_capability_report_fresh(
    value: &PlatformCapabilityReportV1,
    now_epoch_ms: u64,
) -> Result<(), &'static str> {
    validate_capability_report_shape(value)?;
    if now_epoch_ms < value.valid_from_epoch_ms || now_epoch_ms > value.expires_at_epoch_ms {
        Err("activation capability report is stale")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_enrollment_request(
    value: &ManagedLocalEnrollmentRequestV1,
) -> Result<(), &'static str> {
    if value.candidate_version == 0
        || !is_canonical_sha256(&value.candidate_hash)
        || !is_canonical_sha256(&value.projected_provenance_hash)
        || !is_canonical_sha256(&value.capability_digest)
        || validate_build_bundle(&value.whole_bundle).is_err()
    {
        Err("invalid managed-local enrollment request")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_enrollment_prepared(
    value: &EnrollmentPreparedV1,
) -> Result<(), &'static str> {
    if !is_canonical_sha256(&value.enrollment_digest)
        || !valid_text(&value.stable_launcher, MAX_SUMMARY_BYTES)
        || value.restart_instructions.is_empty()
        || value.restart_instructions.len() > 32
        || value
            .restart_instructions
            .iter()
            .any(|step| !valid_text(step, MAX_SUMMARY_BYTES))
    {
        Err("invalid enrollment preparation response")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_checkpoint(value: &ManagementCheckpointRefV1) -> Result<(), &'static str> {
    if value.committed_sequence == 0
        || !is_canonical_sha256(&value.snapshot_hash)
        || !is_canonical_sha256(&value.checkpoint_hash)
    {
        Err("invalid Management checkpoint")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_candidate_decision(
    value: &RepairCandidateDecisionV1,
) -> Result<(), &'static str> {
    if value.candidate_version == 0 || !valid_text(&value.reason, MAX_SUMMARY_BYTES) {
        Err("invalid candidate decision")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_repair_activation_baton(
    value: &RepairActivationBatonV1,
) -> Result<(), &'static str> {
    let next_generation = value.current_process_generation.0.checked_add(1);
    let rollback_generation = value.current_process_generation.0.checked_add(2);
    if value.schema_version != REPAIR_SCHEMA_VERSION_V1
        || value.candidate_version == 0
        || !is_canonical_sha256(&value.candidate_hash)
        || validate_build_bundle(&value.candidate_bundle).is_err()
        || !is_canonical_sha256(&value.disclosure_hash)
        || !is_canonical_sha256(&value.provenance_hash)
        || !is_canonical_sha256(&value.enrollment_digest)
        || value.capability_generation == 0
        || !is_canonical_sha256(&value.capability_digest)
        || validate_build_bundle(&value.previous_working_build).is_err()
        || validate_checkpoint(&value.management_checkpoint).is_err()
        || validate_focused_verification_plan(&value.verification_plan).is_err()
        || value.current_process_generation.0 == 0
        || next_generation != Some(value.candidate_process_generation.0)
        || rollback_generation != Some(value.rollback_process_generation.0)
        || validate_bootstrap_deadlines(&value.deadlines).is_err()
        || value.expires_at_epoch_ms == 0
        || repair_activation_baton_hash_v1(value).map_err(|_| "invalid baton hash")?
            != value.baton_hash
    {
        Err("invalid repair activation baton")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_admission(value: &BootstrapAcceptedAdmissionV1) -> Result<(), &'static str> {
    if !is_canonical_sha256(&value.baton_hash)
        || value.candidate_process_generation.0 == 0
        || value.rollback_process_generation.0 == 0
        || value.candidate_process_generation == value.rollback_process_generation
        || bootstrap_admission_hash_v1(value).map_err(|_| "invalid admission hash")?
            != value.admission_hash
    {
        Err("invalid bootstrap admission")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_quiescence_facts(value: &CoreQuiescenceFactsV1) -> Result<(), &'static str> {
    if value.process_generation.0 == 0
        || core_quiescence_facts_hash_v1(value).map_err(|_| "invalid quiescence facts hash")?
            != value.facts_hash
    {
        Err("invalid current-generation quiescence facts")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_focused_verification_evidence(
    value: &FocusedVerificationEvidenceV1,
) -> Result<(), &'static str> {
    if !is_canonical_sha256(&value.plan_hash)
        || value.results.is_empty()
        || value.results.len() > MAX_VERIFICATION_CHECKS
        || contains_duplicate_ids(
            &value
                .results
                .iter()
                .map(|result| result.check_id.clone())
                .collect::<Vec<_>>(),
        )
        || value.results.iter().any(|result| {
            !valid_text(&result.summary, MAX_SUMMARY_BYTES)
                || result.evidence.is_empty()
                || result.evidence.len() > MAX_EVIDENCE_ARTIFACTS
                || result
                    .evidence
                    .iter()
                    .any(|artifact| validate_artifact(artifact).is_err())
        })
        || focused_verification_evidence_hash_v1(value)
            .map_err(|_| "invalid verification evidence hash")?
            != value.evidence_hash
    {
        Err("invalid focused-verification evidence")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_focused_evidence_against_plan(
    evidence: &FocusedVerificationEvidenceV1,
    plan: &FocusedVerificationPlanV1,
) -> Result<(), &'static str> {
    validate_focused_verification_evidence(evidence)?;
    if evidence.plan_id != plan.plan_id
        || evidence.plan_hash != plan.plan_hash
        || evidence.results.len() != plan.checks.len()
        || plan.checks.iter().any(|check| {
            !evidence
                .results
                .iter()
                .any(|result| result.check_id == check.check_id)
        })
    {
        Err("verification evidence does not exactly cover the sealed plan")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_authenticated_result(
    value: &AuthenticatedBootstrapResultV1,
    baton: &RepairActivationBatonV1,
    admission: Option<&BootstrapAcceptedAdmissionV1>,
) -> Result<(), &'static str> {
    validate_bootstrap_result(&value.receipt)?;
    if !value.peer.same_user_authenticated
        || value.peer.recipient_process_generation != value.receipt.recipient_process_generation
        || !is_canonical_sha256(&value.peer.ownership_hash)
        || !is_canonical_sha256(&value.peer.channel_binding_hash)
        || value.receipt.activation_id != baton.activation_id
        || value.receipt.baton_hash != baton.baton_hash
        || value.receipt.management_checkpoint_id != baton.management_checkpoint.checkpoint_id
        || value.receipt.sealed_at_epoch_ms > baton.expires_at_epoch_ms
    {
        return Err("bootstrap result authentication or baton binding failed");
    }
    match (&value.receipt.result, admission) {
        (BootstrapResultKindV1::Unsupported { .. }, None)
            if value.receipt.recipient_process_generation == baton.current_process_generation =>
        {
            Ok(())
        }
        (
            BootstrapResultKindV1::ActivatedVerified {
                focused_verification,
            },
            Some(admission),
        ) if value.receipt.recipient_process_generation
            == admission.candidate_process_generation =>
        {
            validate_focused_evidence_against_plan(focused_verification, &baton.verification_plan)?;
            if focused_verification
                .results
                .iter()
                .all(|result| result.passed)
            {
                Ok(())
            } else {
                Err("ActivatedVerified receipt contains a failed focused check")
            }
        }
        (
            BootstrapResultKindV1::RolledBack { .. }
            | BootstrapResultKindV1::ManualRecoveryRequired { .. },
            Some(admission),
        ) if value.receipt.recipient_process_generation
            == admission.rollback_process_generation =>
        {
            Ok(())
        }
        _ => Err("bootstrap result status or recipient generation is unexpected"),
    }
}

pub(crate) fn validate_result_fresh(
    value: &AuthenticatedBootstrapResultV1,
    baton: &RepairActivationBatonV1,
    now_epoch_ms: u64,
) -> Result<(), &'static str> {
    if value.receipt.sealed_at_epoch_ms > now_epoch_ms || now_epoch_ms > baton.expires_at_epoch_ms {
        Err("bootstrap result is stale or from the future")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_data_compatibility_for_activation(
    value: &DataCompatibilityV1,
) -> Result<(), &'static str> {
    match value {
        DataCompatibilityV1::RollbackCompatible => Ok(()),
        DataCompatibilityV1::DeferredUntilVerified { explanation }
            if valid_text(explanation, MAX_SUMMARY_BYTES) =>
        {
            Ok(())
        }
        DataCompatibilityV1::DeferredUntilVerified { .. } => {
            Err("deferred data compatibility requires disclosure")
        }
        DataCompatibilityV1::ForwardOnlyMigrationRequired { .. } => {
            Err("forward-only data changes cannot be activated")
        }
    }
}

fn validate_bootstrap_result(value: &BootstrapResultV1) -> Result<(), &'static str> {
    let kind_valid = match &value.result {
        BootstrapResultKindV1::Unsupported { reason } => valid_platform_reason(reason),
        BootstrapResultKindV1::ActivatedVerified {
            focused_verification,
        } => validate_focused_verification_evidence(focused_verification).is_ok(),
        BootstrapResultKindV1::RolledBack {
            reason,
            rollback_evidence,
        } => {
            valid_text(reason, MAX_SUMMARY_BYTES)
                && !rollback_evidence.is_empty()
                && rollback_evidence.len() <= MAX_EVIDENCE_ARTIFACTS
                && rollback_evidence
                    .iter()
                    .all(|artifact| validate_artifact(artifact).is_ok())
        }
        BootstrapResultKindV1::ManualRecoveryRequired {
            observed_slot_state,
            instructions,
            ..
        } => {
            valid_text(observed_slot_state, MAX_SUMMARY_BYTES)
                && !instructions.is_empty()
                && instructions.len() <= 32
                && instructions
                    .iter()
                    .all(|step| valid_text(step, MAX_SUMMARY_BYTES))
        }
    };
    if value.schema_version != REPAIR_SCHEMA_VERSION_V1
        || !is_canonical_sha256(&value.baton_hash)
        || value.recipient_process_generation.0 == 0
        || value.sealed_at_epoch_ms == 0
        || !kind_valid
        || bootstrap_result_hash_v1(value).map_err(|_| "invalid receipt hash")?
            != value.receipt_hash
    {
        Err("invalid bootstrap result receipt")
    } else {
        Ok(())
    }
}

pub(crate) fn validate_bootstrap_deadlines(
    value: &BootstrapDeadlinesV1,
) -> Result<(), &'static str> {
    let values = [
        value.admission_ms,
        value.cleanup_ms,
        value.startup_ms,
        value.focused_verification_ms,
        value.rollback_ms,
        value.result_read_ms,
    ];
    let total = values
        .iter()
        .try_fold(0_u64, |total, value| total.checked_add(*value));
    if values
        .iter()
        .any(|value| *value == 0 || *value > MAX_PHASE_DEADLINE_MS)
        || total.is_none_or(|total| total > MAX_TOTAL_ACTIVATION_MS)
    {
        Err("invalid bootstrap deadlines")
    } else {
        Ok(())
    }
}

pub(crate) fn total_deadline_ms(value: &BootstrapDeadlinesV1) -> Option<u64> {
    [
        value.admission_ms,
        value.cleanup_ms,
        value.startup_ms,
        value.focused_verification_ms,
        value.rollback_ms,
        value.result_read_ms,
    ]
    .into_iter()
    .try_fold(0_u64, u64::checked_add)
}

fn valid_platform_reason(value: &PlatformReasonV1) -> bool {
    valid_text(&value.code, 128)
        && valid_text(&value.message, MAX_SUMMARY_BYTES)
        && !value.next_steps.is_empty()
        && value.next_steps.len() <= 32
        && value
            .next_steps
            .iter()
            .all(|step| valid_text(step, MAX_SUMMARY_BYTES))
}
