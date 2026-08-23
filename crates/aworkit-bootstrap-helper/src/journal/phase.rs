//! Closed phase state machines for enrollment and activation.
//!
//! Phases advance only along the edges defined here, which mirror the durable
//! activation/watchdog/rollback/relaunch/receipt/crash-recovery state machine.
//! Terminal phases are immutable; timestamps never select a phase.

use super::model::{BootstrapPhaseV1, EnrollmentPhaseV1};

/// Whether `from` may advance to `to` for the enrollment machine.
#[must_use]
pub fn enrollment_can_advance(from: EnrollmentPhaseV1, to: EnrollmentPhaseV1) -> bool {
    matches!(
        (from, to),
        (EnrollmentPhaseV1::Intent, EnrollmentPhaseV1::Published)
            | (EnrollmentPhaseV1::Published, EnrollmentPhaseV1::Prepared)
    )
}

/// Whether the enrollment phase is terminal and therefore immutable.
#[must_use]
pub fn enrollment_is_terminal(phase: EnrollmentPhaseV1) -> bool {
    matches!(phase, EnrollmentPhaseV1::Prepared)
}

/// Whether `from` may advance to `to` for the activation machine.
#[must_use]
pub fn bootstrap_can_advance(from: BootstrapPhaseV1, to: BootstrapPhaseV1) -> bool {
    if from == to {
        return false;
    }
    matches!(
        (from, to),
        (BootstrapPhaseV1::Idle, BootstrapPhaseV1::AdmittingBaton)
            | (
                BootstrapPhaseV1::AdmittingBaton,
                BootstrapPhaseV1::Unsupported
            )
            | (
                BootstrapPhaseV1::AdmittingBaton,
                BootstrapPhaseV1::BatonDurable
            )
            | (
                BootstrapPhaseV1::BatonDurable,
                BootstrapPhaseV1::SlotsVerified
            )
            | (
                BootstrapPhaseV1::BatonDurable,
                BootstrapPhaseV1::Unsupported
            )
            | (
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::QuiescingCurrent
            )
            | (
                BootstrapPhaseV1::SlotsVerified,
                BootstrapPhaseV1::Unsupported
            )
            | (
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::AbortedBeforeSwitch
            )
            | (
                BootstrapPhaseV1::QuiescingCurrent,
                BootstrapPhaseV1::CandidateSelected
            )
            | (
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::CandidateLaunching
            )
            | (
                BootstrapPhaseV1::CandidateSelected,
                BootstrapPhaseV1::RollingBack
            )
            | (
                BootstrapPhaseV1::CandidateLaunching,
                BootstrapPhaseV1::AwaitingCandidateIdentity
            )
            | (
                BootstrapPhaseV1::CandidateLaunching,
                BootstrapPhaseV1::RollingBack
            )
            | (
                BootstrapPhaseV1::AwaitingCandidateIdentity,
                BootstrapPhaseV1::CandidateVerifying
            )
            | (
                BootstrapPhaseV1::AwaitingCandidateIdentity,
                BootstrapPhaseV1::RollingBack
            )
            | (
                BootstrapPhaseV1::CandidateVerifying,
                BootstrapPhaseV1::Verified
            )
            | (
                BootstrapPhaseV1::CandidateVerifying,
                BootstrapPhaseV1::RollingBack
            )
            | (
                BootstrapPhaseV1::Verified,
                BootstrapPhaseV1::ResultAvailable
            )
            | (
                BootstrapPhaseV1::RollingBack,
                BootstrapPhaseV1::PreviousSelected
            )
            | (
                BootstrapPhaseV1::RollingBack,
                BootstrapPhaseV1::ManualRecoveryRequired
            )
            | (
                BootstrapPhaseV1::PreviousSelected,
                BootstrapPhaseV1::PreviousRelaunching
            )
            | (
                BootstrapPhaseV1::PreviousRelaunching,
                BootstrapPhaseV1::RolledBack
            )
            | (
                BootstrapPhaseV1::PreviousRelaunching,
                BootstrapPhaseV1::ManualRecoveryRequired
            )
            | (
                BootstrapPhaseV1::RolledBack,
                BootstrapPhaseV1::ResultAvailable
            )
            | (
                BootstrapPhaseV1::Unsupported,
                BootstrapPhaseV1::ResultAvailable
            )
            | (
                BootstrapPhaseV1::AbortedBeforeSwitch,
                BootstrapPhaseV1::ResultAvailable
            )
            | (
                BootstrapPhaseV1::ManualRecoveryRequired,
                BootstrapPhaseV1::ResultAvailable
            )
            | (BootstrapPhaseV1::Recovering, BootstrapPhaseV1::BatonDurable)
            | (BootstrapPhaseV1::Recovering, BootstrapPhaseV1::RollingBack)
            | (
                BootstrapPhaseV1::Recovering,
                BootstrapPhaseV1::ResultAvailable
            )
            | (
                BootstrapPhaseV1::Recovering,
                BootstrapPhaseV1::ManualRecoveryRequired
            )
            | (
                BootstrapPhaseV1::Recovering,
                BootstrapPhaseV1::AbortedBeforeSwitch
            )
            | (BootstrapPhaseV1::Verified, BootstrapPhaseV1::Recovering)
            | (
                BootstrapPhaseV1::BatonDurable
                    | BootstrapPhaseV1::SlotsVerified
                    | BootstrapPhaseV1::QuiescingCurrent
                    | BootstrapPhaseV1::CandidateSelected
                    | BootstrapPhaseV1::CandidateLaunching
                    | BootstrapPhaseV1::AwaitingCandidateIdentity
                    | BootstrapPhaseV1::CandidateVerifying
                    | BootstrapPhaseV1::RollingBack
                    | BootstrapPhaseV1::PreviousSelected
                    | BootstrapPhaseV1::PreviousRelaunching,
                BootstrapPhaseV1::Recovering
            )
    )
}

/// Whether the activation phase is terminal and therefore immutable.
///
/// The disposition-reached phases (a verified candidate, a completed rollback,
/// an unsupported or aborted guarantee, or manual recovery) can only be sealed
/// into the protected receipt, never advanced further. `ResultAvailable` marks
/// the receipt as durable and is likewise immutable.
#[must_use]
pub fn bootstrap_is_terminal(phase: BootstrapPhaseV1) -> bool {
    matches!(
        phase,
        BootstrapPhaseV1::Verified
            | BootstrapPhaseV1::RolledBack
            | BootstrapPhaseV1::Unsupported
            | BootstrapPhaseV1::AbortedBeforeSwitch
            | BootstrapPhaseV1::ManualRecoveryRequired
            | BootstrapPhaseV1::ResultAvailable
    )
}

/// Whether a result disposition may be sealed from the durable activation
/// phase. An aborted-before-switch flow shares the externally defined
/// `Unsupported` receipt while remaining distinguishable in the journal.
#[must_use]
pub fn result_can_seal(
    phase: BootstrapPhaseV1,
    kind: &aworkit_protocol::BootstrapResultKindV1,
) -> bool {
    use aworkit_protocol::BootstrapResultKindV1;
    match kind {
        BootstrapResultKindV1::ActivatedVerified { .. } => phase == BootstrapPhaseV1::Verified,
        BootstrapResultKindV1::RolledBack { .. } => phase == BootstrapPhaseV1::RolledBack,
        BootstrapResultKindV1::Unsupported { .. } => matches!(
            phase,
            BootstrapPhaseV1::Unsupported | BootstrapPhaseV1::AbortedBeforeSwitch
        ),
        BootstrapResultKindV1::ManualRecoveryRequired { .. } => {
            phase == BootstrapPhaseV1::ManualRecoveryRequired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_machine_only_walks_the_defined_edge() {
        assert!(enrollment_can_advance(
            EnrollmentPhaseV1::Intent,
            EnrollmentPhaseV1::Published
        ));
        assert!(enrollment_can_advance(
            EnrollmentPhaseV1::Published,
            EnrollmentPhaseV1::Prepared
        ));
        assert!(!enrollment_can_advance(
            EnrollmentPhaseV1::Intent,
            EnrollmentPhaseV1::Prepared
        ));
        assert!(enrollment_is_terminal(EnrollmentPhaseV1::Prepared));
        assert!(!enrollment_is_terminal(EnrollmentPhaseV1::Intent));
    }

    #[test]
    fn activation_terminal_phases_are_immutable() {
        for terminal in [
            BootstrapPhaseV1::Verified,
            BootstrapPhaseV1::RolledBack,
            BootstrapPhaseV1::Unsupported,
            BootstrapPhaseV1::AbortedBeforeSwitch,
            BootstrapPhaseV1::ManualRecoveryRequired,
            BootstrapPhaseV1::ResultAvailable,
        ] {
            assert!(bootstrap_is_terminal(terminal), "{terminal:?}");
        }
        for active in [
            BootstrapPhaseV1::Idle,
            BootstrapPhaseV1::AdmittingBaton,
            BootstrapPhaseV1::CandidateVerifying,
            BootstrapPhaseV1::RollingBack,
        ] {
            assert!(!bootstrap_is_terminal(active), "{active:?}");
        }
    }

    #[test]
    fn activation_rejects_same_phase_and_unknown_edges() {
        assert!(!bootstrap_can_advance(
            BootstrapPhaseV1::Idle,
            BootstrapPhaseV1::Idle
        ));
        assert!(!bootstrap_can_advance(
            BootstrapPhaseV1::Idle,
            BootstrapPhaseV1::Verified
        ));
        assert!(!bootstrap_can_advance(
            BootstrapPhaseV1::ResultAvailable,
            BootstrapPhaseV1::Idle
        ));
    }
}
