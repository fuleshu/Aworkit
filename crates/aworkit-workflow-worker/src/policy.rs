//! Frozen attempt policy, including conservative treatment of uncertain effects.

/// Normalized outcome classification supplied by a core-mediated capability result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectOutcome {
    Success,
    DefiniteNotStarted,
    Failed,
    OutcomeUncertain,
}
/// The exact scheduler action permitted after an attempt outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttemptDecision {
    Continue,
    Retry,
    Fallback(String),
    WaitForApproval,
    TerminalFailure,
}
/// Immutable policy declared in the frozen plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptPolicy {
    pub max_retries: u32,
    pub fallback: Option<String>,
    pub requires_approval: bool,
}
impl AttemptPolicy {
    /// Selects a deterministic action and never retries an uncertain side effect.
    #[must_use]
    pub fn decide(&self, attempt: u32, outcome: EffectOutcome) -> AttemptDecision {
        match outcome {
            EffectOutcome::Success => AttemptDecision::Continue,
            EffectOutcome::OutcomeUncertain => AttemptDecision::WaitForApproval,
            EffectOutcome::DefiniteNotStarted if attempt < self.max_retries => {
                AttemptDecision::Retry
            }
            EffectOutcome::Failed if attempt < self.max_retries && !self.requires_approval => {
                AttemptDecision::Retry
            }
            EffectOutcome::DefiniteNotStarted | EffectOutcome::Failed => self
                .fallback
                .clone()
                .map_or(AttemptDecision::TerminalFailure, AttemptDecision::Fallback),
        }
    }
}
