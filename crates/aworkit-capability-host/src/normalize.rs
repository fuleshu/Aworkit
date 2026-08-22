//! Streaming redaction and conservative side-effect outcome normalization.

use aworkit_protocol::StableId;
use thiserror::Error;
use zeroize::Zeroizing;

/// Compatibility outcome used by the original scaffold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeKind {
    Succeeded,
    FailedSafe,
    UnknownEffect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityOutcome {
    pub invocation_id: StableId,
    pub kind: OutcomeKind,
    pub retry_safe: bool,
}

/// Secret set deliberately omits `Debug` so logs cannot print plaintext material.
#[derive(Clone, Default)]
pub struct Redactor {
    secrets: Zeroizing<Vec<String>>,
    maximum_secret_bytes: usize,
}

impl Redactor {
    #[must_use]
    pub fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|value| !value.is_empty());
        secrets.sort_by_key(|right| std::cmp::Reverse(right.len()));
        secrets.dedup();
        let maximum_secret_bytes = secrets.iter().map(String::len).max().unwrap_or(0);
        Self {
            secrets: Zeroizing::new(secrets),
            maximum_secret_bytes,
        }
    }

    #[must_use]
    pub fn redact(&self, value: &str) -> String {
        self.secrets.iter().fold(value.to_owned(), |text, secret| {
            text.replace(secret, "[REDACTED]")
        })
    }

    #[must_use]
    pub fn stream(&self) -> StreamingRedactor<'_> {
        StreamingRedactor {
            redactor: self,
            pending: Zeroizing::new(String::new()),
        }
    }
}

/// Retains a bounded suffix so a secret split between chunks is never emitted early.
pub struct StreamingRedactor<'a> {
    redactor: &'a Redactor,
    pending: Zeroizing<String>,
}

impl StreamingRedactor<'_> {
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let redacted = self.redactor.redact(&self.pending);
        let retain = self.redactor.maximum_secret_bytes.saturating_sub(1);
        if retain == 0 || redacted.len() <= retain {
            if retain == 0 {
                self.pending.clear();
                return redacted;
            }
            return String::new();
        }
        let mut split = redacted.len() - retain;
        while split > 0 && !redacted.is_char_boundary(split) {
            split -= 1;
        }
        let emitted = redacted[..split].to_owned();
        self.pending.clear();
        self.pending.push_str(&redacted[split..]);
        emitted
    }

    pub fn finish(mut self) -> String {
        let result = self.redactor.redact(&self.pending);
        self.pending.clear();
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchEvidenceV1 {
    DefinitelyNotStarted,
    Started,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalEvidenceV1 {
    Succeeded,
    Failed,
    CancelledWithEvidence,
    MissingOrConflicting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeDispositionV1 {
    Succeeded,
    FailedDefiniteNotStarted,
    FailedKnownStarted,
    CancelledWithEvidence,
    OutcomeUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrySafetyV1 {
    EligibleUnderFrozenPolicy,
    SameInvocationIdOnly,
    NotSafe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectEvidenceV1 {
    pub dispatch: DispatchEvidenceV1,
    pub terminal: TerminalEvidenceV1,
    pub descriptor_is_idempotent: bool,
    pub host_guarantees_same_id_deduplication: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityOutcomeV1 {
    pub invocation_id: StableId,
    pub disposition: OutcomeDispositionV1,
    pub retry_safety: RetrySafetyV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NormalizedContentV1 {
    AssistantOutput(String),
    StandardOutput(String),
    StandardError(String),
    ReasoningRaw(String),
    ReasoningSummary(String),
    Progress(String),
    Diagnostic(String),
    FinalResult(String),
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostInvocationEventV1 {
    pub invocation_id: StableId,
    pub sequence: u64,
    pub content: NormalizedContentV1,
}

/// Per-invocation normalizer owns one sequence and one terminal fence.
pub struct InvocationNormalizer {
    invocation_id: StableId,
    next_sequence: u64,
    terminal: bool,
    redactor: Redactor,
}

impl InvocationNormalizer {
    #[must_use]
    pub fn new(invocation_id: StableId, redactor: Redactor) -> Self {
        Self {
            invocation_id,
            next_sequence: 0,
            terminal: false,
            redactor,
        }
    }

    pub fn event(
        &mut self,
        content: NormalizedContentV1,
    ) -> Result<HostInvocationEventV1, NormalizeError> {
        if self.terminal {
            return Err(NormalizeError::TerminalClosed);
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(NormalizeError::SequenceExhausted)?;
        Ok(HostInvocationEventV1 {
            invocation_id: self.invocation_id.clone(),
            sequence: self.next_sequence,
            content: redact_content(content, &self.redactor),
        })
    }

    pub fn terminal(
        &mut self,
        evidence: EffectEvidenceV1,
    ) -> Result<CapabilityOutcomeV1, NormalizeError> {
        if self.terminal {
            return Err(NormalizeError::TerminalClosed);
        }
        self.terminal = true;
        Ok(classify_outcome(self.invocation_id.clone(), evidence))
    }
}

fn redact_content(content: NormalizedContentV1, redactor: &Redactor) -> NormalizedContentV1 {
    match content {
        NormalizedContentV1::AssistantOutput(value) => {
            NormalizedContentV1::AssistantOutput(redactor.redact(&value))
        }
        NormalizedContentV1::StandardOutput(value) => {
            NormalizedContentV1::StandardOutput(redactor.redact(&value))
        }
        NormalizedContentV1::StandardError(value) => {
            NormalizedContentV1::StandardError(redactor.redact(&value))
        }
        NormalizedContentV1::ReasoningRaw(value) => {
            NormalizedContentV1::ReasoningRaw(redactor.redact(&value))
        }
        NormalizedContentV1::ReasoningSummary(value) => {
            NormalizedContentV1::ReasoningSummary(redactor.redact(&value))
        }
        NormalizedContentV1::Progress(value) => {
            NormalizedContentV1::Progress(redactor.redact(&value))
        }
        NormalizedContentV1::Diagnostic(value) => {
            NormalizedContentV1::Diagnostic(redactor.redact(&value))
        }
        NormalizedContentV1::FinalResult(value) => {
            NormalizedContentV1::FinalResult(redactor.redact(&value))
        }
        NormalizedContentV1::Error(value) => NormalizedContentV1::Error(redactor.redact(&value)),
    }
}

#[must_use]
pub fn classify_outcome(
    invocation_id: StableId,
    evidence: EffectEvidenceV1,
) -> CapabilityOutcomeV1 {
    let disposition = match (evidence.dispatch, evidence.terminal) {
        (
            DispatchEvidenceV1::Started | DispatchEvidenceV1::Unknown,
            TerminalEvidenceV1::Succeeded,
        ) => OutcomeDispositionV1::Succeeded,
        (DispatchEvidenceV1::DefinitelyNotStarted, TerminalEvidenceV1::Failed)
        | (DispatchEvidenceV1::DefinitelyNotStarted, TerminalEvidenceV1::MissingOrConflicting) => {
            OutcomeDispositionV1::FailedDefiniteNotStarted
        }
        (DispatchEvidenceV1::Started, TerminalEvidenceV1::Failed) => {
            OutcomeDispositionV1::FailedKnownStarted
        }
        (DispatchEvidenceV1::Started, TerminalEvidenceV1::CancelledWithEvidence) => {
            OutcomeDispositionV1::CancelledWithEvidence
        }
        _ => OutcomeDispositionV1::OutcomeUncertain,
    };
    let retry_safety = match disposition {
        OutcomeDispositionV1::FailedDefiniteNotStarted => RetrySafetyV1::EligibleUnderFrozenPolicy,
        OutcomeDispositionV1::FailedKnownStarted | OutcomeDispositionV1::CancelledWithEvidence
            if evidence.descriptor_is_idempotent =>
        {
            RetrySafetyV1::EligibleUnderFrozenPolicy
        }
        OutcomeDispositionV1::FailedKnownStarted | OutcomeDispositionV1::CancelledWithEvidence
            if evidence.host_guarantees_same_id_deduplication =>
        {
            RetrySafetyV1::SameInvocationIdOnly
        }
        _ => RetrySafetyV1::NotSafe,
    };
    CapabilityOutcomeV1 {
        invocation_id,
        disposition,
        retry_safety,
    }
}

/// Compatibility sequence normalizer retained for existing adapters.
#[derive(Default)]
pub struct StreamNormalizer {
    next: u64,
}

impl StreamNormalizer {
    pub fn event(&mut self, raw: &str, redactor: &Redactor) -> (u64, String) {
        self.next = self.next.saturating_add(1);
        (self.next, redactor.redact(raw))
    }

    pub fn outcome(
        &self,
        invocation_id: StableId,
        definitely_succeeded: Option<bool>,
    ) -> CapabilityOutcome {
        let kind = match definitely_succeeded {
            Some(true) => OutcomeKind::Succeeded,
            Some(false) => OutcomeKind::FailedSafe,
            None => OutcomeKind::UnknownEffect,
        };
        CapabilityOutcome {
            invocation_id,
            retry_safe: matches!(kind, OutcomeKind::FailedSafe),
            kind,
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NormalizeError {
    #[error("terminal outcome already emitted")]
    TerminalClosed,
    #[error("stream sequence is exhausted")]
    SequenceExhausted,
}
