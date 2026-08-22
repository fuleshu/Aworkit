//! Trusted plugin lifecycle, restart, drift, and no-replay state machine.

use std::collections::BTreeMap;

use aworkit_protocol::{MAX_SAFE_WIRE_INTEGER, ProcessGeneration, StableId};
use thiserror::Error;

use crate::{
    CapabilityOutcomeV1, DispatchEvidenceV1, EffectEvidenceV1, OutcomeDispositionV1, RetrySafetyV1,
    TerminalEvidenceV1, classify_outcome,
};

use super::{
    PinnedPluginManifestV1, PluginCancelResultV1, PluginEffectStatusV1, PluginHandshakeIdentityV1,
    PluginHandshakeRequestV1, PluginHandshakeResultV1, PluginHealthResultV1, PluginHealthStatusV1,
    PluginInvocationEventKindV1, PluginInvocationEventV1, PluginInvocationRequestV1,
    PluginInvocationResultV1, PluginTerminalStatusV1, TRUSTED_PLUGIN_SECURITY_DISCLOSURE,
};

const MAXIMUM_RETAINED_SETTLEMENTS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginLifecycleStateV1 {
    EnabledPinned,
    Launching,
    Healthy,
    RunningInvocation,
    RestartBackoff,
    OutcomeUncertain,
    Quarantined,
    Disabled,
    ShuttingDown,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginDispatchPhaseV1 {
    Prepared,
    Sent,
    Accepted,
    EffectMayHaveStarted,
}

/// The lifecycle never replays an invocation. A definitely-not-started outcome
/// merely permits the trusted core to evaluate a distinct frozen-policy attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginReplayDispositionV1 {
    NeverReplay,
    CoreMayCreateNewAttempt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInvocationSettlementV1 {
    pub outcome: CapabilityOutcomeV1,
    pub replay: PluginReplayDispositionV1,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginRestartPolicyV1 {
    pub maximum_restart_attempts: u32,
    pub initial_backoff_millis: u64,
    pub maximum_backoff_millis: u64,
}

impl Default for PluginRestartPolicyV1 {
    fn default() -> Self {
        Self {
            maximum_restart_attempts: 3,
            initial_backoff_millis: 100,
            maximum_backoff_millis: 10_000,
        }
    }
}

impl PluginRestartPolicyV1 {
    fn validate(self) -> Result<Self, PluginLifecycleError> {
        if self.initial_backoff_millis == 0
            || self.maximum_backoff_millis < self.initial_backoff_millis
            || self.maximum_backoff_millis > MAX_SAFE_WIRE_INTEGER
        {
            return Err(PluginLifecycleError::InvalidRestartPolicy);
        }
        Ok(self)
    }
}

/// In-memory lifecycle evidence is bounded. Canonical invocation settlement is
/// owned by the trusted core; this cache only fences duplicates in one process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginLifecycleLimitsV1 {
    pub maximum_retained_settlements: usize,
}

impl Default for PluginLifecycleLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_retained_settlements: 1024,
        }
    }
}

impl PluginLifecycleLimitsV1 {
    fn validate(self) -> Result<Self, PluginLifecycleError> {
        if self.maximum_retained_settlements == 0
            || self.maximum_retained_settlements > MAXIMUM_RETAINED_SETTLEMENTS
        {
            Err(PluginLifecycleError::InvalidLifecycleLimits)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Debug)]
struct ActiveInvocationV1 {
    invocation_id: StableId,
    phase: PluginDispatchPhaseV1,
    last_sequence: u64,
    cancellation_requested: bool,
}

/// State for one exact extension version/hash in one host generation.
pub struct TrustedPluginLifecycleV1 {
    pinned: PinnedPluginManifestV1,
    state: PluginLifecycleStateV1,
    restart_policy: PluginRestartPolicyV1,
    lifecycle_limits: PluginLifecycleLimitsV1,
    restart_attempts: u32,
    restart_not_before_millis: Option<u64>,
    active: Option<ActiveInvocationV1>,
    settlements: BTreeMap<String, PluginInvocationSettlementV1>,
    state_reason: Option<String>,
}

impl TrustedPluginLifecycleV1 {
    pub fn new(
        pinned: PinnedPluginManifestV1,
        restart_policy: PluginRestartPolicyV1,
    ) -> Result<Self, PluginLifecycleError> {
        Self::new_with_limits(pinned, restart_policy, PluginLifecycleLimitsV1::default())
    }

    pub fn new_with_limits(
        pinned: PinnedPluginManifestV1,
        restart_policy: PluginRestartPolicyV1,
        lifecycle_limits: PluginLifecycleLimitsV1,
    ) -> Result<Self, PluginLifecycleError> {
        Ok(Self {
            pinned,
            state: PluginLifecycleStateV1::EnabledPinned,
            restart_policy: restart_policy.validate()?,
            lifecycle_limits: lifecycle_limits.validate()?,
            restart_attempts: 0,
            restart_not_before_millis: None,
            active: None,
            settlements: BTreeMap::new(),
            state_reason: None,
        })
    }

    #[must_use]
    pub fn state(&self) -> PluginLifecycleStateV1 {
        self.state
    }

    #[must_use]
    pub fn pinned(&self) -> &PinnedPluginManifestV1 {
        &self.pinned
    }

    #[must_use]
    pub fn security_disclosure(&self) -> &'static str {
        TRUSTED_PLUGIN_SECURITY_DISCLOSURE
    }

    /// An ordinary trusted plugin process is never a security sandbox.
    #[must_use]
    pub fn is_security_sandbox(&self) -> bool {
        false
    }

    #[must_use]
    pub fn state_reason(&self) -> Option<&str> {
        self.state_reason.as_deref()
    }

    #[must_use]
    pub fn restart_not_before_millis(&self) -> Option<u64> {
        self.restart_not_before_millis
    }

    #[must_use]
    pub fn restart_attempts(&self) -> u32 {
        self.restart_attempts
    }

    #[must_use]
    pub fn retained_settlement_count(&self) -> usize {
        self.settlements.len()
    }

    #[must_use]
    pub fn active_dispatch_phase(&self) -> Option<PluginDispatchPhaseV1> {
        self.active.as_ref().map(|value| value.phase)
    }

    #[must_use]
    pub fn settlement(&self, invocation_id: &StableId) -> Option<&PluginInvocationSettlementV1> {
        self.settlements.get(invocation_id.as_str())
    }

    #[must_use]
    pub fn expected_handshake(&self) -> PluginHandshakeRequestV1 {
        let mut contribution_ids = self.pinned.pin().contribution_ids.clone();
        contribution_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        PluginHandshakeRequestV1 {
            expected: PluginHandshakeIdentityV1 {
                extension_id: self.pinned.pin().extension_id.clone(),
                version: self.pinned.pin().version.clone(),
                content_hash: self.pinned.pin().content_hash.clone(),
                protocol_version: self.pinned.pin().protocol_version,
                contribution_ids,
            },
        }
    }

    pub fn begin_launch(&mut self) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::EnabledPinned)?;
        self.state = PluginLifecycleStateV1::Launching;
        self.state_reason = None;
        Ok(())
    }

    pub fn complete_handshake(
        &mut self,
        result: &PluginHandshakeResultV1,
        now_millis: u64,
    ) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::Launching)?;
        validate_now(now_millis)?;
        if !result.accepted {
            self.schedule_restart(now_millis, "plugin rejected the host handshake")?;
            return Err(PluginLifecycleError::HandshakeRejected);
        }
        if result.observed != self.expected_handshake().expected {
            self.quarantine(
                "plugin handshake identity, version, hash, protocol, or contribution drift",
            );
            return Err(PluginLifecycleError::HandshakeDrift);
        }
        self.state = PluginLifecycleStateV1::Healthy;
        self.restart_not_before_millis = None;
        self.state_reason = None;
        Ok(())
    }

    pub fn launch_failed(
        &mut self,
        now_millis: u64,
        reason: impl Into<String>,
    ) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::Launching)?;
        self.schedule_restart(now_millis, reason)
    }

    /// Starts a fresh process after backoff. No previous invocation is retained
    /// or resent by this transition.
    pub fn begin_restart(&mut self, now_millis: u64) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::RestartBackoff)?;
        validate_now(now_millis)?;
        let not_before = self
            .restart_not_before_millis
            .ok_or(PluginLifecycleError::InvalidState)?;
        if now_millis < not_before {
            return Err(PluginLifecycleError::RestartBackoffActive);
        }
        self.state = PluginLifecycleStateV1::Launching;
        self.restart_not_before_millis = None;
        Ok(())
    }

    pub fn begin_invocation(
        &mut self,
        request: &PluginInvocationRequestV1,
    ) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::Healthy)?;
        if !self.pinned.permits_contribution(&request.contribution_id) {
            return Err(PluginLifecycleError::ContributionNotPinned);
        }
        if self
            .settlements
            .contains_key(request.invocation_id.as_str())
        {
            return Err(PluginLifecycleError::InvocationAlreadySettled);
        }
        if self.settlements.len() >= self.lifecycle_limits.maximum_retained_settlements {
            self.quarantine("plugin settlement retention capacity is exhausted");
            return Err(PluginLifecycleError::SettlementCapacityExhausted);
        }
        self.active = Some(ActiveInvocationV1 {
            invocation_id: request.invocation_id.clone(),
            phase: PluginDispatchPhaseV1::Prepared,
            last_sequence: 0,
            cancellation_requested: false,
        });
        self.state = PluginLifecycleStateV1::RunningInvocation;
        Ok(())
    }

    pub fn mark_invocation_sent(
        &mut self,
        invocation_id: &StableId,
    ) -> Result<(), PluginLifecycleError> {
        let active = self.active_mut(invocation_id)?;
        if active.phase != PluginDispatchPhaseV1::Prepared {
            return Err(PluginLifecycleError::InvalidInvocationPhase);
        }
        active.phase = PluginDispatchPhaseV1::Sent;
        Ok(())
    }

    pub fn accept_invocation(
        &mut self,
        invocation_id: &StableId,
    ) -> Result<(), PluginLifecycleError> {
        let active = self.active_mut(invocation_id)?;
        if active.phase != PluginDispatchPhaseV1::Sent {
            return Err(PluginLifecycleError::InvalidInvocationPhase);
        }
        active.phase = PluginDispatchPhaseV1::Accepted;
        Ok(())
    }

    pub fn observe_invocation_event(
        &mut self,
        event: &PluginInvocationEventV1,
    ) -> Result<(), PluginLifecycleError> {
        let active = self.active_mut(&event.invocation_id)?;
        if active.phase == PluginDispatchPhaseV1::Prepared || event.sequence <= active.last_sequence
        {
            return Err(PluginLifecycleError::InvalidInvocationEvent);
        }
        active.last_sequence = event.sequence;
        if event.event == PluginInvocationEventKindV1::EffectMayHaveStarted {
            active.phase = PluginDispatchPhaseV1::EffectMayHaveStarted;
        }
        Ok(())
    }

    pub fn request_cancel(&mut self, invocation_id: &StableId) -> Result<(), PluginLifecycleError> {
        let active = self.active_mut(invocation_id)?;
        active.cancellation_requested = true;
        Ok(())
    }

    pub fn apply_cancel_result(
        &mut self,
        result: &PluginCancelResultV1,
    ) -> Result<Option<PluginInvocationSettlementV1>, PluginLifecycleError> {
        let active = self.active_ref(&result.invocation_id)?;
        if !active.cancellation_requested {
            return Err(PluginLifecycleError::CancellationNotRequested);
        }
        if !result.confirmed {
            return Ok(None);
        }
        let terminal = PluginInvocationResultV1 {
            invocation_id: result.invocation_id.clone(),
            status: PluginTerminalStatusV1::Cancelled,
            effect: result.effect,
            output: None,
            error: None,
        };
        self.finish_invocation(&terminal).map(Some)
    }

    pub fn finish_invocation(
        &mut self,
        result: &PluginInvocationResultV1,
    ) -> Result<PluginInvocationSettlementV1, PluginLifecycleError> {
        let active = self.take_active(&result.invocation_id)?;
        let evidence = terminal_evidence(active.phase, result.status, result.effect);
        let settlement = settlement(result.invocation_id.clone(), evidence, None);
        self.state = PluginLifecycleStateV1::Healthy;
        self.state_reason = None;
        self.settlements
            .insert(result.invocation_id.as_str().to_owned(), settlement.clone());
        Ok(settlement)
    }

    /// Records exact process-loss evidence. Ambiguous dispatch is terminally
    /// uncertain and blocks both restart and replay until explicit repair.
    pub fn process_crashed(
        &mut self,
        now_millis: u64,
        reason: impl Into<String>,
    ) -> Result<Option<PluginInvocationSettlementV1>, PluginLifecycleError> {
        validate_now(now_millis)?;
        let reason = bounded_reason(reason.into());
        if let Some(active) = self.active.take() {
            let dispatch = match active.phase {
                PluginDispatchPhaseV1::Prepared => DispatchEvidenceV1::DefinitelyNotStarted,
                PluginDispatchPhaseV1::Sent => DispatchEvidenceV1::Unknown,
                PluginDispatchPhaseV1::Accepted | PluginDispatchPhaseV1::EffectMayHaveStarted => {
                    DispatchEvidenceV1::Started
                }
            };
            let evidence = EffectEvidenceV1 {
                dispatch,
                terminal: TerminalEvidenceV1::MissingOrConflicting,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            };
            let value = settlement(active.invocation_id.clone(), evidence, Some(reason.clone()));
            self.settlements
                .insert(active.invocation_id.as_str().to_owned(), value.clone());
            if value.outcome.disposition == OutcomeDispositionV1::FailedDefiniteNotStarted {
                self.schedule_restart(now_millis, reason)?;
            } else {
                self.state = PluginLifecycleStateV1::OutcomeUncertain;
                self.state_reason = Some(reason);
                self.restart_not_before_millis = None;
            }
            return Ok(Some(value));
        }
        match self.state {
            PluginLifecycleStateV1::Launching
            | PluginLifecycleStateV1::Healthy
            | PluginLifecycleStateV1::ShuttingDown => {
                self.schedule_restart(now_millis, reason)?;
                Ok(None)
            }
            _ => Err(PluginLifecycleError::InvalidState),
        }
    }

    pub fn observe_health(
        &mut self,
        result: &PluginHealthResultV1,
        now_millis: u64,
    ) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::Healthy)?;
        match result.status {
            PluginHealthStatusV1::Healthy => Ok(()),
            PluginHealthStatusV1::Degraded | PluginHealthStatusV1::Unhealthy => self
                .schedule_restart(
                    now_millis,
                    result
                        .detail
                        .clone()
                        .unwrap_or_else(|| "plugin health check failed".to_owned()),
                ),
        }
    }

    /// Any exact-pin change quarantines this lifecycle. A running invocation is
    /// settled conservatively and is never transferred to updated code.
    pub fn observe_identity(
        &mut self,
        extension_id: &StableId,
        version: &str,
        content_hash: &str,
        host_generation: ProcessGeneration,
    ) -> Result<Option<PluginInvocationSettlementV1>, PluginLifecycleError> {
        let unchanged = extension_id == &self.pinned.pin().extension_id
            && version == self.pinned.pin().version
            && content_hash == self.pinned.pin().content_hash
            && host_generation == self.pinned.host_generation();
        if unchanged {
            return Ok(None);
        }
        let reason = "plugin identity, version, hash, or host generation drifted".to_owned();
        let settlement = self.active.take().map(|active| {
            let dispatch = match active.phase {
                PluginDispatchPhaseV1::Prepared => DispatchEvidenceV1::DefinitelyNotStarted,
                PluginDispatchPhaseV1::Sent => DispatchEvidenceV1::Unknown,
                PluginDispatchPhaseV1::Accepted | PluginDispatchPhaseV1::EffectMayHaveStarted => {
                    DispatchEvidenceV1::Started
                }
            };
            let value = settlement(
                active.invocation_id.clone(),
                EffectEvidenceV1 {
                    dispatch,
                    terminal: TerminalEvidenceV1::MissingOrConflicting,
                    descriptor_is_idempotent: false,
                    host_guarantees_same_id_deduplication: false,
                },
                Some(reason.clone()),
            );
            self.settlements
                .insert(active.invocation_id.as_str().to_owned(), value.clone());
            value
        });
        self.quarantine(reason);
        Ok(settlement)
    }

    pub fn disable(&mut self, reason: impl Into<String>) -> Option<PluginInvocationSettlementV1> {
        let reason = bounded_reason(reason.into());
        let value = self.active.take().map(|active| {
            let dispatch = match active.phase {
                PluginDispatchPhaseV1::Prepared => DispatchEvidenceV1::DefinitelyNotStarted,
                PluginDispatchPhaseV1::Sent => DispatchEvidenceV1::Unknown,
                PluginDispatchPhaseV1::Accepted | PluginDispatchPhaseV1::EffectMayHaveStarted => {
                    DispatchEvidenceV1::Started
                }
            };
            let value = settlement(
                active.invocation_id.clone(),
                EffectEvidenceV1 {
                    dispatch,
                    terminal: TerminalEvidenceV1::MissingOrConflicting,
                    descriptor_is_idempotent: false,
                    host_guarantees_same_id_deduplication: false,
                },
                Some(reason.clone()),
            );
            self.settlements
                .insert(active.invocation_id.as_str().to_owned(), value.clone());
            value
        });
        self.state = PluginLifecycleStateV1::Disabled;
        self.restart_not_before_millis = None;
        self.state_reason = Some(reason);
        value
    }

    pub fn quarantine_uncertain(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::OutcomeUncertain)?;
        self.quarantine(reason);
        Ok(())
    }

    pub fn begin_shutdown(&mut self) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::Healthy)?;
        self.state = PluginLifecycleStateV1::ShuttingDown;
        Ok(())
    }

    pub fn complete_shutdown(&mut self, clean: bool) -> Result<(), PluginLifecycleError> {
        self.require_state(PluginLifecycleStateV1::ShuttingDown)?;
        if clean {
            self.state = PluginLifecycleStateV1::Stopped;
            self.state_reason = None;
            Ok(())
        } else {
            self.quarantine("plugin shutdown or descendant cleanup was not confirmed");
            Err(PluginLifecycleError::UncleanShutdown)
        }
    }

    fn active_ref(
        &self,
        invocation_id: &StableId,
    ) -> Result<&ActiveInvocationV1, PluginLifecycleError> {
        let active = self
            .active
            .as_ref()
            .ok_or(PluginLifecycleError::NoActiveInvocation)?;
        if active.invocation_id != *invocation_id {
            return Err(PluginLifecycleError::InvocationIdentityMismatch);
        }
        Ok(active)
    }

    fn active_mut(
        &mut self,
        invocation_id: &StableId,
    ) -> Result<&mut ActiveInvocationV1, PluginLifecycleError> {
        let active = self
            .active
            .as_mut()
            .ok_or(PluginLifecycleError::NoActiveInvocation)?;
        if active.invocation_id != *invocation_id {
            return Err(PluginLifecycleError::InvocationIdentityMismatch);
        }
        Ok(active)
    }

    fn take_active(
        &mut self,
        invocation_id: &StableId,
    ) -> Result<ActiveInvocationV1, PluginLifecycleError> {
        self.active_ref(invocation_id)?;
        self.active
            .take()
            .ok_or(PluginLifecycleError::NoActiveInvocation)
    }

    fn require_state(&self, expected: PluginLifecycleStateV1) -> Result<(), PluginLifecycleError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(PluginLifecycleError::InvalidState)
        }
    }

    fn schedule_restart(
        &mut self,
        now_millis: u64,
        reason: impl Into<String>,
    ) -> Result<(), PluginLifecycleError> {
        validate_now(now_millis)?;
        let reason = bounded_reason(reason.into());
        if self.restart_attempts >= self.restart_policy.maximum_restart_attempts {
            self.quarantine(format!("plugin restart budget exhausted: {reason}"));
            return Ok(());
        }
        let shift = self.restart_attempts.min(63);
        let factor = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay = self
            .restart_policy
            .initial_backoff_millis
            .saturating_mul(factor)
            .min(self.restart_policy.maximum_backoff_millis);
        let not_before = now_millis
            .checked_add(delay)
            .filter(|value| *value <= MAX_SAFE_WIRE_INTEGER)
            .ok_or(PluginLifecycleError::InvalidClock)?;
        self.restart_attempts = self.restart_attempts.saturating_add(1);
        self.restart_not_before_millis = Some(not_before);
        self.state = PluginLifecycleStateV1::RestartBackoff;
        self.state_reason = Some(reason);
        Ok(())
    }

    fn quarantine(&mut self, reason: impl Into<String>) {
        self.state = PluginLifecycleStateV1::Quarantined;
        self.restart_not_before_millis = None;
        self.state_reason = Some(bounded_reason(reason.into()));
    }
}

fn terminal_evidence(
    phase: PluginDispatchPhaseV1,
    status: PluginTerminalStatusV1,
    effect: PluginEffectStatusV1,
) -> EffectEvidenceV1 {
    let phase_dispatch = match phase {
        PluginDispatchPhaseV1::Prepared => DispatchEvidenceV1::DefinitelyNotStarted,
        PluginDispatchPhaseV1::Sent => DispatchEvidenceV1::Unknown,
        PluginDispatchPhaseV1::Accepted | PluginDispatchPhaseV1::EffectMayHaveStarted => {
            DispatchEvidenceV1::Started
        }
    };
    let dispatch = match (phase_dispatch, effect) {
        (DispatchEvidenceV1::DefinitelyNotStarted, PluginEffectStatusV1::DefinitelyNotStarted) => {
            DispatchEvidenceV1::DefinitelyNotStarted
        }
        (DispatchEvidenceV1::Started, PluginEffectStatusV1::Started) => DispatchEvidenceV1::Started,
        (DispatchEvidenceV1::Unknown, PluginEffectStatusV1::DefinitelyNotStarted) => {
            DispatchEvidenceV1::DefinitelyNotStarted
        }
        (DispatchEvidenceV1::Unknown, PluginEffectStatusV1::Started) => DispatchEvidenceV1::Started,
        (_, PluginEffectStatusV1::Unknown) => DispatchEvidenceV1::Unknown,
        _ => DispatchEvidenceV1::Unknown,
    };
    let terminal = match status {
        PluginTerminalStatusV1::Succeeded
            if phase != PluginDispatchPhaseV1::Prepared
                && effect != PluginEffectStatusV1::DefinitelyNotStarted =>
        {
            TerminalEvidenceV1::Succeeded
        }
        PluginTerminalStatusV1::Succeeded => TerminalEvidenceV1::MissingOrConflicting,
        PluginTerminalStatusV1::Failed => TerminalEvidenceV1::Failed,
        PluginTerminalStatusV1::Cancelled
            if effect == PluginEffectStatusV1::DefinitelyNotStarted =>
        {
            TerminalEvidenceV1::Failed
        }
        PluginTerminalStatusV1::Cancelled if effect == PluginEffectStatusV1::Started => {
            TerminalEvidenceV1::CancelledWithEvidence
        }
        PluginTerminalStatusV1::Cancelled => TerminalEvidenceV1::MissingOrConflicting,
    };
    EffectEvidenceV1 {
        dispatch,
        terminal,
        descriptor_is_idempotent: false,
        host_guarantees_same_id_deduplication: false,
    }
}

fn settlement(
    invocation_id: StableId,
    evidence: EffectEvidenceV1,
    reason: Option<String>,
) -> PluginInvocationSettlementV1 {
    let mut outcome = classify_outcome(invocation_id, evidence);
    // Plugin lifecycle never claims an adapter-level same-ID deduplication
    // guarantee, even if a malformed future classifier were to infer one.
    if outcome.disposition == OutcomeDispositionV1::OutcomeUncertain {
        outcome.retry_safety = RetrySafetyV1::NotSafe;
    }
    let replay = if outcome.disposition == OutcomeDispositionV1::FailedDefiniteNotStarted {
        PluginReplayDispositionV1::CoreMayCreateNewAttempt
    } else {
        PluginReplayDispositionV1::NeverReplay
    };
    PluginInvocationSettlementV1 {
        outcome,
        replay,
        reason,
    }
}

fn validate_now(now_millis: u64) -> Result<(), PluginLifecycleError> {
    if now_millis > MAX_SAFE_WIRE_INTEGER {
        Err(PluginLifecycleError::InvalidClock)
    } else {
        Ok(())
    }
}

fn bounded_reason(mut reason: String) -> String {
    const MAXIMUM_REASON_BYTES: usize = 4096;
    if reason.is_empty() {
        return "plugin lifecycle failure".to_owned();
    }
    if reason.len() > MAXIMUM_REASON_BYTES {
        let mut boundary = MAXIMUM_REASON_BYTES;
        while !reason.is_char_boundary(boundary) {
            boundary -= 1;
        }
        reason.truncate(boundary);
    }
    reason
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PluginLifecycleError {
    #[error("plugin restart policy is invalid")]
    InvalidRestartPolicy,
    #[error("plugin lifecycle limits are invalid")]
    InvalidLifecycleLimits,
    #[error("plugin lifecycle transition is invalid for its current state")]
    InvalidState,
    #[error("plugin handshake was rejected")]
    HandshakeRejected,
    #[error("plugin handshake drifted from the exact core pin")]
    HandshakeDrift,
    #[error("plugin restart backoff has not elapsed")]
    RestartBackoffActive,
    #[error("plugin lifecycle clock is outside the exact wire range")]
    InvalidClock,
    #[error("plugin contribution is not in the exact core pin")]
    ContributionNotPinned,
    #[error("plugin invocation was already settled and cannot be replayed")]
    InvocationAlreadySettled,
    #[error("plugin settlement retention capacity is exhausted")]
    SettlementCapacityExhausted,
    #[error("plugin has no active invocation")]
    NoActiveInvocation,
    #[error("plugin invocation identity does not match the active invocation")]
    InvocationIdentityMismatch,
    #[error("plugin invocation phase transition is invalid")]
    InvalidInvocationPhase,
    #[error("plugin invocation event is out of order or arrived before dispatch")]
    InvalidInvocationEvent,
    #[error("plugin cancellation result arrived without a cancellation request")]
    CancellationNotRequested,
    #[error("plugin shutdown or descendant cleanup was not confirmed")]
    UncleanShutdown,
}
