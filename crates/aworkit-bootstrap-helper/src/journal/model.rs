//! Data model for the tamper-evident enrollment and activation journal.
//!
//! The journal is a single-writer, append-only, hash-chained write-ahead record
//! for one mutually exclusive managed-local enrollment or activation
//! transaction. Enrollment and activation each advance through their own closed
//! phase machine; ordinals and record hashes form one append-only chain per
//! transaction id. Terminal receipts are sealed exactly once and are immutable.

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    BootstrapDeadlinesV1, BootstrapResultV1, EnrollmentPreparedV1, ManagedLocalEnrollmentRequestV1,
    ManualRecoveryNoticeV1,
};
use serde::{Deserialize, Serialize};

/// Journal schema version. V1 is the first durable journal layout.
pub const JOURNAL_SCHEMA_VERSION_V1: u16 = 1;

/// Helper protocol version stamped into every header.
pub const JOURNAL_HELPER_VERSION_V1: u16 = 1;

/// Capped durable record count; recovery is linear in this bound.
pub const MAX_JOURNAL_RECORDS: u64 = 512;

/// Upper bound on manual-recovery instruction entries.
pub const MAX_RECOVERY_INSTRUCTIONS: usize = 32;

/// Upper bound on the whole-bundle / observation hash text carried by a record.
pub const MAX_HASH_BYTES: usize = 128;

/// Kind of maintenance transaction a journal root holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalTransactionKindV1 {
    Enrollment,
    Activation,
}

impl JournalTransactionKindV1 {
    /// The enrollment transaction has not activated; it only prepared a root.
    #[must_use]
    pub const fn is_enrollment(self) -> bool {
        matches!(self, Self::Enrollment)
    }
}

/// Enrollment phases advance only through [`crate::journal::phase`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentPhaseV1 {
    /// The authenticated enrollment intent has been durably recorded.
    Intent,
    /// Publication intent and exact observation have been durably recorded.
    Published,
    /// Terminal: `EnrollmentPreparedV1` is sealed.
    Prepared,
}

/// Activation phases mirror the durable activation/rollback state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapPhaseV1 {
    Idle,
    AdmittingBaton,
    /// Terminal: managed-local guarantee absent before quiescence.
    Unsupported,
    BatonDurable,
    SlotsVerified,
    QuiescingCurrent,
    /// Terminal: current generation could not quiesce before any selector change.
    AbortedBeforeSwitch,
    CandidateSelected,
    CandidateLaunching,
    AwaitingCandidateIdentity,
    CandidateVerifying,
    /// Verified candidate; awaiting the protected receipt.
    Verified,
    /// Terminal-adjacent: the protected receipt is durable.
    ResultAvailable,
    RollingBack,
    PreviousSelected,
    PreviousRelaunching,
    /// Terminal: previous generation relaunched and handshaked.
    RolledBack,
    /// Terminal: no safe automatic terminal; manual recovery notice emitted.
    ManualRecoveryRequired,
    /// Entered at helper start when a nonterminal journal is found.
    Recovering,
}

/// A single hash-chained journal record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalRecordV1 {
    /// Zero-based position in the chain.
    pub ordinal: u64,
    /// Hash of the previous record; `None` for the first record.
    pub previous_record_hash: Option<String>,
    pub payload: JournalRecordPayloadV1,
    /// SHA-256 over the canonical encoding of `(ordinal, previous, payload)`.
    pub record_hash: String,
}

/// Closed set of record payloads an append-only chain may contain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JournalRecordPayloadV1 {
    EnrollmentIntent(EnrollmentIntentV1),
    EnrollmentObservation(EnrollmentObservationV1),
    BatonAccepted(BatonAcceptedV1),
    EffectIntent(BootstrapEffectV1),
    ObservedEffect(BootstrapEffectV1),
    PhaseAdvance(PhaseAdvanceV1),
    CommandAdmitted(CommandAdmittedV1),
}

/// Durable record that an authenticated, generation-fenced bootstrap command
/// was admitted. It binds the command to its hash so a later replay with the
/// same id but different bytes is detectable corruption. It does not itself
/// advance a phase; the coordinator journals phase transitions it drives.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandAdmittedV1 {
    pub activation_id: StableId,
    pub command_id: StableId,
    pub command_kind: String,
    pub command_hash: String,
    pub process_generation: ProcessGeneration,
    pub durable_phase: BootstrapPhaseV1,
}

/// Durable enrollment intent: the authenticated request plus helper-controlled
/// storage identities that later publication steps must reconcile against.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentIntentV1 {
    pub request: ManagedLocalEnrollmentRequestV1,
    pub managed_root_identity_hash: String,
    pub launcher_identity_hash: String,
    pub journal_identity_hash: String,
    pub selector_identity_hash: String,
}

/// Durable enrollment observation: the exact result of preparing the root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentObservationV1 {
    pub initial_active_bundle_hash: String,
    pub published_slot_verified: bool,
}

/// Durable activation intent: baton acceptance plus every fence the later
/// selector/process effects must match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatonAcceptedV1 {
    pub activation_id: StableId,
    pub baton_id: StableId,
    pub baton_hash: String,
    pub command_hash: String,
    pub challenge_id: StableId,
    pub challenge_hash: String,
    pub peer_executable_hash: String,
    pub peer_os_identity_hash: String,
    pub admission_id: StableId,
    pub admission_hash: String,
    pub management_checkpoint_id: StableId,
    pub profile_version: u16,
    pub provenance_digest: String,
    pub enrollment_digest: String,
    pub capability_generation: u64,
    pub capability_digest: String,
    pub candidate_slot_hash: String,
    pub previous_slot_hash: String,
    pub verification_plan_hash: String,
    pub current_process_generation: ProcessGeneration,
    pub candidate_process_generation: ProcessGeneration,
    pub rollback_process_generation: ProcessGeneration,
    pub deadlines: BootstrapDeadlinesV1,
}

/// A selector or process effect, recorded once as intent and once as observed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapEffectV1 {
    pub current_slot_hash: String,
    pub target_slot_hash: String,
    pub capability_generation: u64,
    pub process_generation: ProcessGeneration,
    /// Observed-effect hash; empty for an intent and filled by the observation.
    pub observation_hash: String,
}

/// A monotonic phase transition recorded after its driving effect is durable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhaseAdvanceV1 {
    pub phase: BootstrapPhaseV1,
}

/// Helper-controlled storage identities recorded with the enrollment intent so a
/// crash can reconcile only the named temporary root and publication identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentIdentitiesV1 {
    pub managed_root_identity_hash: String,
    pub launcher_identity_hash: String,
    pub journal_identity_hash: String,
    pub selector_identity_hash: String,
}

/// Enrollment-side mutation input fenced by ordinal and expected phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentJournalMutationV1 {
    pub enrollment_id: StableId,
    pub expected_ordinal: u64,
    pub expected_phase: EnrollmentPhaseV1,
    pub observation: EnrollmentObservationV1,
}

/// Activation-side effect mutation input fenced by ordinal and expected phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapJournalMutationV1 {
    pub activation_id: StableId,
    pub expected_ordinal: u64,
    pub expected_phase: BootstrapPhaseV1,
    pub effect: BootstrapEffectV1,
}

/// Activation-side phase-advance mutation input fenced by ordinal and phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapPhaseAdvanceV1 {
    pub activation_id: StableId,
    pub expected_ordinal: u64,
    pub expected_phase: BootstrapPhaseV1,
    pub next_phase: BootstrapPhaseV1,
}

/// Durable acknowledgement returned after a record is committed to the chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalDurableAckV1 {
    pub transaction_kind: JournalTransactionKindV1,
    pub transaction_id: StableId,
    pub ordinal: u64,
    pub record_hash: String,
}

/// Compact phase snapshot, replace-written only after its source record is
/// durable. It is always reconstructible from the chain and is used only to
/// speed recovery, never as an independent source of truth.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalSnapshotV1 {
    pub transaction_kind: JournalTransactionKindV1,
    pub transaction_id: StableId,
    pub head_ordinal: u64,
    pub head_record_hash: String,
    pub phase: JournalPhaseV1,
}

/// Type-erased phase used by the compact snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhaseV1 {
    Enrollment(EnrollmentPhaseV1),
    Bootstrap(BootstrapPhaseV1),
}

/// Durable terminal receipt sealed into a journal root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalReceiptV1 {
    EnrollmentPrepared(EnrollmentTerminalSealV1),
    BootstrapResult(BootstrapTerminalSealV1),
}

/// Journal-bound enrollment receipt. The seal binds the externally shared DTO
/// to the exact durable journal head without widening the cross-process DTO.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentTerminalSealV1 {
    pub receipt: EnrollmentPreparedV1,
    pub head_ordinal: u64,
    pub head_record_hash: String,
    pub seal_hash: String,
}

/// Journal-bound bootstrap receipt. A manual-recovery notice, when present, is
/// covered by the seal together with the result and exact journal head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapTerminalSealV1 {
    pub receipt: BootstrapResultV1,
    pub head_ordinal: u64,
    pub head_record_hash: String,
    pub manual_recovery_notice_hash: Option<String>,
    pub seal_hash: String,
}

/// Durable header written before the first record; it fixes the transaction
/// kind, identity, and schema so recovery can reject a mismatched writer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JournalHeaderV1 {
    Enrollment(EnrollmentJournalHeaderV1),
    Activation(ActivationJournalHeaderV1),
}

impl JournalHeaderV1 {
    #[must_use]
    pub const fn kind(&self) -> JournalTransactionKindV1 {
        match self {
            Self::Enrollment(_) => JournalTransactionKindV1::Enrollment,
            Self::Activation(_) => JournalTransactionKindV1::Activation,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentJournalHeaderV1 {
    pub schema_version: u16,
    pub enrollment_id: StableId,
    pub helper_version: u16,
    pub protocol_version: u16,
    /// SHA-256 over the canonical encoding of the remaining fields.
    pub header_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivationJournalHeaderV1 {
    pub schema_version: u16,
    pub activation_id: StableId,
    pub baton_hash: String,
    pub profile_version: u16,
    pub helper_version: u16,
    pub protocol_version: u16,
    /// SHA-256 over the canonical encoding of the remaining fields.
    pub header_hash: String,
}

/// Recovery facts for an enrollment transaction read back from the chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnrollmentRecoveryStateV1 {
    pub enrollment_id: StableId,
    pub phase: EnrollmentPhaseV1,
    pub head_ordinal: u64,
    pub head_record_hash: String,
    pub terminal: Option<EnrollmentPreparedV1>,
}

/// Recovery facts for an activation transaction read back from the chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapRecoveryStateV1 {
    pub activation_id: StableId,
    pub phase: BootstrapPhaseV1,
    pub head_ordinal: u64,
    pub head_record_hash: String,
    pub terminal: Option<BootstrapResultV1>,
    pub manual_recovery: Option<ManualRecoveryNoticeV1>,
    /// An intent whose exact observation was not yet durable when the helper
    /// stopped. Recovery must reconcile this before changing phase.
    pub open_effect: Option<BootstrapEffectV1>,
    /// The accepted baton's fence facts, rebuilt so the gateway can re-derive
    /// its in-memory session after a helper restart.
    pub baton: Option<BatonAcceptedV1>,
    /// Durable command identities and hashes used to rebuild deduplication
    /// after a helper restart.
    pub admitted_commands: Vec<CommandAdmittedV1>,
}
