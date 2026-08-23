//! Immutable execution context for one admitted activation.

use crate::profile::ActivationPlanV1;
use aworkit_protocol::StableId;
use aworkit_trusted_core::FocusedVerificationPlanV1;

/// Facts not duplicated in the durable baton but required to launch and seal
/// its result. Callers cannot choose selector targets or process generations;
/// those remain fixed in `plan` and are cross-checked against the journal.
#[derive(Clone, Debug)]
pub struct ActivationExecutionV1 {
    pub plan: ActivationPlanV1,
    pub installation_id: StableId,
    pub management_checkpoint_id: StableId,
    pub verification_plan: FocusedVerificationPlanV1,
    pub helper_detached_and_surviving: bool,
    /// Audit metadata only; timestamps never choose a phase or disposition.
    pub sealed_at_epoch_ms: u64,
}
