//! Durable worker-proposal, approval, dispatch, and settlement ordering.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ApprovalRequirement, AuthorityManifest};

const HOST_DISPATCH_DEFINITELY_NOT_STARTED_REASON: &str = "host_dispatch_definitely_not_started";

/// Identifies the broker-owned terminal settlement written when the host
/// conclusively rejects a dispatch before any tool effect can start.
#[must_use]
pub fn is_definitely_not_started_settlement_v1(settlement_hash: &str, uncertain: bool) -> bool {
    !uncertain && settlement_hash == outcome_hash(HOST_DISPATCH_DEFINITELY_NOT_STARTED_REASON)
}

/// Compatibility proposal used by the first broker scaffold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerProposal {
    pub proposal_id: StableId,
    pub capability_id: StableId,
    pub payload_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationDecision {
    Denied,
    AwaitingApproval,
    Approved { invocation_id: StableId },
}

/// Stateless compatibility helper. It can only narrow a frozen manifest.
pub struct InvocationBroker;

impl InvocationBroker {
    #[must_use]
    pub fn decide(
        manifest: &AuthorityManifest,
        proposal: &WorkerProposal,
        approved: bool,
    ) -> InvocationDecision {
        let Some(binding) = manifest.capability_bindings.iter().find(|binding| {
            binding.capability_id == proposal.capability_id && binding.enabled && binding.compatible
        }) else {
            return InvocationDecision::Denied;
        };
        if binding.approval == ApprovalRequirement::PerInvocation && !approved {
            return InvocationDecision::AwaitingApproval;
        }
        InvocationDecision::Approved {
            invocation_id: invocation_id(&proposal.proposal_id),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerInvocationProposalV1 {
    pub proposal_id: StableId,
    pub run_id: StableId,
    pub node_id: StableId,
    pub attempt: u32,
    pub capability_id: StableId,
    pub payload_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalChallengeV1 {
    pub invocation_id: StableId,
    pub nonce: StableId,
    pub expires_epoch_millis: u64,
    pub capability_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResponseV1 {
    pub invocation_id: StableId,
    pub nonce: StableId,
    pub approved: bool,
    pub now_epoch_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovedDispatchV1 {
    pub invocation_id: StableId,
    pub proposal_id: StableId,
    pub capability_id: StableId,
    pub payload_hash: String,
    pub manifest_id: StableId,
    pub lease_ids: Vec<StableId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BrokerDecisionV1 {
    Denied,
    AwaitingApproval(ApprovalChallengeV1),
    DispatchReady(ApprovedDispatchV1),
    AlreadySettled(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InvocationLedgerEventV1 {
    Proposed {
        invocation_id: StableId,
        proposal: WorkerInvocationProposalV1,
        manifest_id: StableId,
    },
    ApprovalRequested(ApprovalChallengeV1),
    ApprovalRejected {
        invocation_id: StableId,
        reason: String,
    },
    Authorized(ApprovedDispatchV1),
    DispatchAttempted {
        invocation_id: StableId,
    },
    DispatchAccepted {
        invocation_id: StableId,
    },
    ProgressCommitted {
        invocation_id: StableId,
        sequence: u64,
        payload_hash: String,
    },
    Settled {
        invocation_id: StableId,
        outcome_hash: String,
        uncertain: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DispatchOutboxV1 {
    pub outbox_id: StableId,
    pub dispatch: ApprovedDispatchV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerResultOutboxV1 {
    pub outbox_id: StableId,
    pub invocation_id: StableId,
    pub outcome_hash: String,
    pub uncertain: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryAcceptanceV1 {
    Accepted,
    AlreadyAccepted,
    RejectedDefinitelyNotStarted,
    Ambiguous,
}

/// Authenticated supervised-host port. Same invocation IDs must be deduplicated
/// by the receiving host generation.
pub trait ApprovedHostDispatchPortV1: Send + Sync {
    fn dispatch(&self, dispatch: &ApprovedDispatchV1) -> Result<DeliveryAcceptanceV1, BrokerError>;
}

/// Worker result port receives only settlement outboxes already committed in
/// the invocation ledger.
pub trait CommittedWorkerResultPortV1: Send + Sync {
    fn deliver(&self, result: &WorkerResultOutboxV1) -> Result<DeliveryAcceptanceV1, BrokerError>;
}

/// Core-owned lease issuance boundary. Implementations must derive stable,
/// idempotent lease identities from the frozen proposal and authority manifest.
/// The returned identities are committed in the same ledger write as the
/// approved dispatch, so the host never observes an unrecorded lease.
pub trait InvocationLeasePortV1: Send + Sync {
    fn issue_for_dispatch(
        &self,
        proposal: &WorkerInvocationProposalV1,
        manifest: &AuthorityManifest,
        invocation_id: &StableId,
    ) -> Result<Vec<StableId>, BrokerError>;

    /// Revokes leases prepared for a ledger write that did not commit.
    fn revoke_uncommitted(&self, lease_ids: &[StableId]) -> Result<(), BrokerError>;
}

/// Atomic durable ledger boundary. Dispatch is visible only with `Authorized` in the same write.
pub trait InvocationLedgerPortV1: Send + Sync {
    fn append_atomic(
        &self,
        events: &[InvocationLedgerEventV1],
        outbox: Option<&DispatchOutboxV1>,
    ) -> Result<(), BrokerError>;
    fn events(&self, invocation_id: &StableId)
    -> Result<Vec<InvocationLedgerEventV1>, BrokerError>;
    fn pending_dispatches(&self) -> Result<Vec<DispatchOutboxV1>, BrokerError>;
    fn mark_dispatch_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError>;
    fn append_settlement_atomic(
        &self,
        event: &InvocationLedgerEventV1,
        outbox: &WorkerResultOutboxV1,
    ) -> Result<(), BrokerError>;
    fn pending_worker_results(&self) -> Result<Vec<WorkerResultOutboxV1>, BrokerError>;
    fn mark_worker_result_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError>;
}

/// Hermetic durable-order fixture used by the core contract tests.
#[derive(Clone, Default)]
pub struct MemoryInvocationLedger {
    state: Arc<Mutex<MemoryLedgerState>>,
}

#[derive(Default)]
struct MemoryLedgerState {
    events: Vec<InvocationLedgerEventV1>,
    outbox: BTreeMap<String, (DispatchOutboxV1, bool)>,
    worker_results: BTreeMap<String, (WorkerResultOutboxV1, bool)>,
    fail_next: bool,
}

impl MemoryInvocationLedger {
    pub fn fail_next_write(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_next = true;
        }
    }
}

impl InvocationLedgerPortV1 for MemoryInvocationLedger {
    fn append_atomic(
        &self,
        events: &[InvocationLedgerEventV1],
        outbox: Option<&DispatchOutboxV1>,
    ) -> Result<(), BrokerError> {
        let mut state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        if state.fail_next {
            state.fail_next = false;
            return Err(BrokerError::CommitFailed);
        }
        if let Some(outbox) = outbox {
            if let Some((existing, _)) = state.outbox.get(outbox.outbox_id.as_str()) {
                if existing != outbox {
                    return Err(BrokerError::IdentityConflict);
                }
            }
        }
        state.events.extend_from_slice(events);
        if let Some(outbox) = outbox {
            state
                .outbox
                .entry(outbox.outbox_id.as_str().to_owned())
                .or_insert_with(|| (outbox.clone(), false));
        }
        Ok(())
    }

    fn events(
        &self,
        invocation_id: &StableId,
    ) -> Result<Vec<InvocationLedgerEventV1>, BrokerError> {
        let state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        Ok(state
            .events
            .iter()
            .filter(|event| event_invocation_id(event) == invocation_id)
            .cloned()
            .collect())
    }

    fn pending_dispatches(&self) -> Result<Vec<DispatchOutboxV1>, BrokerError> {
        let state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        Ok(state
            .outbox
            .values()
            .filter(|(_, delivered)| !delivered)
            .map(|(outbox, _)| outbox.clone())
            .collect())
    }

    fn mark_dispatch_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError> {
        let mut state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        let (_, delivered) = state
            .outbox
            .get_mut(outbox_id.as_str())
            .ok_or(BrokerError::UnknownInvocation)?;
        *delivered = true;
        Ok(())
    }

    fn append_settlement_atomic(
        &self,
        event: &InvocationLedgerEventV1,
        outbox: &WorkerResultOutboxV1,
    ) -> Result<(), BrokerError> {
        let mut state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        if state.fail_next {
            state.fail_next = false;
            return Err(BrokerError::CommitFailed);
        }
        if let Some((existing, _)) = state.worker_results.get(outbox.outbox_id.as_str()) {
            if existing != outbox {
                return Err(BrokerError::IdentityConflict);
            }
            return Ok(());
        }
        state.events.push(event.clone());
        state.worker_results.insert(
            outbox.outbox_id.as_str().to_owned(),
            (outbox.clone(), false),
        );
        Ok(())
    }

    fn pending_worker_results(&self) -> Result<Vec<WorkerResultOutboxV1>, BrokerError> {
        let state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        Ok(state
            .worker_results
            .values()
            .filter(|(_, delivered)| !delivered)
            .map(|(outbox, _)| outbox.clone())
            .collect())
    }

    fn mark_worker_result_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError> {
        let mut state = self.state.lock().map_err(|_| BrokerError::Unavailable)?;
        let (_, delivered) = state
            .worker_results
            .get_mut(outbox_id.as_str())
            .ok_or(BrokerError::UnknownInvocation)?;
        *delivered = true;
        Ok(())
    }
}

pub struct DurableInvocationBroker {
    ledger: Arc<dyn InvocationLedgerPortV1>,
    lease_port: Option<Arc<dyn InvocationLeasePortV1>>,
    approval_ttl_millis: u64,
}

impl DurableInvocationBroker {
    #[must_use]
    pub fn new(ledger: Arc<dyn InvocationLedgerPortV1>, approval_ttl_millis: u64) -> Self {
        Self {
            ledger,
            lease_port: None,
            approval_ttl_millis: approval_ttl_millis.max(1),
        }
    }

    /// Adds the core-owned, invocation-scoped lease issuer used before an
    /// approved dispatch is atomically recorded. No host or worker can mint a
    /// lease through this API.
    #[must_use]
    pub fn with_lease_port(mut self, lease_port: Arc<dyn InvocationLeasePortV1>) -> Self {
        self.lease_port = Some(lease_port);
        self
    }

    pub fn propose(
        &self,
        manifest: &AuthorityManifest,
        proposal: WorkerInvocationProposalV1,
        now_epoch_millis: u64,
    ) -> Result<BrokerDecisionV1, BrokerError> {
        let invocation_id = invocation_id(&proposal.proposal_id);
        let existing = self.ledger.events(&invocation_id)?;
        if !existing.is_empty() {
            verify_proposal_identity(&existing, &proposal, &manifest.manifest_id)?;
            match replay_decision(&existing) {
                Ok(decision) => return Ok(decision),
                Err(BrokerError::IncompleteState) => {
                    return self.decide_after_proposed(manifest, proposal, now_epoch_millis);
                }
                Err(error) => return Err(error),
            }
        }
        self.ledger.append_atomic(
            &[InvocationLedgerEventV1::Proposed {
                invocation_id: invocation_id.clone(),
                proposal: proposal.clone(),
                manifest_id: manifest.manifest_id.clone(),
            }],
            None,
        )?;
        self.decide_after_proposed(manifest, proposal, now_epoch_millis)
    }

    fn decide_after_proposed(
        &self,
        manifest: &AuthorityManifest,
        proposal: WorkerInvocationProposalV1,
        now_epoch_millis: u64,
    ) -> Result<BrokerDecisionV1, BrokerError> {
        let invocation_id = invocation_id(&proposal.proposal_id);
        let Some(binding) = manifest.capability_bindings.iter().find(|binding| {
            binding.capability_id == proposal.capability_id && binding.enabled && binding.compatible
        }) else {
            self.ledger.append_atomic(
                &[InvocationLedgerEventV1::ApprovalRejected {
                    invocation_id,
                    reason: "authority_denied".into(),
                }],
                None,
            )?;
            return Ok(BrokerDecisionV1::Denied);
        };
        if binding.approval == ApprovalRequirement::PerInvocation {
            let challenge = ApprovalChallengeV1 {
                invocation_id,
                nonce: approval_nonce(&proposal.proposal_id, &manifest.manifest_id),
                expires_epoch_millis: now_epoch_millis
                    .checked_add(self.approval_ttl_millis)
                    .ok_or(BrokerError::InvalidApproval)?,
                capability_id: proposal.capability_id,
            };
            self.ledger.append_atomic(
                &[InvocationLedgerEventV1::ApprovalRequested(
                    challenge.clone(),
                )],
                None,
            )?;
            Ok(BrokerDecisionV1::AwaitingApproval(challenge))
        } else {
            self.authorize(manifest, proposal)
        }
    }

    pub fn resolve_approval(
        &self,
        manifest: &AuthorityManifest,
        response: &ApprovalResponseV1,
    ) -> Result<BrokerDecisionV1, BrokerError> {
        let events = self.ledger.events(&response.invocation_id)?;
        let (proposal, stored_manifest) = proposed_identity(&events)?;
        if &stored_manifest != &manifest.manifest_id {
            return Err(BrokerError::StaleManifest);
        }
        let challenge = events
            .iter()
            .find_map(|event| match event {
                InvocationLedgerEventV1::ApprovalRequested(challenge) => Some(challenge),
                _ => None,
            })
            .ok_or(BrokerError::InvalidApproval)?;
        if let Ok(existing) = replay_decision(&events) {
            match (&existing, response.approved) {
                (BrokerDecisionV1::DispatchReady(_), true)
                | (BrokerDecisionV1::AlreadySettled(_), true)
                | (BrokerDecisionV1::Denied, false) => return Ok(existing),
                (BrokerDecisionV1::AwaitingApproval(_), _) => {}
                _ => return Err(BrokerError::IdentityConflict),
            }
        }
        if challenge.nonce != response.nonce {
            return Err(BrokerError::InvalidApproval);
        }
        if response.now_epoch_millis >= challenge.expires_epoch_millis {
            self.ledger.append_atomic(
                &[InvocationLedgerEventV1::ApprovalRejected {
                    invocation_id: response.invocation_id.clone(),
                    reason: "approval_expired".into(),
                }],
                None,
            )?;
            return Ok(BrokerDecisionV1::Denied);
        }
        if !response.approved {
            self.ledger.append_atomic(
                &[InvocationLedgerEventV1::ApprovalRejected {
                    invocation_id: response.invocation_id.clone(),
                    reason: "user_rejected".into(),
                }],
                None,
            )?;
            return Ok(BrokerDecisionV1::Denied);
        }
        self.authorize(manifest, proposal)
    }

    fn authorize(
        &self,
        manifest: &AuthorityManifest,
        proposal: WorkerInvocationProposalV1,
    ) -> Result<BrokerDecisionV1, BrokerError> {
        let binding = manifest
            .capability_bindings
            .iter()
            .find(|binding| binding.capability_id == proposal.capability_id)
            .ok_or(BrokerError::NotAuthorized)?;
        if !binding.enabled || !binding.compatible {
            return Err(BrokerError::NotAuthorized);
        }
        let invocation_id = invocation_id(&proposal.proposal_id);
        let mut lease_ids = match &self.lease_port {
            Some(port) => port.issue_for_dispatch(&proposal, manifest, &invocation_id)?,
            None => Vec::new(),
        };
        lease_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if lease_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            if let Some(port) = &self.lease_port {
                let _ = port.revoke_uncommitted(&lease_ids);
            }
            return Err(BrokerError::DuplicateLease);
        }
        let dispatch = ApprovedDispatchV1 {
            invocation_id,
            proposal_id: proposal.proposal_id,
            capability_id: proposal.capability_id,
            payload_hash: proposal.payload_hash,
            manifest_id: manifest.manifest_id.clone(),
            lease_ids: lease_ids.clone(),
        };
        let outbox = DispatchOutboxV1 {
            outbox_id: stable_digest_id("dispatch", dispatch.invocation_id.as_str()),
            dispatch: dispatch.clone(),
        };
        if let Err(error) = self.ledger.append_atomic(
            &[InvocationLedgerEventV1::Authorized(dispatch.clone())],
            Some(&outbox),
        ) {
            if let Some(port) = &self.lease_port {
                port.revoke_uncommitted(&lease_ids)
                    .map_err(|_| BrokerError::LeaseRollbackFailed)?;
            }
            return Err(error);
        }
        Ok(BrokerDecisionV1::DispatchReady(dispatch))
    }

    pub fn accept_dispatch(&self, invocation_id: &StableId) -> Result<(), BrokerError> {
        let events = self.ledger.events(invocation_id)?;
        require_authorized(&events)?;
        if !events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAttempted { .. }))
        {
            return Err(BrokerError::NotAttempted);
        }
        if events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. }))
        {
            return Ok(());
        }
        self.ledger.append_atomic(
            &[InvocationLedgerEventV1::DispatchAccepted {
                invocation_id: invocation_id.clone(),
            }],
            None,
        )
    }

    pub fn commit_progress(
        &self,
        invocation_id: &StableId,
        sequence: u64,
        payload_hash: String,
    ) -> Result<(), BrokerError> {
        let events = self.ledger.events(invocation_id)?;
        require_authorized(&events)?;
        if !events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. }))
        {
            return Err(BrokerError::NotAccepted);
        }
        if events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::Settled { .. }))
        {
            return Err(BrokerError::AlreadySettled);
        }
        if let Some(existing_hash) = events.iter().find_map(|event| match event {
            InvocationLedgerEventV1::ProgressCommitted {
                sequence: existing,
                payload_hash,
                ..
            } if *existing == sequence => Some(payload_hash),
            _ => None,
        }) {
            return if existing_hash == &payload_hash {
                Ok(())
            } else {
                Err(BrokerError::IdentityConflict)
            };
        }
        let next_sequence = events
            .iter()
            .filter_map(|event| match event {
                InvocationLedgerEventV1::ProgressCommitted { sequence, .. } => Some(*sequence),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(BrokerError::ProgressSequence)?;
        if sequence != next_sequence {
            return Err(BrokerError::ProgressSequence);
        }
        self.ledger.append_atomic(
            &[InvocationLedgerEventV1::ProgressCommitted {
                invocation_id: invocation_id.clone(),
                sequence,
                payload_hash,
            }],
            None,
        )
    }

    pub fn settle(
        &self,
        invocation_id: &StableId,
        outcome_hash: String,
        uncertain: bool,
    ) -> Result<(), BrokerError> {
        let events = self.ledger.events(invocation_id)?;
        require_authorized(&events)?;
        if !events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. }))
        {
            return Err(BrokerError::NotAccepted);
        }
        if let Some(existing) = events.iter().find_map(|event| match event {
            InvocationLedgerEventV1::Settled {
                outcome_hash,
                uncertain,
                ..
            } => Some((outcome_hash, *uncertain)),
            _ => None,
        }) {
            return if existing == (&outcome_hash, uncertain) {
                Ok(())
            } else {
                Err(BrokerError::IdentityConflict)
            };
        }
        let event = InvocationLedgerEventV1::Settled {
            invocation_id: invocation_id.clone(),
            outcome_hash: outcome_hash.clone(),
            uncertain,
        };
        let outbox = WorkerResultOutboxV1 {
            outbox_id: stable_digest_id("result", invocation_id.as_str()),
            invocation_id: invocation_id.clone(),
            outcome_hash,
            uncertain,
        };
        self.ledger.append_settlement_atomic(&event, &outbox)
    }

    pub fn pending_dispatches(&self) -> Result<Vec<DispatchOutboxV1>, BrokerError> {
        self.ledger.pending_dispatches()
    }

    pub fn mark_dispatch_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError> {
        self.ledger.mark_dispatch_delivered(outbox_id)
    }

    /// Delivers committed dispatch outboxes. Ambiguous acceptance is surfaced
    /// and left pending; this method never loops or silently replays it.
    pub fn deliver_dispatches(
        &self,
        host: &dyn ApprovedHostDispatchPortV1,
    ) -> Result<usize, BrokerError> {
        let mut delivered = 0;
        for outbox in self.ledger.pending_dispatches()? {
            if self.deliver_outbox(&outbox, host)? {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// Delivers exactly one pending outbox entry and leaves every other
    /// pending dispatch untouched. Nested dispatches use this so an in-flight
    /// sibling dispatch (the subagent tool runs its child loop inside the
    /// parent tool dispatch) is never mistaken for an abandoned one.
    pub fn deliver_pending_dispatch_for(
        &self,
        invocation_id: &StableId,
        host: &dyn ApprovedHostDispatchPortV1,
    ) -> Result<bool, BrokerError> {
        let Some(outbox) = self
            .ledger
            .pending_dispatches()?
            .into_iter()
            .find(|entry| entry.dispatch.invocation_id == *invocation_id)
        else {
            return Ok(false);
        };
        self.deliver_outbox(&outbox, host)
    }

    /// Delivers one outbox entry with the shared attempt/accept/settle state
    /// machine; returns whether this call delivered it.
    fn deliver_outbox(
        &self,
        outbox: &DispatchOutboxV1,
        host: &dyn ApprovedHostDispatchPortV1,
    ) -> Result<bool, BrokerError> {
        let invocation_id = &outbox.dispatch.invocation_id;
        let events = self.ledger.events(invocation_id)?;
        if events.iter().any(|event| {
            matches!(event, InvocationLedgerEventV1::Settled { .. })
                || matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. })
        }) {
            self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
            return Ok(true);
        }
        if events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAttempted { .. }))
        {
            self.settle_unaccepted_dispatch(
                invocation_id,
                "host_dispatch_recovery_uncertain",
                true,
            )?;
            self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
            return Err(BrokerError::AmbiguousDispatch);
        }
        self.ledger.append_atomic(
            &[InvocationLedgerEventV1::DispatchAttempted {
                invocation_id: invocation_id.clone(),
            }],
            None,
        )?;
        let acceptance = match host.dispatch(&outbox.dispatch) {
            Ok(acceptance) => acceptance,
            Err(error) => {
                self.settle_unaccepted_dispatch(
                    invocation_id,
                    "host_dispatch_transport_uncertain",
                    true,
                )?;
                self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
                return Err(error);
            }
        };
        match acceptance {
            DeliveryAcceptanceV1::Accepted | DeliveryAcceptanceV1::AlreadyAccepted => {
                self.accept_dispatch(invocation_id)?;
                self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
                Ok(true)
            }
            DeliveryAcceptanceV1::RejectedDefinitelyNotStarted => {
                self.settle_unaccepted_dispatch(
                    invocation_id,
                    HOST_DISPATCH_DEFINITELY_NOT_STARTED_REASON,
                    false,
                )?;
                self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
                Err(BrokerError::DispatchRejected)
            }
            DeliveryAcceptanceV1::Ambiguous => {
                self.settle_unaccepted_dispatch(
                    invocation_id,
                    "host_dispatch_acceptance_uncertain",
                    true,
                )?;
                self.ledger.mark_dispatch_delivered(&outbox.outbox_id)?;
                Err(BrokerError::AmbiguousDispatch)
            }
        }
    }

    fn settle_unaccepted_dispatch(
        &self,
        invocation_id: &StableId,
        reason: &str,
        uncertain: bool,
    ) -> Result<(), BrokerError> {
        let events = self.ledger.events(invocation_id)?;
        require_authorized(&events)?;
        if let Some((existing_hash, existing_uncertain)) =
            events.iter().find_map(|event| match event {
                InvocationLedgerEventV1::Settled {
                    outcome_hash,
                    uncertain,
                    ..
                } => Some((outcome_hash, *uncertain)),
                _ => None,
            })
        {
            let expected = outcome_hash(reason);
            return if existing_hash == &expected && existing_uncertain == uncertain {
                Ok(())
            } else {
                Err(BrokerError::IdentityConflict)
            };
        }
        let outcome_hash = outcome_hash(reason);
        let event = InvocationLedgerEventV1::Settled {
            invocation_id: invocation_id.clone(),
            outcome_hash: outcome_hash.clone(),
            uncertain,
        };
        let outbox = WorkerResultOutboxV1 {
            outbox_id: stable_digest_id("result", invocation_id.as_str()),
            invocation_id: invocation_id.clone(),
            outcome_hash,
            uncertain,
        };
        self.ledger.append_settlement_atomic(&event, &outbox)
    }

    pub fn deliver_worker_results(
        &self,
        worker: &dyn CommittedWorkerResultPortV1,
    ) -> Result<usize, BrokerError> {
        let mut delivered = 0;
        for outbox in self.ledger.pending_worker_results()? {
            match worker.deliver(&outbox)? {
                DeliveryAcceptanceV1::Accepted | DeliveryAcceptanceV1::AlreadyAccepted => {
                    self.ledger
                        .mark_worker_result_delivered(&outbox.outbox_id)?;
                    delivered += 1;
                }
                DeliveryAcceptanceV1::RejectedDefinitelyNotStarted => {
                    return Err(BrokerError::WorkerDeliveryRejected);
                }
                DeliveryAcceptanceV1::Ambiguous => {
                    return Err(BrokerError::AmbiguousWorkerDelivery);
                }
            }
        }
        Ok(delivered)
    }
}

fn invocation_id(proposal_id: &StableId) -> StableId {
    stable_digest_id("invoke", proposal_id.as_str())
}

fn approval_nonce(proposal_id: &StableId, manifest_id: &StableId) -> StableId {
    stable_digest_id(
        "approval",
        &format!("{}:{}", proposal_id.as_str(), manifest_id.as_str()),
    )
}

fn stable_digest_id(prefix: &str, input: &str) -> StableId {
    let digest = format!("{:x}", Sha256::digest(input.as_bytes()));
    StableId::parse(format!("{prefix}.{}", &digest[..32]))
        .expect("fixed digest IDs satisfy StableId bounds")
}

fn outcome_hash(reason: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(reason.as_bytes()))
}

fn event_invocation_id(event: &InvocationLedgerEventV1) -> &StableId {
    match event {
        InvocationLedgerEventV1::Proposed { invocation_id, .. }
        | InvocationLedgerEventV1::ApprovalRejected { invocation_id, .. }
        | InvocationLedgerEventV1::DispatchAttempted { invocation_id }
        | InvocationLedgerEventV1::DispatchAccepted { invocation_id }
        | InvocationLedgerEventV1::ProgressCommitted { invocation_id, .. }
        | InvocationLedgerEventV1::Settled { invocation_id, .. } => invocation_id,
        InvocationLedgerEventV1::ApprovalRequested(challenge) => &challenge.invocation_id,
        InvocationLedgerEventV1::Authorized(dispatch) => &dispatch.invocation_id,
    }
}

fn verify_proposal_identity(
    events: &[InvocationLedgerEventV1],
    proposal: &WorkerInvocationProposalV1,
    manifest_id: &StableId,
) -> Result<(), BrokerError> {
    let (stored, stored_manifest) = proposed_identity(events)?;
    if &stored == proposal && &stored_manifest == manifest_id {
        Ok(())
    } else {
        Err(BrokerError::IdentityConflict)
    }
}

fn proposed_identity(
    events: &[InvocationLedgerEventV1],
) -> Result<(WorkerInvocationProposalV1, StableId), BrokerError> {
    events
        .iter()
        .find_map(|event| match event {
            InvocationLedgerEventV1::Proposed {
                proposal,
                manifest_id,
                ..
            } => Some((proposal.clone(), manifest_id.clone())),
            _ => None,
        })
        .ok_or(BrokerError::UnknownInvocation)
}

fn replay_decision(events: &[InvocationLedgerEventV1]) -> Result<BrokerDecisionV1, BrokerError> {
    if let Some((outcome_hash, _)) = events.iter().find_map(|event| match event {
        InvocationLedgerEventV1::Settled {
            outcome_hash,
            uncertain,
            ..
        } => Some((outcome_hash, uncertain)),
        _ => None,
    }) {
        return Ok(BrokerDecisionV1::AlreadySettled(outcome_hash.clone()));
    }
    if let Some(dispatch) = events.iter().find_map(|event| match event {
        InvocationLedgerEventV1::Authorized(dispatch) => Some(dispatch),
        _ => None,
    }) {
        return Ok(BrokerDecisionV1::DispatchReady(dispatch.clone()));
    }
    if events
        .iter()
        .any(|event| matches!(event, InvocationLedgerEventV1::ApprovalRejected { .. }))
    {
        return Ok(BrokerDecisionV1::Denied);
    }
    if let Some(challenge) = events.iter().find_map(|event| match event {
        InvocationLedgerEventV1::ApprovalRequested(challenge) => Some(challenge),
        _ => None,
    }) {
        return Ok(BrokerDecisionV1::AwaitingApproval(challenge.clone()));
    }
    Err(BrokerError::IncompleteState)
}

fn require_authorized(events: &[InvocationLedgerEventV1]) -> Result<(), BrokerError> {
    if events
        .iter()
        .any(|event| matches!(event, InvocationLedgerEventV1::Authorized(_)))
    {
        Ok(())
    } else {
        Err(BrokerError::NotAuthorized)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BrokerError {
    #[error("invocation ledger is unavailable")]
    Unavailable,
    #[error("durable invocation commit failed")]
    CommitFailed,
    #[error("invocation identity was reused with different content")]
    IdentityConflict,
    #[error("unknown invocation")]
    UnknownInvocation,
    #[error("approval nonce is stale, expired, or invalid")]
    InvalidApproval,
    #[error("authority manifest changed during approval")]
    StaleManifest,
    #[error("invocation has not been durably authorized")]
    NotAuthorized,
    #[error("invocation already settled")]
    AlreadySettled,
    #[error("dispatch has not been durably accepted")]
    NotAccepted,
    #[error("dispatch has not been durably marked attempted")]
    NotAttempted,
    #[error("invocation progress sequence is not contiguous")]
    ProgressSequence,
    #[error("durable invocation state is incomplete")]
    IncompleteState,
    #[error("host definitely rejected dispatch before start")]
    DispatchRejected,
    #[error("host dispatch acceptance is ambiguous; automatic replay is forbidden")]
    AmbiguousDispatch,
    #[error("worker definitely rejected committed result delivery")]
    WorkerDeliveryRejected,
    #[error("worker result acceptance is ambiguous; automatic replay is forbidden")]
    AmbiguousWorkerDelivery,
    #[error("a lease issuer returned the same lease identity more than once")]
    DuplicateLease,
    #[error("an uncommitted invocation lease could not be revoked")]
    LeaseRollbackFailed,
}
