//! Validation facade for the repair aggregate.

mod bootstrap;
mod candidate;
mod hashing;

pub use candidate::MAX_REPAIR_INVESTIGATION_TOKENS_V1;
pub use hashing::{
    RepairValidationError, bootstrap_admission_hash_v1, bootstrap_result_hash_v1,
    build_provenance_hash_v1, capability_report_digest_v1, core_quiescence_facts_hash_v1,
    focused_verification_evidence_hash_v1, focused_verification_plan_hash_v1,
    investigation_execution_receipt_hash_v1, repair_activation_baton_hash_v1,
    repair_candidate_hash_v1, repair_disclosure_hash_v1, repair_group_id_for_fingerprint_v1,
};

pub(crate) use bootstrap::{
    total_deadline_ms, validate_admission, validate_authenticated_result,
    validate_bootstrap_deadlines, validate_candidate_decision, validate_capability_report_fresh,
    validate_capability_report_shape, validate_checkpoint,
    validate_data_compatibility_for_activation, validate_enrollment_prepared,
    validate_enrollment_request, validate_focused_evidence_against_plan, validate_quiescence_facts,
    validate_repair_activation_baton, validate_result_fresh,
};
pub(crate) use candidate::{
    freeze_investigation_authority, validate_authenticated_investigation_execution,
    validate_candidate, validate_investigation, validate_occurrence,
};
