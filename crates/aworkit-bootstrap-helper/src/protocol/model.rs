//! Wire and session data model for the authenticated bootstrap protocol.
//!
//! These are the bounded DTOs that cross the helper's single core-facing,
//! version-negotiated local protocol boundary. The gateway bounds and
//! schema-validates every one of them, deduplicates command IDs, and fences
//! the current, candidate, and rollback application generations. Accepted
//! identity and command facts are written through the activation journal
//! before any acknowledgement is returned.

use aworkit_protocol::{CapabilityOutcomeV1, ProcessGeneration, StableId};
use aworkit_trusted_core::IntegrityStrengthV1;
use serde::{Deserialize, Serialize};

use crate::journal::EnrollmentIdentitiesV1;

/// The only helper protocol version supported by the gateway.
pub const BOOTSTRAP_PROTOCOL_VERSION_V1: u16 = 1;

/// Upper bound on the opaque one-use challenge nonce text.
pub const MAX_BOOTSTRAP_NONCE_CHARS: usize = 128;

/// Upper bound on the number of in-flight, not-yet-acknowledged commands the
/// gateway will remember for deduplication.
pub const MAX_SEEN_COMMAND_IDS: usize = 256;

/// Maximum canonical JSON body accepted by the bootstrap gateway (256 KiB).
pub const MAX_BOOTSTRAP_DTO_BYTES: usize = 256 * 1024;

/// Maximum duration of any independently bounded bootstrap phase.
pub const MAX_BOOTSTRAP_DEADLINE_MS: u64 = 10 * 60 * 1000;

/// OS-authenticated identity of the peer core generation on the channel.
///
/// A raw PID is never sufficient identity; the executable and OS handles are
/// required on every platform before admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerIdentityV1 {
    pub peer_process_generation: ProcessGeneration,
    pub peer_executable_hash: String,
    pub peer_os_identity_hash: String,
}

/// One-use challenge the helper issues to bind a peer to this protocol.
///
/// The nonce may be consumed exactly once; a reused, expired, or
/// peer-mismatched challenge fails closed before any effect is admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapChallengeV1 {
    pub challenge_id: StableId,
    pub protocol_version: u16,
    pub nonce: String,
    pub helper_identity_hash: String,
    pub expected_peer: PeerIdentityV1,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    /// SHA-256 over the canonical encoding of the preceding fields.
    pub challenge_hash: String,
}

/// Closed local-build enrollment state reported by the profile preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalBuildEnrollmentStateV1 {
    Enrolled,
    NotEnrolled,
    Mismatched,
}

/// Deterministic ownership and layout facts that gate the unprivileged profile.
///
/// These are observations that may only *downgrade* eligibility, never upgrade
/// it; a missing fact keeps the build unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnershipFactsV1 {
    pub per_user_owned: bool,
    pub same_volume: bool,
    pub selector_atomic: bool,
    pub helper_survives_outside_slots: bool,
}

/// Bounded local-build enrollment record consumed by capability queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalBuildEnrollmentV1 {
    pub installation_id: StableId,
    pub profile_version: u16,
    pub enrollment_digest: String,
    pub active_slot_hash: String,
    pub current_bundle_hash: String,
    pub selector_identity_hash: String,
    pub helper_identity_hash: String,
    pub launcher_identity_hash: String,
    pub journal_identity_hash: String,
    pub ownership: OwnershipFactsV1,
    pub state: LocalBuildEnrollmentStateV1,
}

/// The single v1 activation profile. V1 deliberately implements only this.
///
/// It declares the same-user hash-and-ownership integrity strength and makes
/// no publisher-authentication claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedLocalBuildProfileV1 {
    pub profile_version: u16,
    pub integrity_strength: IntegrityStrengthV1,
}

/// Bounded plan the profile emits for a managed-local enrollment preparation.
///
/// A plan is a fixed set of helper-controlled identities, never a free-form
/// path, shell command, or caller-selected destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentPlanV1 {
    pub plan_id: StableId,
    pub installation_id: StableId,
    pub profile_version: u16,
    pub helper_root_identity_hash: String,
    pub initial_active_slot_root_hash: String,
    pub selector_identity_hash: String,
    pub journal_identity_hash: String,
    /// SHA-256 over the canonical encoding of the preceding fields.
    pub plan_hash: String,
}

/// Closed union of actionable bootstrap commands the gateway will admit.
///
/// Generic paths, shell strings, environment maps, tool requests, plugin
/// payloads, and eligibility overrides are not representable here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BootstrapCommandV1 {
    /// Begins an admitted activation after the baton is durably accepted.
    BeginActivation {
        command_id: StableId,
        activation_id: StableId,
        challenge_id: StableId,
        expected_phase: crate::journal::BootstrapPhaseV1,
    },
    /// The candidate generation is launched and identity-verified.
    CandidateGenerationReady {
        command_id: StableId,
        activation_id: StableId,
        generation: ProcessGeneration,
    },
    /// Focused verification finished for the exact candidate generation.
    FocusedVerificationCompleted {
        command_id: StableId,
        activation_id: StableId,
        generation: ProcessGeneration,
        verification_plan_hash: String,
        outcome: CapabilityOutcomeV1,
    },
    /// Reads the protected result, only to the sealed recipient generation.
    ReadResult {
        command_id: StableId,
        activation_id: StableId,
        recipient_process_generation: ProcessGeneration,
    },
}

/// Durable acknowledgement returned only after the command hash is journaled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapCommandAckV1 {
    pub command_id: StableId,
    pub activation_id: StableId,
    pub command_hash: String,
    pub phase: crate::journal::BootstrapPhaseV1,
    /// `true` once the command's hash is durable in the activation journal.
    pub durable: bool,
}

/// The exact result of preparing a managed-local enrollment root.
///
/// The observation is journaled as the durable effect of the intent, and the
/// prepared receipt is sealed exactly once. Both come from the same
/// helper-controlled layout and never from peer-supplied paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentPreparationV1 {
    pub observation: crate::journal::EnrollmentObservationV1,
    pub prepared: aworkit_trusted_core::EnrollmentPreparedV1,
}

/// Helper-controlled storage and protocol identity the gateway holds.
///
/// These are the helper's own facts (where its journal, launcher, and selector
/// live), never values supplied by the peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelperIdentityV1 {
    pub helper_identity_hash: String,
    pub profile_version: u16,
    pub enrollment_identities: EnrollmentIdentitiesV1,
}

/// Bounded in-memory session for one nonterminal activation.
///
/// The gateway owns no independent durable file; these facts are rebuilt from
/// the activation journal after a helper restart.
#[derive(Clone, Debug)]
pub struct BootstrapSessionV1 {
    pub activation_id: StableId,
    pub protocol_version: u16,
    pub peer: PeerIdentityV1,
    pub challenge_id: StableId,
    pub challenge_nonce: String,
    pub challenge_hash: String,
    pub helper_identity_hash: String,
    pub provenance_digest: String,
    pub enrollment_digest: String,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub accepted_baton_hash: String,
    pub verification_plan_hash: String,
    pub current_process_generation: ProcessGeneration,
    pub candidate_process_generation: ProcessGeneration,
    pub rollback_process_generation: ProcessGeneration,
}

/// Which single transaction (enrollment or activation) currently holds the
/// helper's maintenance lock. Enrollment and activation never overlap.
#[derive(Clone, Debug)]
pub enum ActiveTransactionV1 {
    Enrollment { enrollment_id: StableId },
    Activation { activation_id: StableId },
}

impl ActiveTransactionV1 {
    /// The transaction is an activation rather than an enrollment.
    #[must_use]
    pub const fn is_activation(&self) -> bool {
        matches!(self, Self::Activation { .. })
    }
}
