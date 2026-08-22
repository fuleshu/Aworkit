use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use aworkit_capability_host::{
    McpCallKindV1, McpCallV1, McpCancellationEvidenceV1, McpCatalogV1, McpDispatchMilestoneV1,
    McpFeatureSetV1, McpInitializeRequestV1, McpInitializeResponseV1, McpPeerCallResultV1,
    McpPeerErrorV1, McpPeerPort, McpProgressV1, McpServerManifestV1, McpSessionError,
    McpSessionManager, McpToolDescriptorV1, McpTransportKindV1, OutcomeDispositionV1,
    RetrySafetyV1,
};
use aworkit_protocol::{ProcessGeneration, StableId};

const HASH_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("test ID")
}

fn manifest() -> McpServerManifestV1 {
    McpServerManifestV1 {
        server_id: id("mcp.github"),
        adapter_version: "1.0.0".into(),
        binding_hash: HASH_A.into(),
        host_generation: ProcessGeneration(9),
        configured: true,
        enabled: true,
        core_attested: true,
        transport: McpTransportKindV1::Stdio,
        minimum_protocol_version: 1,
        maximum_protocol_version: 2,
        maximum_in_flight: 4,
        maximum_progress_events: 8,
        secret_slots: vec!["token".into()],
        workspace_roots: vec!["workspace".into()],
    }
}

fn initialize_response() -> McpInitializeResponseV1 {
    McpInitializeResponseV1 {
        server_id: id("mcp.github"),
        protocol_version: 2,
        features: McpFeatureSetV1 {
            tools: true,
            resources: true,
            prompts: true,
            progress: true,
            cancellation: true,
        },
        catalog: McpCatalogV1 {
            tools: vec![McpToolDescriptorV1 {
                name: "issues.create".into(),
                input_schema_hash: HASH_A.into(),
                side_effect_known_read_only: false,
            }],
            resources: vec!["repo://current".into()],
            prompts: vec!["review".into()],
        },
    }
}

fn tool_call(invocation_id: &str, schema_hash: &str) -> McpCallV1 {
    McpCallV1 {
        invocation_id: id(invocation_id),
        kind: McpCallKindV1::Tool,
        name: "issues.create".into(),
        expected_schema_hash: Some(schema_hash.into()),
        arguments: serde_json::json!({"title": "bounded"}),
    }
}

#[derive(Default)]
struct ScriptedPeer {
    initializations: Mutex<VecDeque<McpInitializeResponseV1>>,
    calls: Mutex<VecDeque<Result<McpPeerCallResultV1, McpPeerErrorV1>>>,
    invoke_count: AtomicUsize,
}

impl ScriptedPeer {
    fn push_initialize(&self, response: McpInitializeResponseV1) {
        self.initializations
            .lock()
            .expect("initialize script")
            .push_back(response);
    }

    fn push_call(&self, response: Result<McpPeerCallResultV1, McpPeerErrorV1>) {
        self.calls.lock().expect("call script").push_back(response);
    }
}

impl McpPeerPort for ScriptedPeer {
    fn initialize(
        &self,
        _manifest: &McpServerManifestV1,
        _request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
        self.initializations
            .lock()
            .expect("initialize script")
            .pop_front()
            .ok_or_else(|| McpPeerErrorV1 {
                code: "script_exhausted".into(),
                message: "no initialize response".into(),
                dispatch: McpDispatchMilestoneV1::DefinitelyNotStarted,
                transport_lost: false,
            })
    }

    fn invoke(
        &self,
        _manifest: &McpServerManifestV1,
        _call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
        self.invoke_count.fetch_add(1, Ordering::SeqCst);
        self.calls
            .lock()
            .expect("call script")
            .pop_front()
            .expect("scripted call")
    }

    fn cancel(
        &self,
        _manifest: &McpServerManifestV1,
        _invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
        Ok(McpCancellationEvidenceV1::ConfirmedAfterStart)
    }

    fn close(&self, _manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        Ok(())
    }
}

#[test]
fn exact_attestation_negotiates_discovery_and_reuses_one_session() {
    let peer = Arc::new(ScriptedPeer::default());
    peer.push_initialize(initialize_response());
    peer.push_initialize(initialize_response());
    peer.push_call(Ok(McpPeerCallResultV1 {
        result: serde_json::json!({"issue": 41}),
        progress: vec![
            McpProgressV1 {
                sequence: 1,
                message: "accepted".into(),
            },
            McpProgressV1 {
                sequence: 2,
                message: "created".into(),
            },
        ],
    }));
    let manager = McpSessionManager::new(ProcessGeneration(9), peer);

    let snapshot = manager.open(manifest()).expect("initialize");
    let reused = manager.open(manifest()).expect("exact reuse");
    assert_eq!(snapshot, reused);
    assert_eq!(snapshot.protocol_version, 2);
    assert_eq!(snapshot.catalog.tools[0].name, "issues.create");

    let outcome = manager
        .invoke(&id("mcp.github"), &tool_call("invocation.1", HASH_A))
        .expect("call");
    assert_eq!(outcome.outcome.disposition, OutcomeDispositionV1::Succeeded);
    assert_eq!(outcome.progress.len(), 2);
    assert_eq!(outcome.result, Some(serde_json::json!({"issue": 41})));
}

#[test]
fn schema_drift_and_disabled_or_stale_manifests_fail_before_dispatch() {
    let peer = Arc::new(ScriptedPeer::default());
    peer.push_initialize(initialize_response());
    let manager = McpSessionManager::new(ProcessGeneration(9), peer.clone());
    manager.open(manifest()).expect("initialize");

    assert!(matches!(
        manager.invoke(&id("mcp.github"), &tool_call("invocation.2", HASH_B)),
        Err(McpSessionError::SchemaDrift)
    ));
    assert_eq!(peer.invoke_count.load(Ordering::SeqCst), 0);

    let disabled_manager =
        McpSessionManager::new(ProcessGeneration(9), Arc::new(ScriptedPeer::default()));
    let mut disabled = manifest();
    disabled.enabled = false;
    assert!(matches!(
        disabled_manager.open(disabled),
        Err(McpSessionError::Disabled)
    ));
    let mut stale = manifest();
    stale.host_generation = ProcessGeneration(8);
    assert!(matches!(
        disabled_manager.open(stale),
        Err(McpSessionError::StaleAttestation)
    ));
}

#[test]
fn ambiguous_disconnect_is_not_replayed_and_reconnect_requires_same_catalog() {
    let peer = Arc::new(ScriptedPeer::default());
    peer.push_initialize(initialize_response());
    peer.push_initialize(initialize_response());
    peer.push_call(Err(McpPeerErrorV1 {
        code: "disconnected".into(),
        message: "transport lost after write".into(),
        dispatch: McpDispatchMilestoneV1::Started,
        transport_lost: true,
    }));
    let manager = McpSessionManager::new(ProcessGeneration(9), peer.clone());
    manager.open(manifest()).expect("initialize");

    let outcome = manager
        .invoke(&id("mcp.github"), &tool_call("invocation.3", HASH_A))
        .expect("conservative settlement");
    assert_eq!(
        outcome.outcome.disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert_eq!(outcome.outcome.retry_safety, RetrySafetyV1::NotSafe);
    assert!(outcome.evidence.transport_lost);
    assert_eq!(peer.invoke_count.load(Ordering::SeqCst), 1);
    assert!(matches!(
        manager.invoke(&id("mcp.github"), &tool_call("invocation.4", HASH_A)),
        Err(McpSessionError::SessionDegraded)
    ));

    manager.reconnect(&id("mcp.github")).expect("reconnect");
    assert_eq!(peer.invoke_count.load(Ordering::SeqCst), 1);
}

#[test]
fn settled_ids_and_post_dispatch_protocol_violations_never_replay() {
    let peer = Arc::new(ScriptedPeer::default());
    peer.push_initialize(initialize_response());
    peer.push_initialize(initialize_response());
    peer.push_call(Ok(McpPeerCallResultV1 {
        result: serde_json::json!({"ignored": true}),
        progress: vec![
            McpProgressV1 {
                sequence: 2,
                message: "out of order".into(),
            },
            McpProgressV1 {
                sequence: 1,
                message: "invalid".into(),
            },
        ],
    }));
    let manager = McpSessionManager::new(ProcessGeneration(9), peer.clone());
    manager.open(manifest()).expect("initialize");

    let outcome = manager
        .invoke(
            &id("mcp.github"),
            &tool_call("invocation.protocol-violation", HASH_A),
        )
        .expect("post-dispatch violations are evidence-bearing outcomes");
    assert_eq!(
        outcome.outcome.disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert_eq!(outcome.outcome.retry_safety, RetrySafetyV1::NotSafe);
    manager
        .reconnect(&id("mcp.github"))
        .expect("explicit reconnect after protocol violation");
    assert!(matches!(
        manager.invoke(
            &id("mcp.github"),
            &tool_call("invocation.protocol-violation", HASH_A),
        ),
        Err(McpSessionError::InvocationAlreadySettled)
    ));
    assert_eq!(peer.invoke_count.load(Ordering::SeqCst), 1);
}

struct BlockingPeer {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
    cancellation_evidence: McpCancellationEvidenceV1,
    invoke_dispatch: McpDispatchMilestoneV1,
}

impl BlockingPeer {
    fn new() -> Self {
        Self::with_evidence(
            McpCancellationEvidenceV1::ConfirmedAfterStart,
            McpDispatchMilestoneV1::Started,
        )
    }

    fn with_evidence(
        cancellation_evidence: McpCancellationEvidenceV1,
        invoke_dispatch: McpDispatchMilestoneV1,
    ) -> Self {
        Self {
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
            cancellation_evidence,
            invoke_dispatch,
        }
    }

    fn wait_until_started(&self) {
        let state = self.state.lock().expect("blocking peer");
        drop(
            self.changed
                .wait_while(state, |(started, _)| !*started)
                .expect("wait for invoke"),
        );
    }
}

impl McpPeerPort for BlockingPeer {
    fn initialize(
        &self,
        _manifest: &McpServerManifestV1,
        _request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
        Ok(initialize_response())
    }

    fn invoke(
        &self,
        _manifest: &McpServerManifestV1,
        _call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
        let mut state = self.state.lock().expect("blocking peer");
        state.0 = true;
        self.changed.notify_all();
        drop(
            self.changed
                .wait_while(state, |(_, released)| !*released)
                .expect("wait for cancel"),
        );
        Err(McpPeerErrorV1 {
            code: "cancelled".into(),
            message: "peer stopped after cancellation".into(),
            dispatch: self.invoke_dispatch,
            transport_lost: false,
        })
    }

    fn cancel(
        &self,
        _manifest: &McpServerManifestV1,
        _invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
        let mut state = self.state.lock().expect("blocking peer");
        state.1 = true;
        self.changed.notify_all();
        Ok(self.cancellation_evidence)
    }

    fn close(&self, _manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        Ok(())
    }
}

#[test]
fn cancellation_uses_reserved_control_path_and_requires_positive_evidence() {
    let peer = Arc::new(BlockingPeer::new());
    let manager = Arc::new(McpSessionManager::new(ProcessGeneration(9), peer.clone()));
    manager.open(manifest()).expect("initialize");
    let worker_manager = manager.clone();
    let worker = thread::spawn(move || {
        worker_manager.invoke(&id("mcp.github"), &tool_call("invocation.cancel", HASH_A))
    });
    peer.wait_until_started();

    let cancellation = manager
        .cancel(&id("mcp.github"), &id("invocation.cancel"))
        .expect("cancel control");
    assert_eq!(
        cancellation.evidence,
        McpCancellationEvidenceV1::ConfirmedAfterStart
    );
    assert_eq!(cancellation.invocation_id, id("invocation.cancel"));
    let terminal = worker.join().expect("worker thread").expect("settlement");
    assert_eq!(
        terminal.outcome.disposition,
        OutcomeDispositionV1::CancelledWithEvidence
    );
    assert_eq!(terminal.outcome.retry_safety, RetrySafetyV1::NotSafe);
}

#[test]
fn contradictory_cancellation_and_invoke_evidence_is_uncertain_not_retryable() {
    let peer = Arc::new(BlockingPeer::with_evidence(
        McpCancellationEvidenceV1::ConfirmedBeforeEffect,
        McpDispatchMilestoneV1::Started,
    ));
    let manager = Arc::new(McpSessionManager::new(ProcessGeneration(9), peer.clone()));
    manager.open(manifest()).expect("initialize");
    let worker_manager = manager.clone();
    let worker = thread::spawn(move || {
        worker_manager.invoke(
            &id("mcp.github"),
            &tool_call("invocation.cancel-conflict", HASH_A),
        )
    });
    peer.wait_until_started();

    let receipt = manager
        .cancel(&id("mcp.github"), &id("invocation.cancel-conflict"))
        .expect("cancel receipt");
    assert_eq!(
        receipt.evidence,
        McpCancellationEvidenceV1::ConfirmedBeforeEffect
    );
    let terminal = worker.join().expect("worker thread").expect("settlement");
    assert_eq!(
        terminal.outcome.disposition,
        OutcomeDispositionV1::OutcomeUncertain
    );
    assert_eq!(terminal.outcome.retry_safety, RetrySafetyV1::NotSafe);
}

#[test]
fn closed_session_retains_replay_tombstones_until_generation_rotation() {
    let peer = Arc::new(ScriptedPeer::default());
    peer.push_initialize(initialize_response());
    let manager = McpSessionManager::new(ProcessGeneration(9), peer);
    manager.open(manifest()).expect("initialize");
    manager.close(&id("mcp.github")).expect("close");

    assert!(manager.health(&id("mcp.github")).expect("health").retired);
    assert!(matches!(
        manager.open(manifest()),
        Err(McpSessionError::SessionRetired)
    ));
}

struct BlockingInitializePeer {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
    initialize_count: AtomicUsize,
}

impl BlockingInitializePeer {
    fn new() -> Self {
        Self {
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
            initialize_count: AtomicUsize::new(0),
        }
    }

    fn wait_until_started(&self) {
        let state = self.state.lock().expect("initialize peer");
        drop(
            self.changed
                .wait_while(state, |(started, _)| !*started)
                .expect("wait for initialize"),
        );
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("initialize peer");
        state.1 = true;
        self.changed.notify_all();
    }
}

impl McpPeerPort for BlockingInitializePeer {
    fn initialize(
        &self,
        _manifest: &McpServerManifestV1,
        _request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
        self.initialize_count.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().expect("initialize peer");
        state.0 = true;
        self.changed.notify_all();
        drop(
            self.changed
                .wait_while(state, |(_, released)| !*released)
                .expect("wait for release"),
        );
        Ok(initialize_response())
    }

    fn invoke(
        &self,
        _manifest: &McpServerManifestV1,
        _call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
        unreachable!("open race fixture never invokes")
    }

    fn cancel(
        &self,
        _manifest: &McpServerManifestV1,
        _invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
        unreachable!("open race fixture never cancels")
    }

    fn close(&self, _manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        Ok(())
    }
}

#[test]
fn concurrent_open_cannot_overwrite_or_double_initialize_a_binding() {
    let peer = Arc::new(BlockingInitializePeer::new());
    let manager = Arc::new(McpSessionManager::with_limits(
        ProcessGeneration(9),
        peer.clone(),
        1,
        2,
    ));
    let first_manager = manager.clone();
    let first = thread::spawn(move || first_manager.open(manifest()));
    peer.wait_until_started();

    let second_manager = manager.clone();
    let second = thread::spawn(move || {
        let mut drifted = manifest();
        drifted.binding_hash = HASH_B.into();
        second_manager.open(drifted)
    });
    peer.release();

    first.join().expect("first thread").expect("first open");
    assert!(matches!(
        second.join().expect("second thread"),
        Err(McpSessionError::BindingDrift)
    ));
    assert_eq!(peer.initialize_count.load(Ordering::SeqCst), 1);
}

struct SelectiveBlockingInitializePeer {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl SelectiveBlockingInitializePeer {
    fn new() -> Self {
        Self {
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
        }
    }

    fn wait_until_blocked(&self) {
        let state = self.state.lock().expect("selective initialize peer");
        drop(
            self.changed
                .wait_while(state, |(started, _)| !*started)
                .expect("wait for blocked initialize"),
        );
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("selective initialize peer");
        state.1 = true;
        self.changed.notify_all();
    }
}

impl McpPeerPort for SelectiveBlockingInitializePeer {
    fn initialize(
        &self,
        manifest: &McpServerManifestV1,
        _request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
        if manifest.server_id.as_str() == "mcp.blocked" {
            let mut state = self.state.lock().expect("selective initialize peer");
            state.0 = true;
            self.changed.notify_all();
            drop(
                self.changed
                    .wait_while(state, |(_, released)| !*released)
                    .expect("wait for release"),
            );
        }
        let mut response = initialize_response();
        response.server_id = manifest.server_id.clone();
        Ok(response)
    }

    fn invoke(
        &self,
        _manifest: &McpServerManifestV1,
        _call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
        unreachable!("initialization liveness fixture never invokes")
    }

    fn cancel(
        &self,
        _manifest: &McpServerManifestV1,
        _invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
        unreachable!("initialization liveness fixture never cancels")
    }

    fn close(&self, _manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        Ok(())
    }
}

#[test]
fn hung_initialization_does_not_block_unrelated_session_health() {
    let peer = Arc::new(SelectiveBlockingInitializePeer::new());
    let manager = Arc::new(McpSessionManager::with_limits(
        ProcessGeneration(9),
        peer.clone(),
        2,
        2,
    ));
    manager.open(manifest()).expect("open healthy session");

    let blocked_manager = manager.clone();
    let blocked = thread::spawn(move || {
        let mut blocked_manifest = manifest();
        blocked_manifest.server_id = id("mcp.blocked");
        blocked_manager.open(blocked_manifest)
    });
    peer.wait_until_blocked();

    let (sent, received) = mpsc::channel();
    let health_manager = manager.clone();
    let health = thread::spawn(move || {
        sent.send(health_manager.health(&id("mcp.github")))
            .expect("send health result");
    });
    let health_result = received.recv_timeout(Duration::from_millis(250));
    peer.release();
    assert!(
        health_result
            .expect("unrelated health must not wait for peer initialization")
            .is_ok()
    );
    health.join().expect("health thread");
    blocked
        .join()
        .expect("blocked open thread")
        .expect("blocked server opens after release");
}
