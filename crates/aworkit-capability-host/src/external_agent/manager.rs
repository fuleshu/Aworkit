//! Capability-negotiated external-agent session and approval coordination.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use aworkit_protocol::{ProcessGeneration, StableId};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    DispatchEvidenceV1, EffectEvidenceV1, TerminalEvidenceV1, classify_outcome,
    external_agent::contracts::{
        ExternalAgentContinueV1, ExternalAgentHealthV1, ExternalAgentManifestV1,
        ExternalAgentNegotiationV1, ExternalAgentPeerErrorV1, ExternalAgentPeerPort,
        ExternalAgentPeerUpdateV1, ExternalAgentRawContentV1, ExternalAgentStartV1,
        ExternalAgentUpdateV1, ExternalAgentVisibilityV1, ExternalApprovalDecisionV1,
        ExternalApprovalRequestV1, ExternalApprovalResolutionV1, ExternalCancellationEvidenceV1,
        ExternalDispatchMilestoneV1, ExternalEffectEvidenceV1, ExternalTerminalStatusV1,
        NativeSessionRefV1,
    },
};

const MAX_TARGETS: usize = 64;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_LEASES: usize = 64;
const MAX_RETIRED_INVOCATIONS: usize = 4096;

#[derive(Clone, Copy, Eq, PartialEq)]
enum ControlReservation {
    Approval,
    Cancellation,
}

#[derive(Clone, Copy)]
enum ExistingResponseSource {
    Invocation,
    Approval,
    Cancellation,
}

#[derive(Clone)]
struct SessionState {
    reference: NativeSessionRefV1,
    native_session_id: String,
    continuation_cursor: Option<String>,
    pending_approval: Option<ExternalApprovalRequestV1>,
    active_invocation: Option<StableId>,
    retired_invocations: BTreeSet<StableId>,
    retired_invocations_exhausted: bool,
    control_in_flight: Option<ControlReservation>,
    continuation_blocked: bool,
    closing: bool,
}

struct TargetState {
    manifest: ExternalAgentManifestV1,
    negotiation: Option<ExternalAgentNegotiationV1>,
    sessions: BTreeMap<String, SessionState>,
    pending_starts: BTreeSet<StableId>,
    degraded: bool,
}

/// Owns exact target negotiation and opaque native-session correlation for one
/// authenticated capability-host generation.
pub struct ExternalAgentManager {
    generation: ProcessGeneration,
    peer: Arc<dyn ExternalAgentPeerPort>,
    core_authentication_key: Zeroizing<Vec<u8>>,
    targets: Mutex<BTreeMap<String, TargetState>>,
}

impl ExternalAgentManager {
    pub fn new(
        generation: ProcessGeneration,
        peer: Arc<dyn ExternalAgentPeerPort>,
        core_authentication_key: Vec<u8>,
    ) -> Result<Self, ExternalAgentError> {
        if core_authentication_key.len() < 32 {
            return Err(ExternalAgentError::InvalidAuthenticationKey);
        }
        Ok(Self {
            generation,
            peer,
            core_authentication_key: Zeroizing::new(core_authentication_key),
            targets: Mutex::new(BTreeMap::new()),
        })
    }

    /// Negotiates an exact configured target once. Changed bindings require a
    /// new host generation rather than live substitution.
    pub fn register_target(
        &self,
        manifest: ExternalAgentManifestV1,
    ) -> Result<ExternalAgentNegotiationV1, ExternalAgentError> {
        validate_manifest(&manifest, self.generation)?;
        let key = manifest.target_id.as_str().to_owned();
        {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            if let Some(existing) = targets.get(&key) {
                if existing.manifest != manifest || existing.degraded {
                    return Err(ExternalAgentError::BindingDrift);
                }
                return existing
                    .negotiation
                    .clone()
                    .ok_or(ExternalAgentError::TargetRegistrationInProgress);
            }
            if targets.len() >= MAX_TARGETS {
                return Err(ExternalAgentError::TargetLimit);
            }
            targets.insert(
                key.clone(),
                TargetState {
                    manifest: manifest.clone(),
                    negotiation: None,
                    sessions: BTreeMap::new(),
                    pending_starts: BTreeSet::new(),
                    degraded: false,
                },
            );
        }
        let negotiation = match self.peer.negotiate(&manifest) {
            Ok(negotiation) => negotiation,
            Err(error) => {
                self.clear_registration_reservation(&key);
                return Err(ExternalAgentError::Peer(error));
            }
        };
        if let Err(error) = validate_negotiation(&manifest, &negotiation) {
            self.clear_registration_reservation(&key);
            return Err(error);
        }
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| ExternalAgentError::Poisoned)?;
        let target = targets
            .get_mut(&key)
            .ok_or(ExternalAgentError::TargetRegistrationConflict)?;
        if target.manifest != manifest || target.negotiation.is_some() {
            return Err(ExternalAgentError::TargetRegistrationConflict);
        }
        target.negotiation = Some(negotiation.clone());
        Ok(negotiation)
    }

    /// Starts only the explicit target in the approved request. The adapter
    /// cannot select a fallback, workspace, credential, or MCP server itself.
    pub fn start(
        &self,
        target_id: &StableId,
        request: &ExternalAgentStartV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        let (manifest, negotiation) = {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            let target = targets
                .get_mut(target_id.as_str())
                .ok_or(ExternalAgentError::UnknownTarget)?;
            if target.degraded {
                return Err(ExternalAgentError::TargetDegraded);
            }
            let negotiation = target
                .negotiation
                .clone()
                .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
            if target.sessions.len() + target.pending_starts.len()
                >= target.manifest.maximum_active_sessions
            {
                return Err(ExternalAgentError::SessionLimit);
            }
            if target.pending_starts.contains(&request.invocation_id) {
                return Err(ExternalAgentError::InvocationOverlap);
            }
            validate_start(request, target)?;
            target.pending_starts.insert(request.invocation_id.clone());
            (target.manifest.clone(), negotiation)
        };
        let update = match self.peer.start(&manifest, request) {
            Ok(update) => update,
            Err(error) => {
                return self.peer_failure(target_id, request.invocation_id.clone(), error);
            }
        };
        self.accept_new_session(&manifest, &negotiation, request, update)
    }

    /// Continues only the retained exact native session and expected cursor.
    pub fn continue_session(
        &self,
        request: &ExternalAgentContinueV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        let (manifest, negotiation, native_id) = {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            let target = targets
                .get_mut(request.native_session.target_id.as_str())
                .ok_or(ExternalAgentError::UnknownTarget)?;
            if target.degraded {
                return Err(ExternalAgentError::TargetDegraded);
            }
            let negotiation = target
                .negotiation
                .clone()
                .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
            if !negotiation.capabilities.continuation {
                return Err(ExternalAgentError::ContinuationUnsupported);
            }
            validate_forwarding(request.forwarded_mcp.as_ref(), target)?;
            let session = target
                .sessions
                .get_mut(&request.native_session.reference_hash)
                .ok_or(ExternalAgentError::UnknownSession)?;
            validate_session_ref(&request.native_session, session, self.generation)?;
            if session.closing {
                return Err(ExternalAgentError::SessionClosing);
            }
            if session.retired_invocations_exhausted {
                return Err(ExternalAgentError::RetiredInvocationCapacity);
            }
            if session.continuation_blocked {
                return Err(ExternalAgentError::ContinuationBlocked);
            }
            if session.active_invocation.is_some() {
                return Err(ExternalAgentError::InvocationOverlap);
            }
            if session.control_in_flight.is_some() {
                return Err(ExternalAgentError::ControlInFlight);
            }
            if session.pending_approval.is_some() {
                return Err(ExternalAgentError::ApprovalPending);
            }
            if session.retired_invocations.contains(&request.invocation_id) {
                return Err(ExternalAgentError::InvocationTerminal);
            }
            if request.expected_cursor != session.continuation_cursor {
                return Err(ExternalAgentError::ContinuationCursorDrift);
            }
            validate_text(&request.input)?;
            if request.deadline_epoch_millis == 0 {
                return Err(ExternalAgentError::InvalidDeadline);
            }
            session.active_invocation = Some(request.invocation_id.clone());
            (
                target.manifest.clone(),
                negotiation,
                session.native_session_id.clone(),
            )
        };
        let update = match self.peer.continue_session(&manifest, &native_id, request) {
            Ok(update) => update,
            Err(error) => {
                return self.peer_failure_existing(
                    &request.native_session,
                    request.invocation_id.clone(),
                    ExistingResponseSource::Invocation,
                    error,
                );
            }
        };
        self.accept_existing_update(
            &manifest,
            &negotiation,
            &request.native_session,
            request.invocation_id.clone(),
            ExistingResponseSource::Invocation,
            update,
        )
    }

    /// Forwards a generation-fenced core decision. The requested scope is an
    /// upper bound and an external agent never self-approves.
    pub fn resolve_approval(
        &self,
        resolution: &ExternalApprovalResolutionV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        resolution
            .verify(&self.core_authentication_key)
            .map_err(|_| ExternalAgentError::ApprovalAuthentication)?;
        let native_session = resolution.native_session();
        let (manifest, negotiation, native_id, invocation_id) = {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            let target = targets
                .get_mut(native_session.target_id.as_str())
                .ok_or(ExternalAgentError::UnknownTarget)?;
            let negotiation = target
                .negotiation
                .clone()
                .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
            if !negotiation.capabilities.approval_requests {
                return Err(ExternalAgentError::ApprovalsUnsupported);
            }
            let session = target
                .sessions
                .get_mut(&native_session.reference_hash)
                .ok_or(ExternalAgentError::UnknownSession)?;
            validate_session_ref(native_session, session, self.generation)?;
            if session.closing {
                return Err(ExternalAgentError::SessionClosing);
            }
            if session.control_in_flight.is_some() {
                return Err(ExternalAgentError::ControlInFlight);
            }
            let active = session
                .active_invocation
                .as_ref()
                .ok_or(ExternalAgentError::NoActiveInvocation)?;
            if active != resolution.invocation_id() {
                return Err(ExternalAgentError::InvocationCorrelationConflict);
            }
            let pending = session
                .pending_approval
                .as_ref()
                .ok_or(ExternalAgentError::NoApprovalPending)?;
            validate_resolution(pending, resolution, self.generation)?;
            session.control_in_flight = Some(ControlReservation::Approval);
            (
                target.manifest.clone(),
                negotiation,
                session.native_session_id.clone(),
                active.clone(),
            )
        };
        let update = match self
            .peer
            .resolve_approval(&manifest, &native_id, resolution)
        {
            Ok(update) => update,
            Err(error) => {
                return self.peer_failure_existing(
                    native_session,
                    invocation_id,
                    ExistingResponseSource::Approval,
                    error,
                );
            }
        };
        self.accept_existing_update(
            &manifest,
            &negotiation,
            native_session,
            invocation_id,
            ExistingResponseSource::Approval,
            update,
        )
    }

    /// Cancels over a reserved control path. Process death or refusal alone is
    /// not proof that remote work or effects stopped.
    pub fn cancel(
        &self,
        native_session: &NativeSessionRefV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        let (manifest, negotiation, native_id, invocation_id, continuation_cursor) = {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            let target = targets
                .get_mut(native_session.target_id.as_str())
                .ok_or(ExternalAgentError::UnknownTarget)?;
            let negotiation = target
                .negotiation
                .clone()
                .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
            let session = target
                .sessions
                .get_mut(&native_session.reference_hash)
                .ok_or(ExternalAgentError::UnknownSession)?;
            validate_session_ref(native_session, session, self.generation)?;
            if session.closing {
                return Err(ExternalAgentError::SessionClosing);
            }
            if session.control_in_flight.is_some() {
                return Err(ExternalAgentError::ControlInFlight);
            }
            let invocation_id = session
                .active_invocation
                .clone()
                .ok_or(ExternalAgentError::NoActiveInvocation)?;
            session.control_in_flight = Some(ControlReservation::Cancellation);
            (
                target.manifest.clone(),
                negotiation,
                session.native_session_id.clone(),
                invocation_id,
                session.continuation_cursor.clone(),
            )
        };
        let evidence = if negotiation.capabilities.cancellation {
            self.peer.cancel(&manifest, &native_id, &invocation_id)
        } else {
            Ok(ExternalCancellationEvidenceV1::Unsupported)
        };
        let (dispatch, terminal, degraded) = match evidence {
            Ok(ExternalCancellationEvidenceV1::ConfirmedBeforeEffect) => (
                DispatchEvidenceV1::DefinitelyNotStarted,
                TerminalEvidenceV1::Failed,
                false,
            ),
            Ok(ExternalCancellationEvidenceV1::ConfirmedAfterStart) => (
                DispatchEvidenceV1::Started,
                TerminalEvidenceV1::CancelledWithEvidence,
                false,
            ),
            Ok(
                ExternalCancellationEvidenceV1::Refused
                | ExternalCancellationEvidenceV1::Unsupported
                | ExternalCancellationEvidenceV1::Unknown,
            ) => (
                DispatchEvidenceV1::Unknown,
                TerminalEvidenceV1::MissingOrConflicting,
                false,
            ),
            Err(error) => (
                map_dispatch(error.dispatch),
                TerminalEvidenceV1::MissingOrConflicting,
                error.native_session_lost,
            ),
        };
        let outcome = classify_outcome(
            invocation_id.clone(),
            EffectEvidenceV1 {
                dispatch,
                terminal,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        );
        self.fence_after_dispatch(
            native_session,
            &invocation_id,
            ExistingResponseSource::Cancellation,
            outcome.disposition == crate::OutcomeDispositionV1::OutcomeUncertain,
            degraded,
        )?;
        Ok(ExternalAgentUpdateV1 {
            native_session: native_session.clone(),
            continuation_cursor,
            events: Vec::new(),
            approval_request: None,
            terminal: Some(outcome),
            result: None,
            visibility: negotiation.visibility,
        })
    }

    /// Closes and evicts only a terminal-fenced native session. Active work is
    /// never discarded merely to free local capacity.
    pub fn close_session(
        &self,
        native_session: &NativeSessionRefV1,
    ) -> Result<(), ExternalAgentError> {
        let (manifest, native_id) = {
            let mut targets = self
                .targets
                .lock()
                .map_err(|_| ExternalAgentError::Poisoned)?;
            let target = targets
                .get_mut(native_session.target_id.as_str())
                .ok_or(ExternalAgentError::UnknownTarget)?;
            let session = target
                .sessions
                .get_mut(&native_session.reference_hash)
                .ok_or(ExternalAgentError::UnknownSession)?;
            validate_session_ref(native_session, session, self.generation)?;
            if session.active_invocation.is_some()
                || session.pending_approval.is_some()
                || session.control_in_flight.is_some()
            {
                return Err(ExternalAgentError::SessionBusy);
            }
            if session.closing {
                return Err(ExternalAgentError::SessionClosing);
            }
            session.closing = true;
            (target.manifest.clone(), session.native_session_id.clone())
        };
        if let Err(error) = self.peer.close_session(&manifest, &native_id) {
            if let Ok(mut targets) = self.targets.lock()
                && let Some(target) = targets.get_mut(native_session.target_id.as_str())
                && let Some(session) = target.sessions.get_mut(&native_session.reference_hash)
            {
                session.closing = false;
            }
            return Err(ExternalAgentError::Peer(error));
        }
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| ExternalAgentError::Poisoned)?;
        let target = targets
            .get_mut(native_session.target_id.as_str())
            .ok_or(ExternalAgentError::UnknownTarget)?;
        let session = target
            .sessions
            .get(&native_session.reference_hash)
            .ok_or(ExternalAgentError::UnknownSession)?;
        if !session.closing || session.active_invocation.is_some() {
            return Err(ExternalAgentError::SessionBusy);
        }
        target.sessions.remove(&native_session.reference_hash);
        Ok(())
    }

    pub fn health(
        &self,
        target_id: &StableId,
    ) -> Result<ExternalAgentHealthV1, ExternalAgentError> {
        let targets = self
            .targets
            .lock()
            .map_err(|_| ExternalAgentError::Poisoned)?;
        let target = targets
            .get(target_id.as_str())
            .ok_or(ExternalAgentError::UnknownTarget)?;
        let negotiation = target
            .negotiation
            .as_ref()
            .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
        Ok(ExternalAgentHealthV1 {
            target_id: target_id.clone(),
            host_generation: target.manifest.host_generation,
            active_sessions: target.sessions.len() + target.pending_starts.len(),
            reserved_sessions: target.pending_starts.len(),
            sessions_requiring_close: target
                .sessions
                .values()
                .filter(|session| session.retired_invocations_exhausted)
                .count(),
            maximum_active_sessions: target.manifest.maximum_active_sessions,
            degraded: target.degraded,
            capabilities: negotiation.capabilities.clone(),
            visibility: negotiation.visibility,
        })
    }

    fn accept_new_session(
        &self,
        manifest: &ExternalAgentManifestV1,
        negotiation: &ExternalAgentNegotiationV1,
        request: &ExternalAgentStartV1,
        update: ExternalAgentPeerUpdateV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        if let Err(error) = validate_update(
            &update,
            negotiation,
            manifest.maximum_progress_events,
            &request.invocation_id,
        ) {
            return self.post_dispatch_protocol_failure(
                &manifest.target_id,
                request.invocation_id.clone(),
                error,
            );
        }
        let reference = native_reference(
            &manifest.target_id,
            manifest.host_generation,
            &update.native_session_id,
        );
        let public = public_update(
            reference.clone(),
            negotiation.visibility,
            request.invocation_id.clone(),
            &update,
        );
        let active_invocation = update
            .terminal
            .is_none()
            .then(|| request.invocation_id.clone());
        let retired_invocations = update
            .terminal
            .is_some()
            .then(|| request.invocation_id.clone())
            .into_iter()
            .collect();
        let continuation_blocked = public.terminal.as_ref().is_some_and(|outcome| {
            outcome.disposition == crate::OutcomeDispositionV1::OutcomeUncertain
        });
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(target) = targets.get_mut(manifest.target_id.as_str()) else {
            return Err(protocol_outcome_error(
                request.invocation_id.clone(),
                ExternalAgentError::UnknownTarget,
            ));
        };
        if !target.pending_starts.remove(&request.invocation_id) {
            target.degraded = true;
            return Err(protocol_outcome_error(
                request.invocation_id.clone(),
                ExternalAgentError::StartReservationMissing,
            ));
        }
        if target.sessions.contains_key(&reference.reference_hash) {
            target.degraded = true;
            return Err(protocol_outcome_error(
                request.invocation_id.clone(),
                ExternalAgentError::NativeSessionConflict,
            ));
        }
        target.sessions.insert(
            reference.reference_hash.clone(),
            SessionState {
                reference,
                native_session_id: update.native_session_id,
                continuation_cursor: update.continuation_cursor,
                pending_approval: update.approval_request,
                active_invocation,
                retired_invocations,
                retired_invocations_exhausted: false,
                control_in_flight: None,
                continuation_blocked,
                closing: false,
            },
        );
        Ok(public)
    }

    fn accept_existing_update(
        &self,
        manifest: &ExternalAgentManifestV1,
        negotiation: &ExternalAgentNegotiationV1,
        reference: &NativeSessionRefV1,
        invocation_id: StableId,
        source: ExistingResponseSource,
        update: ExternalAgentPeerUpdateV1,
    ) -> Result<ExternalAgentUpdateV1, ExternalAgentError> {
        if let Err(error) = validate_update(
            &update,
            negotiation,
            manifest.maximum_progress_events,
            &invocation_id,
        ) {
            return self.post_dispatch_protocol_failure_existing(
                reference,
                invocation_id,
                source,
                error,
            );
        }
        let public = public_update(
            reference.clone(),
            negotiation.visibility,
            invocation_id.clone(),
            &update,
        );
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(target) = targets.get_mut(manifest.target_id.as_str()) else {
            return Err(protocol_outcome_error(
                invocation_id,
                ExternalAgentError::UnknownTarget,
            ));
        };
        let Some(session) = target.sessions.get_mut(&reference.reference_hash) else {
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id,
                ExternalAgentError::UnknownSession,
            ));
        };
        if let Some(error) = response_blocker(session, &invocation_id, source) {
            return Err(error);
        }
        if session.active_invocation.as_ref() != Some(&invocation_id) {
            retire_invocation(session, &invocation_id);
            session.continuation_blocked = true;
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id,
                ExternalAgentError::InvocationCorrelationConflict,
            ));
        }
        if update.native_session_id != session.native_session_id {
            session.active_invocation = None;
            session.pending_approval = None;
            retire_invocation(session, &invocation_id);
            session.control_in_flight = None;
            session.continuation_blocked = true;
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id,
                ExternalAgentError::NativeSessionDrift,
            ));
        }
        session.continuation_cursor = update.continuation_cursor;
        session.pending_approval = update.approval_request;
        session.control_in_flight = None;
        if public.terminal.is_some() {
            session.active_invocation = None;
            session.pending_approval = None;
            retire_invocation(session, &invocation_id);
            session.continuation_blocked |= public.terminal.as_ref().is_some_and(|outcome| {
                outcome.disposition == crate::OutcomeDispositionV1::OutcomeUncertain
            });
        }
        Ok(public)
    }

    fn peer_failure<T>(
        &self,
        target_id: &StableId,
        invocation_id: StableId,
        error: ExternalAgentPeerErrorV1,
    ) -> Result<T, ExternalAgentError> {
        self.settle_start_reservation_after_dispatch(
            target_id,
            &invocation_id,
            error.native_session_lost
                || error.dispatch != ExternalDispatchMilestoneV1::DefinitelyNotStarted,
        );
        Err(ExternalAgentError::PeerOutcome {
            outcome: classify_outcome(
                invocation_id,
                EffectEvidenceV1 {
                    dispatch: map_dispatch(error.dispatch),
                    terminal: TerminalEvidenceV1::MissingOrConflicting,
                    descriptor_is_idempotent: false,
                    host_guarantees_same_id_deduplication: false,
                },
            ),
            code: error.code,
            native_session_lost: error.native_session_lost,
        })
    }

    fn peer_failure_existing<T>(
        &self,
        native_session: &NativeSessionRefV1,
        invocation_id: StableId,
        source: ExistingResponseSource,
        error: ExternalAgentPeerErrorV1,
    ) -> Result<T, ExternalAgentError> {
        let outcome = classify_outcome(
            invocation_id.clone(),
            EffectEvidenceV1 {
                dispatch: map_dispatch(error.dispatch),
                terminal: TerminalEvidenceV1::MissingOrConflicting,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        );
        self.fence_after_dispatch(
            native_session,
            &invocation_id,
            source,
            outcome.disposition == crate::OutcomeDispositionV1::OutcomeUncertain,
            error.native_session_lost,
        )?;
        Err(ExternalAgentError::PeerOutcome {
            outcome,
            code: error.code,
            native_session_lost: error.native_session_lost,
        })
    }

    fn post_dispatch_protocol_failure<T>(
        &self,
        target_id: &StableId,
        invocation_id: StableId,
        error: ExternalAgentError,
    ) -> Result<T, ExternalAgentError> {
        self.settle_start_reservation_after_dispatch(target_id, &invocation_id, true);
        Err(protocol_outcome_error(invocation_id, error))
    }

    fn post_dispatch_protocol_failure_existing<T>(
        &self,
        native_session: &NativeSessionRefV1,
        invocation_id: StableId,
        source: ExistingResponseSource,
        error: ExternalAgentError,
    ) -> Result<T, ExternalAgentError> {
        self.fence_after_dispatch(native_session, &invocation_id, source, true, true)?;
        Err(protocol_outcome_error(invocation_id, error))
    }

    fn fence_after_dispatch(
        &self,
        native_session: &NativeSessionRefV1,
        invocation_id: &StableId,
        source: ExistingResponseSource,
        continuation_blocked: bool,
        degraded: bool,
    ) -> Result<(), ExternalAgentError> {
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(target) = targets.get_mut(native_session.target_id.as_str()) else {
            return Err(protocol_outcome_error(
                invocation_id.clone(),
                ExternalAgentError::UnknownTarget,
            ));
        };
        let Some(session) = target.sessions.get_mut(&native_session.reference_hash) else {
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id.clone(),
                ExternalAgentError::UnknownSession,
            ));
        };
        if validate_session_ref(native_session, session, self.generation).is_err() {
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id.clone(),
                ExternalAgentError::StaleOrChangedSessionRef,
            ));
        }
        if let Some(error) = response_blocker(session, invocation_id, source) {
            return Err(error);
        }
        target.degraded |= degraded;
        retire_invocation(session, invocation_id);
        session.continuation_blocked |= continuation_blocked;
        if session.active_invocation.as_ref() == Some(invocation_id) {
            session.active_invocation = None;
            session.pending_approval = None;
            session.control_in_flight = None;
        } else {
            target.degraded = true;
            return Err(protocol_outcome_error(
                invocation_id.clone(),
                ExternalAgentError::InvocationCorrelationConflict,
            ));
        }
        Ok(())
    }

    fn clear_registration_reservation(&self, target_id: &str) {
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if targets
            .get(target_id)
            .is_some_and(|target| target.negotiation.is_none())
        {
            targets.remove(target_id);
        }
    }

    fn settle_start_reservation_after_dispatch(
        &self,
        target_id: &StableId,
        invocation_id: &StableId,
        degraded: bool,
    ) {
        if let Some(target) = self
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(target_id.as_str())
        {
            target.pending_starts.remove(invocation_id);
            target.degraded |= degraded;
        }
    }
}

fn response_blocker(
    session: &SessionState,
    invocation_id: &StableId,
    source: ExistingResponseSource,
) -> Option<ExternalAgentError> {
    if session.retired_invocations.contains(invocation_id)
        || (session.retired_invocations_exhausted
            && session.active_invocation.as_ref() != Some(invocation_id))
    {
        return Some(ExternalAgentError::InvocationTerminal);
    }
    let owns_response = matches!(
        (source, session.control_in_flight),
        (ExistingResponseSource::Invocation, None)
            | (
                ExistingResponseSource::Approval,
                Some(ControlReservation::Approval)
            )
            | (
                ExistingResponseSource::Cancellation,
                Some(ControlReservation::Cancellation)
            )
    );
    (!owns_response).then_some(ExternalAgentError::ControlInFlight)
}

fn retire_invocation(session: &mut SessionState, invocation_id: &StableId) {
    if session.retired_invocations.contains(invocation_id) {
        return;
    }
    if session.retired_invocations.len() >= MAX_RETIRED_INVOCATIONS {
        session.retired_invocations_exhausted = true;
        session.continuation_blocked = true;
    } else {
        session.retired_invocations.insert(invocation_id.clone());
    }
}

fn validate_manifest(
    manifest: &ExternalAgentManifestV1,
    generation: ProcessGeneration,
) -> Result<(), ExternalAgentError> {
    if !manifest.configured {
        return Err(ExternalAgentError::NotConfigured);
    }
    if !manifest.enabled {
        return Err(ExternalAgentError::Disabled);
    }
    if !manifest.core_attested {
        return Err(ExternalAgentError::Unattested);
    }
    if manifest.host_generation != generation {
        return Err(ExternalAgentError::StaleAttestation);
    }
    if !is_hash(&manifest.binding_hash)
        || manifest.adapter_version.is_empty()
        || manifest.maximum_active_sessions == 0
        || manifest.maximum_progress_events == 0
        || !strictly_sorted_unique(&manifest.allowed_workspace_roots)
        || !strictly_sorted_unique(&manifest.allowed_mcp_server_ids)
        || !strictly_sorted_unique(&manifest.secret_slots)
    {
        return Err(ExternalAgentError::InvalidManifest);
    }
    Ok(())
}

fn validate_negotiation(
    manifest: &ExternalAgentManifestV1,
    negotiation: &ExternalAgentNegotiationV1,
) -> Result<(), ExternalAgentError> {
    if negotiation.target_id != manifest.target_id
        || negotiation.host_generation != manifest.host_generation
    {
        return Err(ExternalAgentError::NegotiationIdentityDrift);
    }
    if negotiation.protocol_version.is_empty()
        || negotiation.protocol_version.len() > 128
        || (negotiation.capabilities.continuation && !negotiation.capabilities.native_sessions)
        || (negotiation.capabilities.approval_requests && !negotiation.capabilities.native_sessions)
    {
        return Err(ExternalAgentError::InvalidNegotiation);
    }
    Ok(())
}

fn validate_start(
    request: &ExternalAgentStartV1,
    target: &TargetState,
) -> Result<(), ExternalAgentError> {
    validate_text(&request.task)?;
    validate_text(&request.desired_result)?;
    if request.deadline_epoch_millis == 0
        || request.maximum_turns == 0
        || request.lease_handles.len() > MAX_LEASES
        || request.lease_handles.len() > target.manifest.secret_slots.len()
        || !strictly_sorted_unique(&request.workspace_roots)
        || !request
            .lease_handles
            .windows(2)
            .all(|pair| pair[0].as_str() < pair[1].as_str())
    {
        return Err(ExternalAgentError::InvalidStart);
    }
    let allowed: BTreeSet<_> = target.manifest.allowed_workspace_roots.iter().collect();
    if !request
        .workspace_roots
        .iter()
        .all(|root| allowed.contains(root))
    {
        return Err(ExternalAgentError::WorkspaceScopeBroadened);
    }
    validate_forwarding(request.forwarded_mcp.as_ref(), target)
}

fn validate_forwarding(
    forwarded: Option<&crate::ForwardableMcpSetV1>,
    target: &TargetState,
) -> Result<(), ExternalAgentError> {
    let Some(forwarded) = forwarded else {
        return Ok(());
    };
    let negotiation = target
        .negotiation
        .as_ref()
        .ok_or(ExternalAgentError::TargetRegistrationInProgress)?;
    if !negotiation.capabilities.selected_mcp_forwarding {
        return Err(ExternalAgentError::McpForwardingUnsupported);
    }
    let allowed: BTreeSet<_> = target
        .manifest
        .allowed_mcp_server_ids
        .iter()
        .map(String::as_str)
        .collect();
    if forwarded
        .servers
        .keys()
        .any(|server_id| !allowed.contains(server_id.as_str()))
    {
        return Err(ExternalAgentError::McpScopeBroadened);
    }
    Ok(())
}

fn validate_update(
    update: &ExternalAgentPeerUpdateV1,
    negotiation: &ExternalAgentNegotiationV1,
    maximum_progress_events: usize,
    expected_invocation_id: &StableId,
) -> Result<(), ExternalAgentError> {
    if &update.invocation_id != expected_invocation_id {
        return Err(ExternalAgentError::InvocationCorrelationConflict);
    }
    if update.native_session_id.is_empty() || update.native_session_id.len() > 4096 {
        return Err(ExternalAgentError::InvalidNativeSession);
    }
    if (!negotiation.capabilities.progress && !update.events.is_empty())
        || update.events.len() > maximum_progress_events
        || update
            .events
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        || update
            .events
            .iter()
            .any(|event| event.sequence == 0 || raw_content_len(&event.content) > 256 * 1024)
    {
        return Err(ExternalAgentError::ProgressViolation);
    }
    if update.approval_request.is_some() && !negotiation.capabilities.approval_requests {
        return Err(ExternalAgentError::UnexpectedApprovalRequest);
    }
    if update.terminal.is_some() && update.approval_request.is_some() {
        return Err(ExternalAgentError::ConflictingUpdate);
    }
    if update.terminal.as_ref().is_some_and(|terminal| {
        terminal.status == ExternalTerminalStatusV1::Succeeded
            && update.effect == ExternalEffectEvidenceV1::DefinitelyNotStarted
    }) {
        return Err(ExternalAgentError::ConflictingUpdate);
    }
    if let Some(approval) = &update.approval_request {
        validate_approval_request(approval)?;
        if &approval.invocation_id != expected_invocation_id {
            return Err(ExternalAgentError::ApprovalIdentityConflict);
        }
    }
    Ok(())
}

fn validate_approval_request(
    request: &ExternalApprovalRequestV1,
) -> Result<(), ExternalAgentError> {
    validate_text(&request.summary)?;
    if request.requested_scopes.is_empty()
        || !strictly_sorted_unique(&request.requested_scopes)
        || request
            .requested_scopes
            .iter()
            .any(|scope| scope.len() > 1024)
    {
        return Err(ExternalAgentError::InvalidApprovalRequest);
    }
    Ok(())
}

fn validate_resolution(
    pending: &ExternalApprovalRequestV1,
    resolution: &ExternalApprovalResolutionV1,
    generation: ProcessGeneration,
) -> Result<(), ExternalAgentError> {
    if resolution.host_generation() != generation {
        return Err(ExternalAgentError::StaleApprovalResolution);
    }
    if resolution.request_id() != &pending.request_id
        || resolution.invocation_id() != &pending.invocation_id
    {
        return Err(ExternalAgentError::ApprovalIdentityConflict);
    }
    if !strictly_sorted_unique(resolution.granted_scopes()) {
        return Err(ExternalAgentError::InvalidApprovalResolution);
    }
    let requested: BTreeSet<_> = pending.requested_scopes.iter().collect();
    if !resolution
        .granted_scopes()
        .iter()
        .all(|scope| requested.contains(scope))
    {
        return Err(ExternalAgentError::ApprovalScopeBroadened);
    }
    if resolution.decision() == ExternalApprovalDecisionV1::Denied
        && !resolution.granted_scopes().is_empty()
    {
        return Err(ExternalAgentError::InvalidApprovalResolution);
    }
    Ok(())
}

fn validate_session_ref(
    supplied: &NativeSessionRefV1,
    session: &SessionState,
    generation: ProcessGeneration,
) -> Result<(), ExternalAgentError> {
    if supplied.host_generation != generation || supplied != &session.reference {
        return Err(ExternalAgentError::StaleOrChangedSessionRef);
    }
    Ok(())
}

fn public_update(
    reference: NativeSessionRefV1,
    visibility: ExternalAgentVisibilityV1,
    invocation_id: StableId,
    update: &ExternalAgentPeerUpdateV1,
) -> ExternalAgentUpdateV1 {
    let terminal = update.terminal.as_ref().map(|terminal| {
        let (dispatch, terminal_evidence) = match terminal.status {
            ExternalTerminalStatusV1::Succeeded => {
                (DispatchEvidenceV1::Started, TerminalEvidenceV1::Succeeded)
            }
            ExternalTerminalStatusV1::Failed => {
                (map_effect(update.effect), TerminalEvidenceV1::Failed)
            }
            ExternalTerminalStatusV1::CancelledWithEvidence => match update.effect {
                ExternalEffectEvidenceV1::DefinitelyNotStarted => (
                    DispatchEvidenceV1::DefinitelyNotStarted,
                    TerminalEvidenceV1::Failed,
                ),
                ExternalEffectEvidenceV1::Started => (
                    DispatchEvidenceV1::Started,
                    TerminalEvidenceV1::CancelledWithEvidence,
                ),
                ExternalEffectEvidenceV1::Unknown => (
                    DispatchEvidenceV1::Unknown,
                    TerminalEvidenceV1::MissingOrConflicting,
                ),
            },
            ExternalTerminalStatusV1::Unknown => (
                map_effect(update.effect),
                TerminalEvidenceV1::MissingOrConflicting,
            ),
        };
        classify_outcome(
            invocation_id,
            EffectEvidenceV1 {
                dispatch,
                terminal: terminal_evidence,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        )
    });
    ExternalAgentUpdateV1 {
        native_session: reference,
        continuation_cursor: update.continuation_cursor.clone(),
        events: update.events.clone(),
        approval_request: update.approval_request.clone(),
        terminal,
        result: update
            .terminal
            .as_ref()
            .and_then(|value| value.result.clone()),
        visibility,
    }
}

fn map_effect(value: ExternalEffectEvidenceV1) -> DispatchEvidenceV1 {
    match value {
        ExternalEffectEvidenceV1::DefinitelyNotStarted => DispatchEvidenceV1::DefinitelyNotStarted,
        ExternalEffectEvidenceV1::Started => DispatchEvidenceV1::Started,
        ExternalEffectEvidenceV1::Unknown => DispatchEvidenceV1::Unknown,
    }
}

fn native_reference(
    target_id: &StableId,
    generation: ProcessGeneration,
    native_session_id: &str,
) -> NativeSessionRefV1 {
    let bytes = format!(
        "{}\0{}\0{native_session_id}",
        target_id.as_str(),
        generation.0
    );
    NativeSessionRefV1 {
        target_id: target_id.clone(),
        host_generation: generation,
        reference_hash: format!("sha256:{:x}", Sha256::digest(bytes.as_bytes())),
    }
}

fn map_dispatch(value: ExternalDispatchMilestoneV1) -> DispatchEvidenceV1 {
    match value {
        ExternalDispatchMilestoneV1::DefinitelyNotStarted => {
            DispatchEvidenceV1::DefinitelyNotStarted
        }
        ExternalDispatchMilestoneV1::Started => DispatchEvidenceV1::Started,
        ExternalDispatchMilestoneV1::Unknown => DispatchEvidenceV1::Unknown,
    }
}

fn uncertain_outcome(invocation_id: StableId) -> crate::CapabilityOutcomeV1 {
    classify_outcome(
        invocation_id,
        EffectEvidenceV1 {
            dispatch: DispatchEvidenceV1::Unknown,
            terminal: TerminalEvidenceV1::MissingOrConflicting,
            descriptor_is_idempotent: false,
            host_guarantees_same_id_deduplication: false,
        },
    )
}

fn protocol_outcome_error(
    invocation_id: StableId,
    error: ExternalAgentError,
) -> ExternalAgentError {
    ExternalAgentError::PeerOutcome {
        outcome: uncertain_outcome(invocation_id),
        code: format!("protocol_validation:{error}"),
        native_session_lost: false,
    }
}

fn validate_text(value: &str) -> Result<(), ExternalAgentError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        Err(ExternalAgentError::InvalidText)
    } else {
        Ok(())
    }
}

fn raw_content_len(content: &ExternalAgentRawContentV1) -> usize {
    match content {
        ExternalAgentRawContentV1::AssistantOutput(value)
        | ExternalAgentRawContentV1::Progress(value)
        | ExternalAgentRawContentV1::ReasoningRaw(value)
        | ExternalAgentRawContentV1::ReasoningSummary(value)
        | ExternalAgentRawContentV1::ArtifactReference(value)
        | ExternalAgentRawContentV1::Diagnostic(value) => value.len(),
    }
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn is_hash(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[derive(Debug, Error)]
pub enum ExternalAgentError {
    #[error("external-agent core authentication key is invalid")]
    InvalidAuthenticationKey,
    #[error("external-agent target is not configured")]
    NotConfigured,
    #[error("external-agent target is disabled")]
    Disabled,
    #[error("external-agent target is not core-attested")]
    Unattested,
    #[error("external-agent attestation is for a stale host generation")]
    StaleAttestation,
    #[error("external-agent manifest is malformed")]
    InvalidManifest,
    #[error("external-agent target limit reached")]
    TargetLimit,
    #[error("external-agent target registration is already in progress")]
    TargetRegistrationInProgress,
    #[error("external-agent target registration reservation conflicted")]
    TargetRegistrationConflict,
    #[error("external-agent binding drifted")]
    BindingDrift,
    #[error("external-agent negotiation identity drifted")]
    NegotiationIdentityDrift,
    #[error("external-agent negotiation is internally inconsistent")]
    InvalidNegotiation,
    #[error("unknown external-agent target")]
    UnknownTarget,
    #[error("external-agent target is degraded")]
    TargetDegraded,
    #[error("external-agent session limit reached")]
    SessionLimit,
    #[error("external-agent start reservation was lost after dispatch")]
    StartReservationMissing,
    #[error("external-agent start request is malformed")]
    InvalidStart,
    #[error("external-agent text is empty, oversized, or malformed")]
    InvalidText,
    #[error("external-agent deadline is invalid")]
    InvalidDeadline,
    #[error("external-agent request broadens workspace scope")]
    WorkspaceScopeBroadened,
    #[error("selected MCP forwarding was not negotiated")]
    McpForwardingUnsupported,
    #[error("external-agent request broadens the selected MCP set")]
    McpScopeBroadened,
    #[error("external-agent continuation was not negotiated")]
    ContinuationUnsupported,
    #[error("unknown external-agent native session")]
    UnknownSession,
    #[error("external-agent session has a pending approval")]
    ApprovalPending,
    #[error("external-agent continuation cursor drifted")]
    ContinuationCursorDrift,
    #[error("external-agent continuation is blocked by an uncertain prior outcome")]
    ContinuationBlocked,
    #[error("external-agent native session already has an active invocation")]
    InvocationOverlap,
    #[error("external-agent native session has no active invocation")]
    NoActiveInvocation,
    #[error("external-agent invocation correlation conflicted")]
    InvocationCorrelationConflict,
    #[error("external-agent control operation is already in flight")]
    ControlInFlight,
    #[error("external-agent invocation already crossed its terminal fence")]
    InvocationTerminal,
    #[error("external-agent retired-invocation capacity requires explicit session close")]
    RetiredInvocationCapacity,
    #[error("external-agent native session is being closed")]
    SessionClosing,
    #[error("external-agent native session still has active work")]
    SessionBusy,
    #[error("external-agent native session reference is stale or changed")]
    StaleOrChangedSessionRef,
    #[error("external-agent native session is malformed")]
    InvalidNativeSession,
    #[error("external-agent native session identity conflicted")]
    NativeSessionConflict,
    #[error("external-agent native session identity drifted")]
    NativeSessionDrift,
    #[error("external-agent progress is unsupported, malformed, or over budget")]
    ProgressViolation,
    #[error("external-agent emitted an approval request without negotiated support")]
    UnexpectedApprovalRequest,
    #[error("external-agent update contains conflicting terminal and approval facts")]
    ConflictingUpdate,
    #[error("external-agent approval request is malformed")]
    InvalidApprovalRequest,
    #[error("external-agent approval forwarding was not negotiated")]
    ApprovalsUnsupported,
    #[error("no external-agent approval is pending")]
    NoApprovalPending,
    #[error("external-agent approval resolution is for a stale host generation")]
    StaleApprovalResolution,
    #[error("external-agent approval resolution failed core authentication")]
    ApprovalAuthentication,
    #[error("external-agent approval identity conflicted")]
    ApprovalIdentityConflict,
    #[error("external-agent approval resolution is malformed")]
    InvalidApprovalResolution,
    #[error("external-agent approval resolution broadens requested scope")]
    ApprovalScopeBroadened,
    #[error("external-agent state lock is unavailable")]
    Poisoned,
    #[error("external-agent peer error: {0}")]
    Peer(#[from] ExternalAgentPeerErrorV1),
    #[error("external-agent peer settled conservatively: {code}")]
    PeerOutcome {
        outcome: crate::CapabilityOutcomeV1,
        code: String,
        native_session_lost: bool,
    },
}
