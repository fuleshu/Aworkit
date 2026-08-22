//! User-gated recurring-error repair and bootstrap handoff.
//!
//! This module deliberately stops at trusted-core decisions and typed ports.
//! Investigations execute through the frozen Run authority, while enrollment,
//! process switching, startup watchdogs, and rollback stay owned by the
//! independently surviving bootstrap helper.

mod aggregate;
mod model;
mod orchestrator;
mod ports;
mod validation;

pub use aggregate::{RepairAggregateError, RepairAggregateV1};
pub use model::*;
pub use orchestrator::{RepairError, RepairOrchestratorV1};
pub use ports::*;
pub use validation::{
    MAX_REPAIR_INVESTIGATION_TOKENS_V1, RepairValidationError, bootstrap_admission_hash_v1,
    bootstrap_result_hash_v1, build_provenance_hash_v1, capability_report_digest_v1,
    core_quiescence_facts_hash_v1, focused_verification_evidence_hash_v1,
    focused_verification_plan_hash_v1, investigation_execution_receipt_hash_v1,
    repair_activation_baton_hash_v1, repair_candidate_hash_v1, repair_disclosure_hash_v1,
    repair_group_id_for_fingerprint_v1,
};
