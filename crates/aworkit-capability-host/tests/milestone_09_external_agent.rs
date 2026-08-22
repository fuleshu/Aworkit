use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use aworkit_capability_host::{
    ExternalAgentCapabilitySetV1, ExternalAgentContinueV1, ExternalAgentError,
    ExternalAgentManager, ExternalAgentManifestV1, ExternalAgentNegotiationV1,
    ExternalAgentPeerErrorV1, ExternalAgentPeerPort, ExternalAgentPeerUpdateV1,
    ExternalAgentProtocolV1, ExternalAgentRawContentV1, ExternalAgentRawEventV1,
    ExternalAgentStartV1, ExternalAgentVisibilityV1, ExternalApprovalDecisionV1,
    ExternalApprovalRequestV1, ExternalApprovalResolutionV1, ExternalCancellationEvidenceV1,
    ExternalDispatchMilestoneV1, ExternalEffectEvidenceV1, ExternalTerminalStatusV1,
    ExternalTerminalV1, ForwardableMcpSetV1, McpCapabilitySnapshotV1, McpCatalogV1,
    McpFeatureSetV1, OutcomeDispositionV1, RetrySafetyV1,
};
use aworkit_protocol::{ProcessGeneration, StableId};

const HASH: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CORE_KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("test ID")
}

fn manifest() -> ExternalAgentManifestV1 {
    ExternalAgentManifestV1 {
        target_id: id("agent.codex"),
        adapter_version: "1.0.0".into(),
        binding_hash: HASH.into(),
        host_generation: ProcessGeneration(9),
        configured: true,
        enabled: true,
        core_attested: true,
        protocol: ExternalAgentProtocolV1::CodexAppServer,
        maximum_active_sessions: 4,
        maximum_progress_events: 8,
        allowed_workspace_roots: vec!["/workspace".into()],
        allowed_mcp_server_ids: vec!["mcp.github".into()],
        secret_slots: vec!["api_key".into()],
    }
}

fn capabilities() -> ExternalAgentCapabilitySetV1 {
    ExternalAgentCapabilitySetV1 {
        progress: true,
        native_sessions: true,
        continuation: true,
        cancellation: true,
        approval_requests: true,
        steering: false,
        artifacts: true,
        selected_mcp_forwarding: true,
    }
}

fn negotiation() -> ExternalAgentNegotiationV1 {
    ExternalAgentNegotiationV1 {
        target_id: id("agent.codex"),
        host_generation: ProcessGeneration(9),
        capabilities: capabilities(),
        visibility: ExternalAgentVisibilityV1::PartialLifecycle,
        protocol_version: "codex-app-server.v1".into(),
    }
}

fn start(invocation: &str) -> ExternalAgentStartV1 {
    ExternalAgentStartV1 {
        invocation_id: id(invocation),
        task: "Inspect the exact approved workspace".into(),
        desired_result: "A bounded report".into(),
        workspace_roots: vec!["/workspace".into()],
        deadline_epoch_millis: 10_000,
        maximum_turns: 8,
        lease_handles: vec![id("lease.1")],
        forwarded_mcp: None,
    }
}

fn terminal_update(
    invocation_id: &str,
    native_id: &str,
    cursor: &str,
    result: &str,
) -> ExternalAgentPeerUpdateV1 {
    ExternalAgentPeerUpdateV1 {
        invocation_id: id(invocation_id),
        native_session_id: native_id.into(),
        continuation_cursor: Some(cursor.into()),
        events: vec![ExternalAgentRawEventV1 {
            sequence: 1,
            content: ExternalAgentRawContentV1::AssistantOutput(result.into()),
        }],
        approval_request: None,
        terminal: Some(ExternalTerminalV1 {
            status: ExternalTerminalStatusV1::Succeeded,
            result: Some(serde_json::json!({"text": result})),
        }),
        effect: ExternalEffectEvidenceV1::Started,
    }
}

fn manager(peer: Arc<ScriptedAgentPeer>) -> ExternalAgentManager {
    ExternalAgentManager::new(ProcessGeneration(9), peer, CORE_KEY.to_vec()).expect("manager")
}

#[derive(Default)]
struct PeerGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl PeerGate {
    fn enter_and_wait(&self) {
        let mut state = self.state.lock().expect("peer gate");
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).expect("peer gate wait");
        }
    }

    fn wait_until_entered(&self) {
        let state = self.state.lock().expect("peer gate");
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.0)
            .expect("peer gate wait");
        assert!(state.0 && !timeout.timed_out(), "peer did not enter gate");
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("peer gate");
        state.1 = true;
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct ScriptedAgentPeer {
    negotiations: Mutex<VecDeque<Result<ExternalAgentNegotiationV1, ExternalAgentPeerErrorV1>>>,
    starts: Mutex<VecDeque<Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>>>,
    continuations: Mutex<VecDeque<Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>>>,
    approvals: Mutex<VecDeque<Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>>>,
    cancellations:
        Mutex<VecDeque<Result<ExternalCancellationEvidenceV1, ExternalAgentPeerErrorV1>>>,
    start_count: AtomicUsize,
    negotiation_count: AtomicUsize,
    approval_count: AtomicUsize,
    close_count: AtomicUsize,
    cancelled_invocations: Mutex<Vec<StableId>>,
    negotiation_gate: Mutex<Option<Arc<PeerGate>>>,
    start_gate: Mutex<Option<Arc<PeerGate>>>,
    continuation_gate: Mutex<Option<Arc<PeerGate>>>,
    cancellation_gate: Mutex<Option<Arc<PeerGate>>>,
}

impl ScriptedAgentPeer {
    fn install_gate(slot: &Mutex<Option<Arc<PeerGate>>>) -> Arc<PeerGate> {
        let gate = Arc::new(PeerGate::default());
        *slot.lock().expect("gate slot") = Some(gate.clone());
        gate
    }

    fn push_negotiation(&self, value: ExternalAgentNegotiationV1) {
        self.negotiations
            .lock()
            .expect("negotiation script")
            .push_back(Ok(value));
    }

    fn push_start(&self, value: Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>) {
        self.starts.lock().expect("start script").push_back(value);
    }

    fn push_continuation(&self, value: ExternalAgentPeerUpdateV1) {
        self.continuations
            .lock()
            .expect("continuation script")
            .push_back(Ok(value));
    }

    fn push_approval(&self, value: ExternalAgentPeerUpdateV1) {
        self.approvals
            .lock()
            .expect("approval script")
            .push_back(Ok(value));
    }

    fn push_cancellation(&self, value: ExternalCancellationEvidenceV1) {
        self.cancellations
            .lock()
            .expect("cancellation script")
            .push_back(Ok(value));
    }
}

impl ExternalAgentPeerPort for ScriptedAgentPeer {
    fn negotiate(
        &self,
        _manifest: &ExternalAgentManifestV1,
    ) -> Result<ExternalAgentNegotiationV1, ExternalAgentPeerErrorV1> {
        self.negotiation_count.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.negotiation_gate.lock().expect("gate slot").clone() {
            gate.enter_and_wait();
        }
        self.negotiations
            .lock()
            .expect("negotiation script")
            .pop_front()
            .expect("scripted negotiation")
    }

    fn start(
        &self,
        _manifest: &ExternalAgentManifestV1,
        _request: &ExternalAgentStartV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1> {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.start_gate.lock().expect("gate slot").clone() {
            gate.enter_and_wait();
        }
        self.starts
            .lock()
            .expect("start script")
            .pop_front()
            .expect("scripted start")
    }

    fn continue_session(
        &self,
        _manifest: &ExternalAgentManifestV1,
        _native_session_id: &str,
        _request: &ExternalAgentContinueV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1> {
        if let Some(gate) = self.continuation_gate.lock().expect("gate slot").clone() {
            gate.enter_and_wait();
        }
        self.continuations
            .lock()
            .expect("continuation script")
            .pop_front()
            .expect("scripted continuation")
    }

    fn resolve_approval(
        &self,
        _manifest: &ExternalAgentManifestV1,
        _native_session_id: &str,
        _resolution: &ExternalApprovalResolutionV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1> {
        self.approval_count.fetch_add(1, Ordering::SeqCst);
        self.approvals
            .lock()
            .expect("approval script")
            .pop_front()
            .expect("scripted approval")
    }

    fn cancel(
        &self,
        _manifest: &ExternalAgentManifestV1,
        _native_session_id: &str,
        invocation_id: &StableId,
    ) -> Result<ExternalCancellationEvidenceV1, ExternalAgentPeerErrorV1> {
        self.cancelled_invocations
            .lock()
            .expect("cancel correlation")
            .push(invocation_id.clone());
        if let Some(gate) = self.cancellation_gate.lock().expect("gate slot").clone() {
            gate.enter_and_wait();
        }
        self.cancellations
            .lock()
            .expect("cancellation script")
            .pop_front()
            .unwrap_or(Ok(ExternalCancellationEvidenceV1::Unknown))
    }

    fn close_session(
        &self,
        _manifest: &ExternalAgentManifestV1,
        _native_session_id: &str,
    ) -> Result<(), ExternalAgentPeerErrorV1> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn negotiates_starts_and_explicitly_continues_a_private_native_session() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(terminal_update(
        "invocation.start",
        "native-secret-id",
        "cursor.1",
        "first",
    )));
    peer.push_continuation(terminal_update(
        "invocation.continue",
        "native-secret-id",
        "cursor.2",
        "second",
    ));
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");

    let first = manager
        .start(&id("agent.codex"), &start("invocation.start"))
        .expect("start");
    assert_eq!(
        first.terminal.as_ref().map(|value| value.disposition),
        Some(OutcomeDispositionV1::Succeeded)
    );
    assert!(
        !first
            .native_session
            .reference_hash
            .contains("native-secret-id")
    );
    assert_eq!(
        first.visibility,
        ExternalAgentVisibilityV1::PartialLifecycle
    );

    let second = manager
        .continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.continue"),
            native_session: first.native_session.clone(),
            input: "Continue explicitly".into(),
            deadline_epoch_millis: 20_000,
            expected_cursor: Some("cursor.1".into()),
            forwarded_mcp: None,
        })
        .expect("continue");
    assert_eq!(second.continuation_cursor.as_deref(), Some("cursor.2"));
    assert_eq!(second.result, Some(serde_json::json!({"text": "second"})));
    assert!(matches!(
        manager.continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.continue"),
            native_session: first.native_session,
            input: "attempt to reopen a terminal invocation".into(),
            deadline_epoch_millis: 30_000,
            expected_cursor: Some("cursor.2".into()),
            forwarded_mcp: None,
        }),
        Err(ExternalAgentError::InvocationTerminal)
    ));
}

#[test]
fn approval_requests_wait_for_an_exact_generation_fenced_core_resolution() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(ExternalAgentPeerUpdateV1 {
        invocation_id: id("invocation.approval"),
        native_session_id: "native-approval".into(),
        continuation_cursor: Some("cursor.pending".into()),
        events: vec![],
        approval_request: Some(ExternalApprovalRequestV1 {
            request_id: id("approval.1"),
            invocation_id: id("invocation.approval"),
            summary: "Write the approved file".into(),
            requested_scopes: vec!["files.write".into()],
        }),
        terminal: None,
        effect: ExternalEffectEvidenceV1::DefinitelyNotStarted,
    }));
    peer.push_approval(terminal_update(
        "invocation.approval",
        "native-approval",
        "cursor.done",
        "approved by core",
    ));
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");
    let update = manager
        .start(&id("agent.codex"), &start("invocation.approval"))
        .expect("start");
    assert!(update.approval_request.is_some());
    assert!(update.terminal.is_none());

    let signed = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.forged"),
        ProcessGeneration(9),
        update.native_session.clone(),
        id("invocation.approval"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed resolution");
    let mut forged_json = serde_json::to_value(signed).expect("resolution JSON");
    forged_json["decision"] = serde_json::json!("denied");
    let forged: ExternalApprovalResolutionV1 =
        serde_json::from_value(forged_json).expect("forged wire value");
    assert!(matches!(
        manager.resolve_approval(&forged),
        Err(ExternalAgentError::ApprovalAuthentication)
    ));

    let signed = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.non-ascii-tag"),
        ProcessGeneration(9),
        update.native_session.clone(),
        id("invocation.approval"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed resolution");
    let mut forged_json = serde_json::to_value(signed).expect("resolution JSON");
    forged_json["coreAuthenticationTag"] =
        serde_json::json!("hmac-sha256:éééééééééééééééééééééééééééééééé");
    let forged: ExternalApprovalResolutionV1 =
        serde_json::from_value(forged_json).expect("forged wire value");
    assert!(matches!(
        manager.resolve_approval(&forged),
        Err(ExternalAgentError::ApprovalAuthentication)
    ));

    let wrong_invocation = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.wrong-invocation"),
        ProcessGeneration(9),
        update.native_session.clone(),
        id("invocation.other"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed wrong correlation");
    assert!(matches!(
        manager.resolve_approval(&wrong_invocation),
        Err(ExternalAgentError::InvocationCorrelationConflict)
    ));

    let stale_generation = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.stale-generation"),
        ProcessGeneration(8),
        update.native_session.clone(),
        id("invocation.approval"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed stale generation");
    assert!(matches!(
        manager.resolve_approval(&stale_generation),
        Err(ExternalAgentError::StaleApprovalResolution)
    ));

    let mut wrong_session_ref = update.native_session.clone();
    wrong_session_ref.reference_hash = HASH.into();
    let wrong_session = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.wrong-session"),
        ProcessGeneration(9),
        wrong_session_ref,
        id("invocation.approval"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed wrong session");
    assert!(matches!(
        manager.resolve_approval(&wrong_session),
        Err(ExternalAgentError::UnknownSession)
    ));
    assert_eq!(peer.approval_count.load(Ordering::SeqCst), 0);

    let broadened = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.1"),
        id("decision.bad"),
        ProcessGeneration(9),
        update.native_session.clone(),
        id("invocation.approval"),
        ExternalApprovalDecisionV1::Approved,
        vec!["credentials.read".into()],
    )
    .expect("signed resolution");
    assert!(matches!(
        manager.resolve_approval(&broadened),
        Err(ExternalAgentError::ApprovalScopeBroadened)
    ));
    assert_eq!(peer.approval_count.load(Ordering::SeqCst), 0);

    let resolved = manager
        .resolve_approval(
            &ExternalApprovalResolutionV1::issue(
                CORE_KEY,
                id("approval.1"),
                id("decision.good"),
                ProcessGeneration(9),
                update.native_session.clone(),
                id("invocation.approval"),
                ExternalApprovalDecisionV1::Approved,
                vec!["files.write".into()],
            )
            .expect("signed resolution"),
        )
        .expect("core resolution");
    assert_eq!(
        resolved.terminal.as_ref().map(|value| value.disposition),
        Some(OutcomeDispositionV1::Succeeded)
    );
    assert_eq!(peer.approval_count.load(Ordering::SeqCst), 1);
}

fn forwarded(server_id: &str) -> ForwardableMcpSetV1 {
    let mut servers = BTreeMap::new();
    servers.insert(
        server_id.into(),
        McpCapabilitySnapshotV1 {
            server_id: id(server_id),
            host_generation: ProcessGeneration(9),
            binding_hash: HASH.into(),
            protocol_version: 1,
            features: McpFeatureSetV1::default(),
            catalog: McpCatalogV1::default(),
            catalog_hash: HASH.into(),
        },
    );
    ForwardableMcpSetV1 { servers }
}

#[test]
fn workspace_credentials_and_selected_mcp_sets_cannot_be_widened() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");

    let mut workspace = start("invocation.workspace");
    workspace.workspace_roots = vec!["/outside".into()];
    assert!(matches!(
        manager.start(&id("agent.codex"), &workspace),
        Err(ExternalAgentError::WorkspaceScopeBroadened)
    ));
    let mut credentials = start("invocation.credentials");
    credentials.lease_handles.push(id("lease.2"));
    assert!(matches!(
        manager.start(&id("agent.codex"), &credentials),
        Err(ExternalAgentError::InvalidStart)
    ));
    let mut mcp = start("invocation.mcp");
    mcp.forwarded_mcp = Some(forwarded("mcp.unapproved"));
    assert!(matches!(
        manager.start(&id("agent.codex"), &mcp),
        Err(ExternalAgentError::McpScopeBroadened)
    ));
    assert_eq!(peer.start_count.load(Ordering::SeqCst), 0);
}

#[test]
fn lost_or_unconfirmed_native_work_is_uncertain_and_never_restarted() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Err(ExternalAgentPeerErrorV1 {
        code: "native_session_lost".into(),
        message: "transport disappeared after task acceptance".into(),
        dispatch: ExternalDispatchMilestoneV1::Started,
        native_session_lost: true,
    }));
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");

    let error = manager
        .start(&id("agent.codex"), &start("invocation.lost"))
        .expect_err("uncertain native session");
    let ExternalAgentError::PeerOutcome {
        outcome,
        native_session_lost,
        ..
    } = error
    else {
        panic!("expected conservative peer outcome");
    };
    assert!(native_session_lost);
    assert_eq!(outcome.disposition, OutcomeDispositionV1::OutcomeUncertain);
    assert_eq!(outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert_eq!(peer.start_count.load(Ordering::SeqCst), 1);
    assert!(manager.health(&id("agent.codex")).expect("health").degraded);
}

#[test]
fn unconfirmed_external_cancellation_does_not_claim_remote_work_stopped() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(ExternalAgentPeerUpdateV1 {
        invocation_id: id("invocation.running"),
        native_session_id: "native-running".into(),
        continuation_cursor: Some("cursor.running".into()),
        events: vec![ExternalAgentRawEventV1 {
            sequence: 1,
            content: ExternalAgentRawContentV1::Progress("working".into()),
        }],
        approval_request: None,
        terminal: None,
        effect: ExternalEffectEvidenceV1::Started,
    }));
    peer.push_cancellation(ExternalCancellationEvidenceV1::Refused);
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");
    let update = manager
        .start(&id("agent.codex"), &start("invocation.running"))
        .expect("start");

    assert!(matches!(
        manager.continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.overlap"),
            native_session: update.native_session.clone(),
            input: "overlap".into(),
            deadline_epoch_millis: 20_000,
            expected_cursor: Some("cursor.running".into()),
            forwarded_mcp: None,
        }),
        Err(ExternalAgentError::InvocationOverlap)
    ));
    assert!(matches!(
        manager.close_session(&update.native_session),
        Err(ExternalAgentError::SessionBusy)
    ));

    let cancelled = manager
        .cancel(&update.native_session)
        .expect("conservative cancellation result");
    let terminal = cancelled.terminal.expect("terminal evidence");
    assert_eq!(terminal.disposition, OutcomeDispositionV1::OutcomeUncertain);
    assert_eq!(terminal.retry_safety, RetrySafetyV1::NotSafe);
    assert_eq!(
        peer.cancelled_invocations
            .lock()
            .expect("cancel correlation")
            .as_slice(),
        &[id("invocation.running")]
    );
    assert!(matches!(
        manager.cancel(&update.native_session),
        Err(ExternalAgentError::NoActiveInvocation)
    ));
}

#[test]
fn unknown_effect_failure_is_uncertain_and_blocks_continuation_until_close() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(ExternalAgentPeerUpdateV1 {
        invocation_id: id("invocation.unknown-effect"),
        native_session_id: "native-unknown-effect".into(),
        continuation_cursor: Some("cursor.unknown".into()),
        events: Vec::new(),
        approval_request: None,
        terminal: Some(ExternalTerminalV1 {
            status: ExternalTerminalStatusV1::Failed,
            result: None,
        }),
        effect: ExternalEffectEvidenceV1::Unknown,
    }));
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");
    let update = manager
        .start(&id("agent.codex"), &start("invocation.unknown-effect"))
        .expect("normalized terminal");
    let outcome = update.terminal.expect("outcome");
    assert_eq!(outcome.disposition, OutcomeDispositionV1::OutcomeUncertain);
    assert_eq!(outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert!(matches!(
        manager.continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.after-unknown"),
            native_session: update.native_session.clone(),
            input: "unsafe continuation".into(),
            deadline_epoch_millis: 20_000,
            expected_cursor: Some("cursor.unknown".into()),
            forwarded_mcp: None,
        }),
        Err(ExternalAgentError::ContinuationBlocked)
    ));
    manager
        .close_session(&update.native_session)
        .expect("explicit safe eviction");
    assert_eq!(peer.close_count.load(Ordering::SeqCst), 1);
}

#[test]
fn post_dispatch_wrong_correlation_returns_conservative_outcome_not_plain_validation_error() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(terminal_update(
        "invocation.forged-peer-correlation",
        "native-invalid-update",
        "cursor.invalid",
        "must not be accepted",
    )));
    let manager = manager(peer);
    manager.register_target(manifest()).expect("negotiate");
    let error = manager
        .start(&id("agent.codex"), &start("invocation.expected"))
        .expect_err("post-dispatch validation must settle conservatively");
    let ExternalAgentError::PeerOutcome { outcome, code, .. } = error else {
        panic!("post-dispatch validation leaked a plain unsafe error")
    };
    assert_eq!(outcome.disposition, OutcomeDispositionV1::OutcomeUncertain);
    assert_eq!(outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert!(code.starts_with("protocol_validation:"));
    assert!(manager.health(&id("agent.codex")).expect("health").degraded);
}

#[test]
fn settled_session_close_safely_releases_capacity_without_evicting_active_work() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(terminal_update(
        "invocation.capacity-1",
        "native-capacity-1",
        "cursor.capacity-1",
        "first",
    )));
    peer.push_start(Ok(terminal_update(
        "invocation.capacity-2",
        "native-capacity-2",
        "cursor.capacity-2",
        "second",
    )));
    let manager = manager(peer.clone());
    let mut one_session = manifest();
    one_session.maximum_active_sessions = 1;
    manager.register_target(one_session).expect("negotiate");
    let first = manager
        .start(&id("agent.codex"), &start("invocation.capacity-1"))
        .expect("first session");
    assert!(matches!(
        manager.start(&id("agent.codex"), &start("invocation.capacity-2")),
        Err(ExternalAgentError::SessionLimit)
    ));
    assert_eq!(peer.start_count.load(Ordering::SeqCst), 1);
    manager
        .close_session(&first.native_session)
        .expect("close settled session");
    assert_eq!(
        manager
            .health(&id("agent.codex"))
            .expect("health")
            .active_sessions,
        0
    );
    manager
        .start(&id("agent.codex"), &start("invocation.capacity-2"))
        .expect("capacity released");
    assert_eq!(peer.start_count.load(Ordering::SeqCst), 2);
    assert!(matches!(
        manager.close_session(&first.native_session),
        Err(ExternalAgentError::UnknownSession)
    ));
}

#[test]
fn reserved_cancellation_wins_over_a_late_continuation_and_discards_approval() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(terminal_update(
        "invocation.race.start",
        "native-race",
        "cursor.race.start",
        "ready",
    )));
    peer.push_continuation(ExternalAgentPeerUpdateV1 {
        invocation_id: id("invocation.race.continue"),
        native_session_id: "native-race".into(),
        continuation_cursor: Some("cursor.race.late".into()),
        events: Vec::new(),
        approval_request: Some(ExternalApprovalRequestV1 {
            request_id: id("approval.race.late"),
            invocation_id: id("invocation.race.continue"),
            summary: "must be discarded after cancellation reservation".into(),
            requested_scopes: vec!["files.write".into()],
        }),
        terminal: None,
        effect: ExternalEffectEvidenceV1::Started,
    });
    peer.push_cancellation(ExternalCancellationEvidenceV1::Refused);
    let manager = Arc::new(manager(peer.clone()));
    manager.register_target(manifest()).expect("negotiate");
    let initial = manager
        .start(&id("agent.codex"), &start("invocation.race.start"))
        .expect("start");

    let continuation_gate = ScriptedAgentPeer::install_gate(&peer.continuation_gate);
    let cancellation_gate = ScriptedAgentPeer::install_gate(&peer.cancellation_gate);
    let continue_manager = manager.clone();
    let continue_reference = initial.native_session.clone();
    let continue_thread = thread::spawn(move || {
        continue_manager.continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.race.continue"),
            native_session: continue_reference,
            input: "continue while cancellable".into(),
            deadline_epoch_millis: 20_000,
            expected_cursor: Some("cursor.race.start".into()),
            forwarded_mcp: None,
        })
    });
    continuation_gate.wait_until_entered();

    let cancel_manager = manager.clone();
    let cancel_reference = initial.native_session.clone();
    let cancel_thread = thread::spawn(move || cancel_manager.cancel(&cancel_reference));
    cancellation_gate.wait_until_entered();
    continuation_gate.release();
    assert!(matches!(
        continue_thread.join().expect("continuation thread"),
        Err(ExternalAgentError::ControlInFlight)
    ));
    let late_resolution = ExternalApprovalResolutionV1::issue(
        CORE_KEY,
        id("approval.race.late"),
        id("decision.race.late"),
        ProcessGeneration(9),
        initial.native_session.clone(),
        id("invocation.race.continue"),
        ExternalApprovalDecisionV1::Approved,
        vec!["files.write".into()],
    )
    .expect("signed late resolution");
    assert!(matches!(
        manager.resolve_approval(&late_resolution),
        Err(ExternalAgentError::ControlInFlight)
    ));
    cancellation_gate.release();
    let cancellation = cancel_thread
        .join()
        .expect("cancellation thread")
        .expect("sole terminal");
    assert_eq!(
        cancellation
            .terminal
            .expect("cancellation outcome")
            .disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert!(!manager.health(&id("agent.codex")).expect("health").degraded);
    assert!(matches!(
        manager.cancel(&initial.native_session),
        Err(ExternalAgentError::NoActiveInvocation)
    ));
    assert_eq!(peer.approval_count.load(Ordering::SeqCst), 0);
}

#[test]
fn target_and_start_reservations_close_registration_and_capacity_races() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    let negotiation_gate = ScriptedAgentPeer::install_gate(&peer.negotiation_gate);
    let manager = Arc::new(manager(peer.clone()));
    let mut one_session = manifest();
    one_session.maximum_active_sessions = 1;
    let registration_manager = manager.clone();
    let registration_manifest = one_session.clone();
    let registration =
        thread::spawn(move || registration_manager.register_target(registration_manifest));
    negotiation_gate.wait_until_entered();
    assert!(matches!(
        manager.register_target(one_session.clone()),
        Err(ExternalAgentError::TargetRegistrationInProgress)
    ));
    assert!(matches!(
        manager.health(&id("agent.codex")),
        Err(ExternalAgentError::TargetRegistrationInProgress)
    ));
    assert_eq!(peer.negotiation_count.load(Ordering::SeqCst), 1);
    negotiation_gate.release();
    registration
        .join()
        .expect("registration thread")
        .expect("registration");

    peer.push_start(Ok(terminal_update(
        "invocation.reserved.start",
        "native-reserved-start",
        "cursor.reserved.start",
        "reserved",
    )));
    let start_gate = ScriptedAgentPeer::install_gate(&peer.start_gate);
    let start_manager = manager.clone();
    let first_start = thread::spawn(move || {
        start_manager.start(&id("agent.codex"), &start("invocation.reserved.start"))
    });
    start_gate.wait_until_entered();
    let health = manager.health(&id("agent.codex")).expect("health");
    assert_eq!(health.active_sessions, 1);
    assert_eq!(health.reserved_sessions, 1);
    assert!(matches!(
        manager.start(&id("agent.codex"), &start("invocation.racing.start")),
        Err(ExternalAgentError::SessionLimit)
    ));
    assert_eq!(peer.start_count.load(Ordering::SeqCst), 1);
    start_gate.release();
    first_start
        .join()
        .expect("start thread")
        .expect("reserved start");
    let health = manager.health(&id("agent.codex")).expect("health");
    assert_eq!(health.active_sessions, 1);
    assert_eq!(health.reserved_sessions, 0);
}

#[test]
fn retired_invocation_capacity_fails_closed_and_requires_explicit_close() {
    let peer = Arc::new(ScriptedAgentPeer::default());
    peer.push_negotiation(negotiation());
    peer.push_start(Ok(terminal_update(
        "invocation.retired.0",
        "native-retired-capacity",
        "cursor.retired.0",
        "initial",
    )));
    for index in 1..=4096 {
        peer.push_continuation(terminal_update(
            &format!("invocation.retired.{index}"),
            "native-retired-capacity",
            &format!("cursor.retired.{index}"),
            "settled",
        ));
    }
    let manager = manager(peer.clone());
    manager.register_target(manifest()).expect("negotiate");
    let session = manager
        .start(&id("agent.codex"), &start("invocation.retired.0"))
        .expect("start");
    for index in 1..=4096 {
        manager
            .continue_session(&ExternalAgentContinueV1 {
                invocation_id: id(&format!("invocation.retired.{index}")),
                native_session: session.native_session.clone(),
                input: "bounded retirement".into(),
                deadline_epoch_millis: 20_000,
                expected_cursor: Some(format!("cursor.retired.{}", index - 1)),
                forwarded_mcp: None,
            })
            .expect("settled continuation");
    }
    let health = manager.health(&id("agent.codex")).expect("health");
    assert_eq!(health.sessions_requiring_close, 1);
    assert!(matches!(
        manager.continue_session(&ExternalAgentContinueV1 {
            invocation_id: id("invocation.retired.overflow"),
            native_session: session.native_session.clone(),
            input: "must fail closed".into(),
            deadline_epoch_millis: 20_000,
            expected_cursor: Some("cursor.retired.4096".into()),
            forwarded_mcp: None,
        }),
        Err(ExternalAgentError::RetiredInvocationCapacity)
    ));
    manager
        .close_session(&session.native_session)
        .expect("explicit close after retirement capacity");
    assert_eq!(peer.close_count.load(Ordering::SeqCst), 1);
}
