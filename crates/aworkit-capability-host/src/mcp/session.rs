//! Exact-binding MCP session lifecycle and conservative call settlement.

use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
};

use aworkit_protocol::{ProcessGeneration, StableId};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    DispatchEvidenceV1, EffectEvidenceV1, RetrySafetyV1, TerminalEvidenceV1, classify_outcome,
};

use super::contracts::{
    ForwardableMcpSetV1, McpCallKindV1, McpCallOutcomeV1, McpCallV1, McpCancellationEvidenceV1,
    McpCancellationReceiptV1, McpCapabilitySnapshotV1, McpCatalogV1, McpDispatchMilestoneV1,
    McpFeatureSetV1, McpInitializeRequestV1, McpPeerErrorV1, McpPeerPort, McpProgressV1,
    McpProtocolEvidenceV1, McpServerManifestV1, McpSessionHealthV1,
};

const MAX_SESSIONS: usize = 128;
const MAX_CATALOG_ENTRIES: usize = 16_384;
const MAX_NAME_BYTES: usize = 512;
const MAX_SETTLED_INVOCATIONS: usize = 4_096;

#[derive(Clone)]
struct SessionState {
    manifest: McpServerManifestV1,
    snapshot: McpCapabilitySnapshotV1,
    in_flight: BTreeMap<String, ActiveCallState>,
    settled: BTreeMap<String, ()>,
    reconnect_count: u32,
    reconnect_budget: u32,
    degraded: bool,
    closing: bool,
    retired: bool,
    reconnecting: bool,
}

#[derive(Clone, Debug, Default)]
struct ActiveCallState {
    cancellation_in_progress: bool,
    cancellation: Option<McpCancellationReceiptV1>,
}

/// Bounded session manager for one authenticated capability-host generation.
pub struct McpSessionManager {
    generation: ProcessGeneration,
    peer: Arc<dyn McpPeerPort>,
    sessions: Mutex<BTreeMap<String, SessionState>>,
    initializing: Mutex<BTreeMap<String, McpServerManifestV1>>,
    state_changed: Condvar,
    maximum_sessions: usize,
    reconnect_budget: u32,
}

impl McpSessionManager {
    #[must_use]
    pub fn new(generation: ProcessGeneration, peer: Arc<dyn McpPeerPort>) -> Self {
        Self::with_limits(generation, peer, MAX_SESSIONS, 2)
    }

    #[must_use]
    pub fn with_limits(
        generation: ProcessGeneration,
        peer: Arc<dyn McpPeerPort>,
        maximum_sessions: usize,
        reconnect_budget: u32,
    ) -> Self {
        Self {
            generation,
            peer,
            sessions: Mutex::new(BTreeMap::new()),
            initializing: Mutex::new(BTreeMap::new()),
            state_changed: Condvar::new(),
            maximum_sessions: maximum_sessions.clamp(1, MAX_SESSIONS),
            reconnect_budget,
        }
    }

    /// Initializes or reuses the exact attested server binding. Reuse never
    /// substitutes a changed version/hash/configuration.
    pub fn open(
        &self,
        manifest: McpServerManifestV1,
    ) -> Result<McpCapabilitySnapshotV1, McpSessionError> {
        validate_manifest(&manifest, self.generation)?;
        let key = manifest.server_id.as_str().to_owned();
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        if let Some(session) = sessions.get(&key) {
            if session.closing {
                return Err(McpSessionError::SessionClosing);
            }
            if session.retired {
                return Err(McpSessionError::SessionRetired);
            }
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            if session.manifest == manifest && !session.degraded {
                return Ok(session.snapshot.clone());
            }
            return Err(McpSessionError::BindingDrift);
        }
        let mut initializing = self
            .initializing
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        if let Some(reserved) = initializing.get(&key) {
            return if reserved == &manifest {
                Err(McpSessionError::SessionInitializing)
            } else {
                Err(McpSessionError::BindingDrift)
            };
        }
        if sessions.len().saturating_add(initializing.len()) >= self.maximum_sessions {
            return Err(McpSessionError::SessionLimit);
        }
        initializing.insert(key.clone(), manifest.clone());
        drop(initializing);
        drop(sessions);

        let initialized = initialize_snapshot(self.peer.as_ref(), &manifest, None);
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let mut initializing = self
            .initializing
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let reserved = initializing
            .remove(&key)
            .ok_or(McpSessionError::InitializationReservationLost)?;
        if reserved != manifest || sessions.contains_key(&key) {
            return Err(McpSessionError::BindingDrift);
        }
        let snapshot = initialized?;
        sessions.insert(
            key,
            SessionState {
                manifest,
                snapshot: snapshot.clone(),
                in_flight: BTreeMap::new(),
                settled: BTreeMap::new(),
                reconnect_count: 0,
                reconnect_budget: self.reconnect_budget,
                degraded: false,
                closing: false,
                retired: false,
                reconnecting: false,
            },
        );
        Ok(snapshot)
    }

    /// Explicitly reconnects a degraded transport and proves that negotiation
    /// and discovery did not drift. It never replays the interrupted call.
    pub fn reconnect(
        &self,
        server_id: &StableId,
    ) -> Result<McpCapabilitySnapshotV1, McpSessionError> {
        let (manifest, expected_catalog_hash) = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| McpSessionError::Poisoned)?;
            let session = sessions
                .get_mut(server_id.as_str())
                .ok_or(McpSessionError::UnknownSession)?;
            if session.closing {
                return Err(McpSessionError::SessionClosing);
            }
            if session.retired {
                return Err(McpSessionError::SessionRetired);
            }
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            if !session.in_flight.is_empty() {
                return Err(McpSessionError::CallsStillActive);
            }
            if session.reconnect_count >= session.reconnect_budget {
                return Err(McpSessionError::ReconnectBudgetExhausted);
            }
            session.reconnecting = true;
            (
                session.manifest.clone(),
                session.snapshot.catalog_hash.clone(),
            )
        };
        let initialized =
            initialize_snapshot(self.peer.as_ref(), &manifest, Some(&expected_catalog_hash));
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let session = sessions
            .get_mut(server_id.as_str())
            .ok_or(McpSessionError::UnknownSession)?;
        if !session.reconnecting || session.manifest != manifest {
            return Err(McpSessionError::InitializationReservationLost);
        }
        session.reconnecting = false;
        let snapshot = initialized?;
        session.snapshot = snapshot.clone();
        session.reconnect_count = session.reconnect_count.saturating_add(1);
        session.degraded = false;
        Ok(snapshot)
    }

    /// Invokes exactly one discovered operation. Every error is settled once;
    /// transport loss only degrades the session for a later explicit reconnect.
    pub fn invoke(
        &self,
        server_id: &StableId,
        call: &McpCallV1,
    ) -> Result<McpCallOutcomeV1, McpSessionError> {
        let (manifest, snapshot, reconnect_count) = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| McpSessionError::Poisoned)?;
            let session = sessions
                .get_mut(server_id.as_str())
                .ok_or(McpSessionError::UnknownSession)?;
            if session.closing {
                return Err(McpSessionError::SessionClosing);
            }
            if session.retired {
                return Err(McpSessionError::SessionRetired);
            }
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            if session.degraded {
                return Err(McpSessionError::SessionDegraded);
            }
            validate_call(&session.snapshot, call)?;
            if session.in_flight.len() >= session.manifest.maximum_in_flight {
                return Err(McpSessionError::Backpressure);
            }
            if session.settled.contains_key(call.invocation_id.as_str()) {
                return Err(McpSessionError::InvocationAlreadySettled);
            }
            if session.settled.len() >= MAX_SETTLED_INVOCATIONS {
                return Err(McpSessionError::SettlementCapacity);
            }
            if session.in_flight.contains_key(call.invocation_id.as_str()) {
                return Err(McpSessionError::DuplicateInvocation);
            }
            session.in_flight.insert(
                call.invocation_id.as_str().to_owned(),
                ActiveCallState::default(),
            );
            (
                session.manifest.clone(),
                session.snapshot.clone(),
                session.reconnect_count,
            )
        };

        let peer_result = self.peer.invoke(&manifest, call);
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        sessions = self
            .state_changed
            .wait_while(sessions, |sessions| {
                sessions
                    .get(server_id.as_str())
                    .and_then(|session| session.in_flight.get(call.invocation_id.as_str()))
                    .is_some_and(|active| active.cancellation_in_progress)
            })
            .map_err(|_| McpSessionError::Poisoned)?;
        let session = sessions
            .get_mut(server_id.as_str())
            .ok_or(McpSessionError::UnknownSession)?;
        let active = session
            .in_flight
            .remove(call.invocation_id.as_str())
            .ok_or(McpSessionError::UnknownInvocation)?;

        let outcome = match peer_result {
            Ok(result) => {
                let progress = match validate_progress(
                    result.progress,
                    manifest.maximum_progress_events,
                    snapshot.features.progress,
                ) {
                    Ok(progress) => progress,
                    Err(_) => {
                        session.degraded = true;
                        let outcome = protocol_violation_outcome(
                            call.invocation_id.clone(),
                            &snapshot,
                            reconnect_count,
                        );
                        session
                            .settled
                            .insert(call.invocation_id.as_str().to_owned(), ());
                        return Ok(outcome);
                    }
                };
                successful_call_outcome(
                    call.invocation_id.clone(),
                    result.result,
                    progress,
                    &snapshot,
                    reconnect_count,
                    active.cancellation.as_ref(),
                )
            }
            Err(error) => {
                if error.transport_lost {
                    session.degraded = true;
                }
                failed_call_outcome(
                    call.invocation_id.clone(),
                    &snapshot,
                    reconnect_count,
                    error,
                    active.cancellation.as_ref(),
                )
            }
        };
        session
            .settled
            .insert(call.invocation_id.as_str().to_owned(), ());
        Ok(outcome)
    }

    /// Uses a reserved control path; unsupported or lost cancellation never
    /// becomes proof that an effect did not occur.
    pub fn cancel(
        &self,
        server_id: &StableId,
        invocation_id: &StableId,
    ) -> Result<McpCancellationReceiptV1, McpSessionError> {
        let (manifest, snapshot, reconnect_count) = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| McpSessionError::Poisoned)?;
            let session = sessions
                .get_mut(server_id.as_str())
                .ok_or(McpSessionError::UnknownSession)?;
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            let active = session
                .in_flight
                .get_mut(invocation_id.as_str())
                .ok_or(McpSessionError::UnknownInvocation)?;
            if active.cancellation_in_progress || active.cancellation.is_some() {
                return Err(McpSessionError::DuplicateCancellation);
            }
            active.cancellation_in_progress = true;
            (
                session.manifest.clone(),
                session.snapshot.clone(),
                session.reconnect_count,
            )
        };
        let cancellation = if snapshot.features.cancellation {
            self.peer.cancel(&manifest, invocation_id)
        } else {
            Ok(McpCancellationEvidenceV1::Unsupported)
        };
        let (cancellation_evidence, transport_lost, definitely_not_started) = match cancellation {
            Ok(value) => (
                value,
                false,
                value == McpCancellationEvidenceV1::ConfirmedBeforeEffect,
            ),
            Err(error) => (
                McpCancellationEvidenceV1::Unknown,
                error.transport_lost,
                error.dispatch == McpDispatchMilestoneV1::DefinitelyNotStarted,
            ),
        };
        let receipt = McpCancellationReceiptV1 {
            invocation_id: invocation_id.clone(),
            evidence: cancellation_evidence,
            protocol: evidence(
                &snapshot,
                reconnect_count,
                transport_lost,
                definitely_not_started,
            ),
        };
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let session = sessions
            .get_mut(server_id.as_str())
            .ok_or(McpSessionError::UnknownSession)?;
        let active = session
            .in_flight
            .get_mut(invocation_id.as_str())
            .ok_or(McpSessionError::UnknownInvocation)?;
        active.cancellation_in_progress = false;
        active.cancellation = Some(receipt.clone());
        if transport_lost {
            session.degraded = true;
        }
        drop(sessions);
        self.state_changed.notify_all();
        Ok(receipt)
    }

    /// Closes a session while retaining bounded replay tombstones for the
    /// lifetime of this authenticated host generation.
    pub fn close(&self, server_id: &StableId) -> Result<(), McpSessionError> {
        let manifest = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| McpSessionError::Poisoned)?;
            let session = sessions
                .get_mut(server_id.as_str())
                .ok_or(McpSessionError::UnknownSession)?;
            if session.closing {
                return Err(McpSessionError::SessionClosing);
            }
            if session.retired {
                return Err(McpSessionError::SessionRetired);
            }
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            if !session.in_flight.is_empty() {
                return Err(McpSessionError::CallsStillActive);
            }
            session.closing = true;
            session.manifest.clone()
        };
        match self.peer.close(&manifest) {
            Ok(()) => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| McpSessionError::Poisoned)?;
                let session = sessions
                    .get_mut(server_id.as_str())
                    .ok_or(McpSessionError::UnknownSession)?;
                if !session.in_flight.is_empty() {
                    session.degraded = true;
                    session.closing = false;
                    return Err(McpSessionError::CallsStillActive);
                }
                session.closing = false;
                session.retired = true;
                Ok(())
            }
            Err(error) => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| McpSessionError::Poisoned)?;
                let session = sessions
                    .get_mut(server_id.as_str())
                    .ok_or(McpSessionError::UnknownSession)?;
                session.closing = false;
                session.degraded = true;
                Err(McpSessionError::Peer(error))
            }
        }
    }

    /// Returns only negotiated snapshots; callers still select which exact set
    /// an external-agent adapter is allowed to receive.
    pub fn forwardable_set(
        &self,
        server_ids: &[StableId],
    ) -> Result<ForwardableMcpSetV1, McpSessionError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let mut servers = BTreeMap::new();
        for server_id in server_ids {
            let session = sessions
                .get(server_id.as_str())
                .ok_or(McpSessionError::UnknownSession)?;
            if session.closing {
                return Err(McpSessionError::SessionClosing);
            }
            if session.retired {
                return Err(McpSessionError::SessionRetired);
            }
            if session.reconnecting {
                return Err(McpSessionError::SessionReconnecting);
            }
            if session.degraded {
                return Err(McpSessionError::SessionDegraded);
            }
            if servers
                .insert(server_id.as_str().to_owned(), session.snapshot.clone())
                .is_some()
            {
                return Err(McpSessionError::DuplicateServer);
            }
        }
        Ok(ForwardableMcpSetV1 { servers })
    }

    /// Returns bounded lifecycle facts for Settings and health projection.
    pub fn health(&self, server_id: &StableId) -> Result<McpSessionHealthV1, McpSessionError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| McpSessionError::Poisoned)?;
        let session = sessions
            .get(server_id.as_str())
            .ok_or(McpSessionError::UnknownSession)?;
        Ok(McpSessionHealthV1 {
            server_id: server_id.clone(),
            host_generation: session.manifest.host_generation,
            degraded: session.degraded,
            closing: session.closing,
            retired: session.retired,
            reconnecting: session.reconnecting,
            in_flight: session.in_flight.len(),
            maximum_in_flight: session.manifest.maximum_in_flight,
            settled_invocations: session.settled.len(),
            maximum_settled_invocations: MAX_SETTLED_INVOCATIONS,
            reconnect_count: session.reconnect_count,
            reconnect_budget: session.reconnect_budget,
        })
    }
}

fn initialize_snapshot(
    peer: &dyn McpPeerPort,
    manifest: &McpServerManifestV1,
    expected_catalog_hash: Option<&str>,
) -> Result<McpCapabilitySnapshotV1, McpSessionError> {
    let response = peer.initialize(
        manifest,
        &McpInitializeRequestV1 {
            server_id: manifest.server_id.clone(),
            host_generation: manifest.host_generation,
            minimum_protocol_version: manifest.minimum_protocol_version,
            maximum_protocol_version: manifest.maximum_protocol_version,
        },
    )?;
    if response.server_id != manifest.server_id {
        return Err(McpSessionError::IdentityDrift);
    }
    if !(manifest.minimum_protocol_version..=manifest.maximum_protocol_version)
        .contains(&response.protocol_version)
    {
        return Err(McpSessionError::ProtocolIncompatible);
    }
    validate_catalog(&response.catalog, &response.features)?;
    let catalog_hash = catalog_hash(&response.catalog)?;
    if expected_catalog_hash.is_some_and(|expected| expected != catalog_hash) {
        return Err(McpSessionError::CatalogDrift);
    }
    Ok(McpCapabilitySnapshotV1 {
        server_id: manifest.server_id.clone(),
        host_generation: manifest.host_generation,
        binding_hash: manifest.binding_hash.clone(),
        protocol_version: response.protocol_version,
        features: response.features,
        catalog: response.catalog,
        catalog_hash,
    })
}

fn validate_manifest(
    manifest: &McpServerManifestV1,
    generation: ProcessGeneration,
) -> Result<(), McpSessionError> {
    if !manifest.configured {
        return Err(McpSessionError::NotConfigured);
    }
    if !manifest.enabled {
        return Err(McpSessionError::Disabled);
    }
    if !manifest.core_attested {
        return Err(McpSessionError::Unattested);
    }
    if manifest.host_generation != generation {
        return Err(McpSessionError::StaleAttestation);
    }
    if !is_hash(&manifest.binding_hash)
        || manifest.adapter_version.is_empty()
        || manifest.minimum_protocol_version == 0
        || manifest.minimum_protocol_version > manifest.maximum_protocol_version
        || manifest.maximum_in_flight == 0
        || manifest.maximum_progress_events == 0
    {
        return Err(McpSessionError::InvalidManifest);
    }
    if !strictly_sorted_unique(&manifest.secret_slots)
        || !strictly_sorted_unique(&manifest.workspace_roots)
    {
        return Err(McpSessionError::NonCanonicalManifest);
    }
    Ok(())
}

fn validate_catalog(
    catalog: &McpCatalogV1,
    features: &McpFeatureSetV1,
) -> Result<(), McpSessionError> {
    let total = catalog
        .tools
        .len()
        .saturating_add(catalog.resources.len())
        .saturating_add(catalog.prompts.len());
    if total > MAX_CATALOG_ENTRIES
        || (!features.tools && !catalog.tools.is_empty())
        || (!features.resources && !catalog.resources.is_empty())
        || (!features.prompts && !catalog.prompts.is_empty())
    {
        return Err(McpSessionError::InvalidCatalog);
    }
    let tool_names = catalog
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    if !strictly_sorted_unique(&tool_names)
        || !strictly_sorted_unique(&catalog.resources)
        || !strictly_sorted_unique(&catalog.prompts)
        || catalog
            .tools
            .iter()
            .any(|tool| !valid_name(&tool.name) || !is_hash(&tool.input_schema_hash))
        || catalog.resources.iter().any(|name| !valid_name(name))
        || catalog.prompts.iter().any(|name| !valid_name(name))
    {
        return Err(McpSessionError::InvalidCatalog);
    }
    Ok(())
}

fn validate_call(
    snapshot: &McpCapabilitySnapshotV1,
    call: &McpCallV1,
) -> Result<(), McpSessionError> {
    if !valid_name(&call.name) {
        return Err(McpSessionError::InvalidCall);
    }
    match call.kind {
        McpCallKindV1::Tool => {
            if !snapshot.features.tools {
                return Err(McpSessionError::FeatureNotNegotiated);
            }
            let tool = snapshot
                .catalog
                .tools
                .iter()
                .find(|tool| tool.name == call.name)
                .ok_or(McpSessionError::CatalogEntryMissing)?;
            if call.expected_schema_hash.as_deref() != Some(tool.input_schema_hash.as_str()) {
                return Err(McpSessionError::SchemaDrift);
            }
        }
        McpCallKindV1::Resource => {
            if !snapshot.features.resources {
                return Err(McpSessionError::FeatureNotNegotiated);
            }
            if !snapshot
                .catalog
                .resources
                .iter()
                .any(|name| name == &call.name)
            {
                return Err(McpSessionError::CatalogEntryMissing);
            }
            if call.expected_schema_hash.is_some() {
                return Err(McpSessionError::InvalidCall);
            }
        }
        McpCallKindV1::Prompt => {
            if !snapshot.features.prompts {
                return Err(McpSessionError::FeatureNotNegotiated);
            }
            if !snapshot
                .catalog
                .prompts
                .iter()
                .any(|name| name == &call.name)
            {
                return Err(McpSessionError::CatalogEntryMissing);
            }
            if call.expected_schema_hash.is_some() {
                return Err(McpSessionError::InvalidCall);
            }
        }
    }
    Ok(())
}

fn validate_progress(
    progress: Vec<McpProgressV1>,
    maximum: usize,
    supported: bool,
) -> Result<Vec<McpProgressV1>, McpSessionError> {
    if (!supported && !progress.is_empty()) || progress.len() > maximum {
        return Err(McpSessionError::ProgressViolation);
    }
    if progress
        .windows(2)
        .any(|pair| pair[0].sequence >= pair[1].sequence)
        || progress
            .iter()
            .any(|event| event.sequence == 0 || event.message.len() > 16 * 1024)
    {
        return Err(McpSessionError::ProgressViolation);
    }
    Ok(progress)
}

fn failed_call_outcome(
    invocation_id: StableId,
    snapshot: &McpCapabilitySnapshotV1,
    reconnect_count: u32,
    error: McpPeerErrorV1,
    cancellation: Option<&McpCancellationReceiptV1>,
) -> McpCallOutcomeV1 {
    if let Some(receipt) = cancellation {
        match receipt.evidence {
            McpCancellationEvidenceV1::ConfirmedBeforeEffect => {
                if error.dispatch != McpDispatchMilestoneV1::DefinitelyNotStarted {
                    return conflicting_call_outcome(
                        invocation_id,
                        snapshot,
                        reconnect_count,
                        receipt.protocol.transport_lost || error.transport_lost,
                    );
                }
                return McpCallOutcomeV1 {
                    result: None,
                    progress: Vec::new(),
                    outcome: classify_outcome(
                        invocation_id,
                        EffectEvidenceV1 {
                            dispatch: DispatchEvidenceV1::DefinitelyNotStarted,
                            terminal: TerminalEvidenceV1::Failed,
                            descriptor_is_idempotent: false,
                            host_guarantees_same_id_deduplication: false,
                        },
                    ),
                    evidence: evidence(
                        snapshot,
                        reconnect_count,
                        receipt.protocol.transport_lost || error.transport_lost,
                        true,
                    ),
                };
            }
            McpCancellationEvidenceV1::ConfirmedAfterStart => {
                if error.dispatch == McpDispatchMilestoneV1::DefinitelyNotStarted {
                    return conflicting_call_outcome(
                        invocation_id,
                        snapshot,
                        reconnect_count,
                        receipt.protocol.transport_lost || error.transport_lost,
                    );
                }
                return McpCallOutcomeV1 {
                    result: None,
                    progress: Vec::new(),
                    outcome: classify_outcome(
                        invocation_id,
                        EffectEvidenceV1 {
                            dispatch: DispatchEvidenceV1::Started,
                            terminal: TerminalEvidenceV1::CancelledWithEvidence,
                            descriptor_is_idempotent: false,
                            host_guarantees_same_id_deduplication: false,
                        },
                    ),
                    evidence: evidence(
                        snapshot,
                        reconnect_count,
                        receipt.protocol.transport_lost || error.transport_lost,
                        false,
                    ),
                };
            }
            McpCancellationEvidenceV1::Unsupported | McpCancellationEvidenceV1::Unknown => {}
        }
    }
    let dispatch = map_dispatch(error.dispatch);
    let definitely_not_started = dispatch == DispatchEvidenceV1::DefinitelyNotStarted;
    let outcome = classify_outcome(
        invocation_id,
        EffectEvidenceV1 {
            dispatch,
            terminal: if definitely_not_started {
                TerminalEvidenceV1::Failed
            } else {
                TerminalEvidenceV1::MissingOrConflicting
            },
            descriptor_is_idempotent: false,
            host_guarantees_same_id_deduplication: false,
        },
    );
    debug_assert!(
        definitely_not_started || outcome.retry_safety == RetrySafetyV1::NotSafe,
        "an ambiguous MCP failure must never be marked retry-safe"
    );
    McpCallOutcomeV1 {
        result: None,
        progress: Vec::new(),
        outcome,
        evidence: evidence(
            snapshot,
            reconnect_count,
            error.transport_lost,
            definitely_not_started,
        ),
    }
}

fn conflicting_call_outcome(
    invocation_id: StableId,
    snapshot: &McpCapabilitySnapshotV1,
    reconnect_count: u32,
    transport_lost: bool,
) -> McpCallOutcomeV1 {
    McpCallOutcomeV1 {
        result: None,
        progress: Vec::new(),
        outcome: classify_outcome(
            invocation_id,
            EffectEvidenceV1 {
                dispatch: DispatchEvidenceV1::Unknown,
                terminal: TerminalEvidenceV1::MissingOrConflicting,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        ),
        evidence: evidence(snapshot, reconnect_count, transport_lost, false),
    }
}

fn successful_call_outcome(
    invocation_id: StableId,
    result: serde_json::Value,
    progress: Vec<McpProgressV1>,
    snapshot: &McpCapabilitySnapshotV1,
    reconnect_count: u32,
    cancellation: Option<&McpCancellationReceiptV1>,
) -> McpCallOutcomeV1 {
    let cancellation_conflicts = cancellation.is_some_and(|receipt| {
        matches!(
            receipt.evidence,
            McpCancellationEvidenceV1::ConfirmedBeforeEffect
                | McpCancellationEvidenceV1::ConfirmedAfterStart
        )
    });
    McpCallOutcomeV1 {
        result: Some(result),
        progress,
        outcome: classify_outcome(
            invocation_id,
            EffectEvidenceV1 {
                dispatch: DispatchEvidenceV1::Started,
                terminal: if cancellation_conflicts {
                    TerminalEvidenceV1::MissingOrConflicting
                } else {
                    TerminalEvidenceV1::Succeeded
                },
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        ),
        evidence: evidence(snapshot, reconnect_count, false, false),
    }
}

fn protocol_violation_outcome(
    invocation_id: StableId,
    snapshot: &McpCapabilitySnapshotV1,
    reconnect_count: u32,
) -> McpCallOutcomeV1 {
    McpCallOutcomeV1 {
        result: None,
        progress: Vec::new(),
        outcome: classify_outcome(
            invocation_id,
            EffectEvidenceV1 {
                dispatch: DispatchEvidenceV1::Started,
                terminal: TerminalEvidenceV1::MissingOrConflicting,
                descriptor_is_idempotent: false,
                host_guarantees_same_id_deduplication: false,
            },
        ),
        evidence: evidence(snapshot, reconnect_count, false, false),
    }
}

fn evidence(
    snapshot: &McpCapabilitySnapshotV1,
    reconnect_count: u32,
    transport_lost: bool,
    definitely_not_started: bool,
) -> McpProtocolEvidenceV1 {
    McpProtocolEvidenceV1 {
        server_id: snapshot.server_id.clone(),
        protocol_version: snapshot.protocol_version,
        catalog_hash: snapshot.catalog_hash.clone(),
        reconnect_count,
        transport_lost,
        definitely_not_started,
    }
}

fn map_dispatch(dispatch: McpDispatchMilestoneV1) -> DispatchEvidenceV1 {
    match dispatch {
        McpDispatchMilestoneV1::DefinitelyNotStarted => DispatchEvidenceV1::DefinitelyNotStarted,
        McpDispatchMilestoneV1::Started => DispatchEvidenceV1::Started,
        McpDispatchMilestoneV1::Unknown => DispatchEvidenceV1::Unknown,
    }
}

fn catalog_hash(catalog: &McpCatalogV1) -> Result<String, McpSessionError> {
    let bytes = serde_json::to_vec(catalog).map_err(|_| McpSessionError::InvalidCatalog)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_BYTES && !value.contains(['\0', '\n', '\r'])
}

fn is_hash(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[derive(Debug, Error)]
pub enum McpSessionError {
    #[error("MCP server is not configured")]
    NotConfigured,
    #[error("MCP server is disabled")]
    Disabled,
    #[error("MCP server binding was not attested by the trusted core")]
    Unattested,
    #[error("MCP attestation belongs to a stale host generation")]
    StaleAttestation,
    #[error("MCP server manifest is malformed")]
    InvalidManifest,
    #[error("MCP manifest set-like fields are not canonical")]
    NonCanonicalManifest,
    #[error("MCP session limit reached")]
    SessionLimit,
    #[error("MCP binding changed and cannot replace an active session")]
    BindingDrift,
    #[error("MCP server identity changed during initialization")]
    IdentityDrift,
    #[error("MCP protocol versions are incompatible")]
    ProtocolIncompatible,
    #[error("MCP discovery catalog is malformed")]
    InvalidCatalog,
    #[error("MCP catalog changed across reconnect")]
    CatalogDrift,
    #[error("unknown MCP session")]
    UnknownSession,
    #[error("MCP session is degraded and requires explicit reconnect")]
    SessionDegraded,
    #[error("MCP session is closing")]
    SessionClosing,
    #[error("MCP session was retired for this host generation")]
    SessionRetired,
    #[error("MCP session is initializing")]
    SessionInitializing,
    #[error("MCP session is reconnecting")]
    SessionReconnecting,
    #[error("MCP initialization reservation was lost")]
    InitializationReservationLost,
    #[error("MCP reconnect budget exhausted")]
    ReconnectBudgetExhausted,
    #[error("MCP calls remain active")]
    CallsStillActive,
    #[error("MCP session is at its in-flight limit")]
    Backpressure,
    #[error("duplicate active MCP invocation ID")]
    DuplicateInvocation,
    #[error("MCP invocation ID was already settled and cannot be replayed")]
    InvocationAlreadySettled,
    #[error("MCP session lifetime settlement capacity reached; rotate the host generation")]
    SettlementCapacity,
    #[error("unknown active MCP invocation")]
    UnknownInvocation,
    #[error("MCP cancellation was already requested for this invocation")]
    DuplicateCancellation,
    #[error("MCP call is malformed")]
    InvalidCall,
    #[error("MCP feature was not negotiated")]
    FeatureNotNegotiated,
    #[error("MCP catalog entry is missing")]
    CatalogEntryMissing,
    #[error("MCP tool schema drifted from the frozen catalog")]
    SchemaDrift,
    #[error("MCP progress is unsupported, out of order, or over budget")]
    ProgressViolation,
    #[error("MCP forwarding set contains a duplicate server")]
    DuplicateServer,
    #[error("MCP session state lock is unavailable")]
    Poisoned,
    #[error("MCP peer error: {0:?}")]
    Peer(#[from] McpPeerErrorV1),
}
