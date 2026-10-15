//! The desktop command boundary must accept answers from a lagging projection.
use super::*;

fn setup() -> (TempDir, DesktopRuntime, Arc<QuestionPipeline>) {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline::new(provider));
    core.pipeline = pipeline.clone();
    core.command(send("question.start", 0, "Ask me")).unwrap();
    (root, core, pipeline)
}

fn action(core: &DesktopRuntime, id: &str, kind: &str, payload: Value) -> UiCommandInput {
    UiCommandInput {
        schema_version: 1,
        command_id: id.into(),
        expected_version: core.history.head().unwrap(),
        action: kind.into(),
        target_id: None,
        payload,
    }
}

#[test]
fn interrupted_question_recovery_retries_failed_resume_and_persists_its_receipt() {
    let (root, mut core, pipeline) = setup();
    let answer = action(
        &core,
        "answer",
        "question",
        json!({"questionId":"question.fixture","optionId":"beta"}),
    );
    pipeline.fail_resume.store(true, Ordering::SeqCst);
    assert!(core.command(answer).unwrap_err().contains("simulated"));
    assert!(core.snapshot(0).unwrap().chat.recovery_pending);
    let resume = action(&core, "resume", "resume", json!({}));
    pipeline.fail_resume.store(true, Ordering::SeqCst);
    assert!(
        core.command(resume.clone())
            .unwrap_err()
            .contains("simulated")
    );
    assert!(core.snapshot(0).unwrap().chat.recovery_pending);
    let receipt = core.command(resume.clone()).unwrap();
    assert_eq!(receipt.command_id, "resume");
    assert!(!core.snapshot(0).unwrap().chat.recovery_pending);
    drop(core);
    let mut reopened = runtime(&root, pipeline.provider.clone());
    reopened.pipeline = pipeline.clone();
    let replayed = reopened.command(resume).unwrap();
    assert_eq!(replayed.command_id, receipt.command_id);
    assert_eq!(replayed.current_version, receipt.current_version);
    assert!(replayed.accepted);
    assert_eq!(pipeline.answers.lock().unwrap().len(), 1);
    assert_eq!(
        reopened
            .snapshot(0)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.kind == "question.answered")
            .count(),
        1
    );
}

#[test]
fn stop_closes_question_and_legacy_stops_reject_late_answers_without_staging() {
    for legacy in [false, true] {
        let (_root, mut core, pipeline) = setup();
        if legacy {
            core.history
                .append(
                    "legacy.stop",
                    "hash",
                    core.history.head().unwrap(),
                    vec![("chat.turn_stopped", json!({"createdAt":"now"}))],
                )
                .unwrap();
        } else {
            core.command(action(&core, "stop", "cancel", json!({})))
                .unwrap();
            assert!(
                core.snapshot(0)
                    .unwrap()
                    .events
                    .iter()
                    .any(|event| event.kind == "question.cancelled")
            );
        }
        let head = core.history.head().unwrap();
        assert_eq!(core.snapshot(0).unwrap().chat.phase, "waiting_input");
        let answer = action(
            &core,
            "late.answer",
            "question",
            json!({"questionId":"question.fixture","optionId":"beta"}),
        );
        assert!(
            core.command(answer)
                .unwrap_err()
                .contains("no longer waiting")
        );
        assert_eq!(core.history.head().unwrap(), head);
        assert!(!core.snapshot(0).unwrap().chat.recovery_pending);
        assert!(pipeline.answers.lock().unwrap().is_empty());
    }
}

#[test]
fn recovery_abandons_question_approval_and_maintenance_without_text_or_effects() {
    for (kind, payload) in [
        (
            "question",
            json!({"questionId":"question.fixture","optionId":"beta"}),
        ),
        (
            "approval",
            json!({"decisionId":"approval.fixture","approved":true}),
        ),
        (
            "compact_context",
            json!({"nodeId":"agent.1","baseSequence":1}),
        ),
    ] {
        let (_root, mut core, pipeline) = setup();
        let command = action(&core, "interrupted", kind, payload);
        let frozen = core.history.current_frozen_context().unwrap().unwrap();
        core.history
            .stage_effect_command(PendingChatCommandV1 {
                schema_version: 1,
                frozen_context_hash: frozen.context_hash,
                command_hash: command_fingerprint(&command).unwrap(),
                command,
            })
            .unwrap();
        let before = core.snapshot(0).unwrap();
        core.command(action(
            &core,
            "stop.recovery",
            "abandon_recovery",
            json!({}),
        ))
        .unwrap();
        let after = core.snapshot(0).unwrap();
        assert!(!after.chat.recovery_pending);
        assert_eq!(after.chat.phase, "waiting_input");
        assert_eq!(pipeline.provider.calls.load(Ordering::SeqCst), 1);
        assert!(pipeline.answers.lock().unwrap().is_empty());
        assert_eq!(
            after
                .events
                .iter()
                .filter(|event| event.kind == "message.user")
                .count(),
            before
                .events
                .iter()
                .filter(|event| event.kind == "message.user")
                .count()
        );
    }
}

struct QuestionPipeline {
    provider: Arc<FixtureProvider>,
    result: Mutex<Option<WorkflowExecutionResultV1>>,
    answers: Mutex<Vec<Value>>,
    fail_resume: AtomicBool,
    /// One entry per frozen-endpoint installation, so a test can prove the
    /// Chat's MCP transport set was installed for a resumed pass.
    mcp_preparations: Mutex<Vec<String>>,
    /// The tool name and schema the fixture server advertises, so a test can
    /// edit the server between two application generations.
    mcp_tool_name: Mutex<String>,
    mcp_input_schema: Mutex<Value>,
    /// The connections this Run installed, so a test can simulate the call that
    /// opens a session.
    mcp_installed: Mutex<Vec<McpServerManifestV1>>,
    /// Discovery snapshots of the sessions a *call* opened. Empty until a test
    /// simulates one: starting a Chat and assembling a pass never open a
    /// session.
    mcp_open_sessions: Mutex<BTreeMap<String, McpCapabilitySnapshotV1>>,
    /// Every session open (one per tool call that reached the server), so a test
    /// can prove the message path never contacts an MCP server.
    mcp_opens: AtomicUsize,
    /// Every preflighted request, so a test can inspect the interface the pass
    /// was actually offered.
    preflight_requests: Mutex<Vec<WorkflowExecutionRequestV1>>,
}

impl QuestionPipeline {
    fn new(provider: Arc<FixtureProvider>) -> Self {
        Self {
            provider,
            result: Mutex::new(None),
            answers: Mutex::new(Vec::new()),
            fail_resume: AtomicBool::new(false),
            mcp_preparations: Mutex::new(Vec::new()),
            mcp_tool_name: Mutex::new("echo".into()),
            mcp_input_schema: Mutex::new(fixture_mcp_input_schema()),
            mcp_installed: Mutex::new(Vec::new()),
            mcp_open_sessions: Mutex::new(BTreeMap::new()),
            mcp_opens: AtomicUsize::new(0),
            preflight_requests: Mutex::new(Vec::new()),
        }
    }

    /// Simulates the tool call that reaches the server: the session opens and its
    /// discovery snapshot — the interface the fixture server advertises *now* —
    /// becomes available to later passes.
    fn open_mcp_session(&self) {
        let tool_name = self.mcp_tool_name.lock().unwrap().clone();
        let input_schema = self.mcp_input_schema.lock().unwrap().clone();
        let installed = self.mcp_installed.lock().unwrap().clone();
        let mut sessions = self.mcp_open_sessions.lock().unwrap();
        for manifest in installed {
            sessions.insert(
                manifest.server_id.to_string(),
                frozen_mcp_snapshot(&manifest, &tool_name, &input_schema),
            );
        }
        self.mcp_opens.fetch_add(1, Ordering::SeqCst);
    }
}

impl WorkflowPipelinePort for QuestionPipeline {
    fn preflight(&self, request: &WorkflowExecutionRequestV1) -> Result<(), String> {
        self.preflight_requests
            .lock()
            .unwrap()
            .push(request.clone());
        Ok(())
    }

    fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, String> {
        let mut result = FixtureWorkflowPipeline {
            provider: self.provider.clone(),
            goal: Mutex::new(None),
        }
        .execute(request)?;
        *self.result.lock().unwrap() = Some(result.clone());
        result.status = WorkflowExecutionStatusV1::AwaitingAnswer;
        result.approval = Some(
            serde_json::from_value(json!({
                "decisionId": "question.fixture",
                "nodeId": "agent.1",
                "title": "Release channel",
                "message": "Choose a channel",
                "question": {
                    "kind": "choice",
                    "prompt": "Choose a channel",
                    "options": [{"id": "beta", "label": "Beta"}]
                }
            }))
            .unwrap(),
        );
        Ok(result)
    }

    /// Installs this Run's frozen transports and answers with the discovery
    /// snapshots an earlier *call* left behind.
    ///
    /// This mirrors production: installing is local work, and the pass never
    /// opens a session, so a server that is closed — or merely slow — costs the
    /// message path nothing.
    fn prepare_mcp_sessions(
        &self,
        run_id: &StableId,
        servers: &mut [McpRunServerPreparationV1],
    ) -> Result<(Vec<McpCapabilitySnapshotV1>, Vec<String>), String> {
        self.mcp_preparations
            .lock()
            .unwrap()
            .push(run_id.as_str().to_owned());
        *self.mcp_installed.lock().unwrap() = servers
            .iter()
            .map(|server| server.manifest.clone())
            .collect();
        Ok((
            self.mcp_open_sessions
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect(),
            Vec::new(),
        ))
    }

    fn validate_question_target(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &Value,
    ) -> Result<(), String> {
        let result = self.result.lock().unwrap();
        if question_id != "question.fixture"
            || result.as_ref().unwrap().chat_id.as_str() != chat_id
            || answer["optionId"] != "beta"
        {
            return Err("invalid question answer".into());
        }
        Ok(())
    }

    fn resume_question(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &Value,
    ) -> Result<WorkflowExecutionResultV1, String> {
        self.validate_question_target(question_id, chat_id, answer)?;
        if self.fail_resume.swap(false, Ordering::SeqCst) {
            return Err("simulated interrupted answer".into());
        }
        self.answers.lock().unwrap().push(answer.clone());
        Ok(self.result.lock().unwrap().clone().unwrap())
    }
}

#[test]
fn stale_question_answer_commits_once_and_resumes_its_chat() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline::new(provider));
    core.pipeline = pipeline.clone();
    core.command(send("question.start", 0, "Ask me")).unwrap();
    let waiting = core.snapshot(0).unwrap();
    assert_eq!(waiting.chat.phase, "awaiting_answer");
    let answer = UiCommandInput {
        schema_version: 1,
        command_id: "question.answer".into(),
        // Live question delivery can precede the snapshot which advances this.
        expected_version: 0,
        action: "question".into(),
        target_id: Some(waiting.chat.chat_id),
        payload: json!({"questionId": "question.fixture", "optionId": "beta"}),
    };
    let receipt = core.command(answer.clone()).expect("answer must commit");
    assert!(receipt.accepted);
    assert!(core.command(answer).unwrap().accepted);
    let settled = core.snapshot(0).unwrap();
    assert_eq!(settled.chat.phase, "waiting_input");
    assert!(!settled.chat.recovery_pending);
    assert_eq!(pipeline.answers.lock().unwrap().len(), 1);
    assert_eq!(
        settled
            .events
            .iter()
            .filter(|event| event.kind == "question.answered")
            .count(),
        1
    );
}

use aworkit_capability_host::{
    McpCatalogV1, McpFeatureSetV1, McpServerManifestV1, McpToolDescriptorV1,
};

/// The schema the fixture MCP server's `echo` tool starts with. A test edits
/// [`QuestionPipeline::mcp_input_schema`] to stand in for a server change.
fn fixture_mcp_input_schema() -> Value {
    json!({"type":"object","properties":{"text":{"type":"string"}}})
}

/// The catalog entry saved with the server's Settings connection: what describes
/// its tool until a call has reached the running server.
fn fixture_mcp_saved_schema() -> Value {
    json!({"type":"object"})
}

/// The discovery snapshot the fixture MCP server hands back: one read-only tool
/// whose interface the service resolves into the Chat's pass.
fn frozen_mcp_snapshot(
    manifest: &McpServerManifestV1,
    tool_name: &str,
    input_schema: &Value,
) -> McpCapabilitySnapshotV1 {
    McpCapabilitySnapshotV1 {
        server_id: manifest.server_id.clone(),
        host_generation: manifest.host_generation,
        binding_hash: manifest.binding_hash.clone(),
        protocol_version: 1,
        features: McpFeatureSetV1 {
            tools: true,
            resources: false,
            prompts: false,
            progress: false,
            cancellation: false,
        },
        catalog: McpCatalogV1 {
            tools: vec![McpToolDescriptorV1 {
                name: tool_name.into(),
                input_schema_hash: format!("sha256:question-fixture-{tool_name}"),
                side_effect_known_read_only: true,
                annotations: None,
                description: "Echo".into(),
                input_schema: input_schema.clone(),
            }],
            resources: Vec::new(),
            prompts: Vec::new(),
        },
        catalog_hash: "sha256:question-fixture-catalog".into(),
    }
}

/// Saves one MCP server and binds one of its tools in the default workflow, so a
/// Chat freezes a real MCP transport configuration, manifest and tool binding.
fn configure_mcp_chat(core: &mut DesktopRuntime) {
    core.documents
        .set_default_workflow("workflow.simple-chat")
        .unwrap();
    let mut settings = core.settings_v2_snapshot().settings;
    for provider in &mut settings.providers {
        for model in &mut provider.models {
            model.capabilities = vec!["text".into(), "tools".into()];
        }
    }
    settings.mcp_servers.push(McpServerConfigurationV2 {
        id: "mcp.question-fixture".into(),
        name: "Question fixture".into(),
        enabled: true,
        auto_connect: false,
        plugin: None,
        transport: IntegrationTransportV2::Stdio {
            command: "fixture-mcp".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        },
        tools: vec![crate::runtime::tool_registry::McpToolConfiguration {
            annotations: None,
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: fixture_mcp_saved_schema(),
            enabled: true,
            options: Default::default(),
        }],
    });
    core.settings_v2_commit(SettingsV2CommitInput {
        command_id: "settings.question-fixture".into(),
        expected_version: core.settings_v2_snapshot().version,
        settings,
    })
    .unwrap();
    let mut workflow = core.workflow_snapshot_for("workflow.simple-chat".into());
    workflow.document["nodes"][1]["configuration"]["toolIds"] =
        json!(["mcp://mcp.question-fixture/echo"]);
    core.workflow_commit(WorkflowCommitInput {
        command_id: "workflow.question-fixture".into(),
        expected_version: workflow.version,
        document: workflow.document,
        workflow_id: Some("workflow.simple-chat".into()),
    })
    .unwrap();
}

/// Regression: an answer delivered in a new application generation resumes the
/// pass in a process that never installed the Chat's frozen MCP transports. That
/// resume used to accept work without installing them, so every MCP call in the
/// resumed Run failed with "no MCP transport peer is installed for this
/// application generation" until a fresh turn rebuilt the request. Installing is
/// local work: the answer must not contact a server.
#[test]
fn answering_a_question_installs_the_chats_frozen_mcp_transports() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline::new(provider));
    core.pipeline = pipeline.clone();
    configure_mcp_chat(&mut core);
    core.command(send("question.mcp-start", 0, "Ask me"))
        .unwrap();
    assert!(
        !pipeline.mcp_preparations.lock().unwrap().is_empty(),
        "starting the Chat installs its frozen MCP transports"
    );
    assert_eq!(core.snapshot(0).unwrap().chat.phase, "awaiting_answer");
    drop(core);

    // A new application generation: this process has no sessions for the Chat.
    let mut reopened = runtime(&root, pipeline.provider.clone());
    reopened.pipeline = pipeline.clone();
    pipeline.mcp_preparations.lock().unwrap().clear();
    pipeline.preflight_requests.lock().unwrap().clear();
    let receipt = reopened
        .command(action(
            &reopened,
            "question.mcp-answer",
            "question",
            json!({"questionId": "question.fixture", "optionId": "beta"}),
        ))
        .expect("the answer must be accepted and resume the pass");
    assert!(receipt.accepted);
    assert!(
        !pipeline.mcp_preparations.lock().unwrap().is_empty(),
        "answering a question must install the Chat's frozen MCP transports before the pass resumes"
    );
    assert_eq!(
        pipeline.mcp_opens.load(Ordering::SeqCst),
        0,
        "installing the transports never contacts a server"
    );
    assert_eq!(reopened.snapshot(0).unwrap().chat.phase, "waiting_input");
}

/// Configures an MCP-bound Chat and starts it, returning the runtime, the
/// fixture pipeline and the `mcp://` capability the Chat holds.
fn started_mcp_chat(
    root: &TempDir,
) -> (
    DesktopRuntime,
    Arc<QuestionPipeline>,
    FrozenChatExecutionRecordV1,
    String,
) {
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline::new(provider));
    core.pipeline = pipeline.clone();
    configure_mcp_chat(&mut core);
    core.command(send("mcp.metadata-start", 0, "Ask me"))
        .unwrap();
    let frozen = core.history.current_frozen_context().unwrap().unwrap();
    (
        core,
        pipeline,
        frozen,
        "mcp://mcp.question-fixture/echo".into(),
    )
}

/// The frozen Chat's schema is metadata, not authority: once a call has reached
/// the server, the interface it advertises now is adopted instead of failing the
/// Chat with the removed lockout ("MCP schema changed ... start a New Chat").
#[test]
fn a_changed_mcp_tool_interface_is_adopted_for_the_chat() {
    let root = TempDir::new().unwrap();
    let (mut core, pipeline, frozen, capability) = started_mcp_chat(&root);
    let frozen_definition = frozen
        .context
        .tools
        .iter()
        .find(|tool| tool.tool_id == capability)
        .and_then(|tool| tool.definition.clone())
        .expect("the Chat froze the MCP tool definition");
    assert_eq!(frozen_definition.input_schema, fixture_mcp_saved_schema());

    // The server now advertises the same tool with an added parameter, and a
    // call opens that session.
    let live = json!({
        "type": "object",
        "properties": {"text": {"type": "string"}, "loud": {"type": "boolean"}},
        "required": ["text"]
    });
    *pipeline.mcp_input_schema.lock().unwrap() = live.clone();
    pipeline.open_mcp_session();

    let restored = core
        .restore_frozen_mcp(&frozen.context)
        .expect("changed MCP metadata must not lock the Chat out");
    assert_eq!(restored.definitions[&capability].input_schema, live);
    assert_eq!(
        restored.definitions[&capability].name, frozen_definition.name,
        "the Chat keeps the tool identity it was frozen with"
    );
    assert_eq!(restored.manifests.len(), frozen.context.mcp_manifests.len());
}

/// A tool the server no longer advertises is a changed interface, not a revoked
/// capability: it warns, keeps other capabilities available, and stays offered
/// with the definition the Chat froze — the call reports the error.
#[test]
fn an_mcp_tool_the_server_no_longer_provides_warns_and_stays_offered() {
    let root = TempDir::new().unwrap();
    let (mut core, pipeline, frozen, _) = started_mcp_chat(&root);
    *pipeline.mcp_tool_name.lock().unwrap() = "echo.renamed".into();
    // A call opened a session that provides a differently named tool.
    pipeline.open_mcp_session();

    let restored = core
        .restore_frozen_mcp(&frozen.context)
        .expect("a vanished MCP tool must not block execution");
    assert!(
        restored
            .warnings
            .iter()
            .any(|w| w.contains("no longer provides"))
    );
    assert!(
        restored.definitions.is_empty(),
        "no live interface was discovered for the vanished tool"
    );
}

/// A Chat started with an MCP-bound workflow is immediate and still offers the
/// server's tools. The Chat never contacts a server while freezing or while
/// assembling a pass — opening a session spawns a process or a socket and waits —
/// so a development server that is closed, asleep or merely slow costs the start
/// nothing, and the tool is described from the catalog saved in Settings.
#[test]
fn starting_a_chat_never_contacts_the_mcp_server_and_still_offers_its_tools() {
    let root = TempDir::new().unwrap();
    let (core, pipeline, frozen, capability) = started_mcp_chat(&root);
    assert_eq!(
        pipeline.mcp_opens.load(Ordering::SeqCst),
        0,
        "no MCP session is opened to start a Chat or to run its first pass"
    );
    assert!(pipeline.mcp_open_sessions.lock().unwrap().is_empty());

    // The connection and the tool were frozen from Settings anyway.
    assert_eq!(frozen.context.mcp_manifests.len(), 1);
    assert_eq!(frozen.context.mcp_configurations.len(), 1);
    let binding = frozen
        .context
        .tools
        .iter()
        .find(|tool| tool.tool_id == capability)
        .expect("the server's tool is frozen with the Chat");
    assert!(
        binding.definition.is_some(),
        "an offered tool always carries the interface the model is shown"
    );

    // The pass the model actually received lists the tool, together with the
    // frozen connection that will open it on the first call.
    let offered = pipeline.preflight_requests.lock().unwrap();
    let last = offered.last().expect("the start pass was preflighted");
    assert!(
        last.tools
            .iter()
            .any(|tool| tool.capability_id == capability && tool.definition.is_some()),
        "the server's tool must be offered to the model"
    );
    assert_eq!(
        last.mcp_servers.len(),
        1,
        "the frozen connection travels with every pass"
    );
    let warnings = last.workflow_snapshot["capabilityWarnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.as_str().unwrap_or_default().contains("MCP server")),
        "nothing was contacted, so there is nothing to warn about: {warnings:?}"
    );
    drop(offered);
    drop(core);
}

/// A changed MCP interface never rejects a Chat. The pass after the edit is
/// accepted and offered the interface the Chat holds, and once a call has
/// reached the server the live interface is what later passes offer, without a
/// New Chat.
#[test]
fn a_changed_mcp_interface_reaches_the_next_pass_without_a_new_chat() {
    let root = TempDir::new().unwrap();
    let (mut core, pipeline, _, capability) = started_mcp_chat(&root);
    core.command(action(
        &core,
        "mcp.interface-answer",
        "question",
        json!({"questionId":"question.fixture","optionId":"beta"}),
    ))
    .unwrap();
    assert_eq!(core.snapshot(0).unwrap().chat.phase, "waiting_input");
    drop(core);

    // The server is edited between application generations.
    let live = json!({
        "type": "object",
        "properties": {"text": {"type": "string"}, "loud": {"type": "boolean"}}
    });
    *pipeline.mcp_input_schema.lock().unwrap() = live.clone();
    pipeline.preflight_requests.lock().unwrap().clear();
    let mut reopened = runtime(&root, pipeline.provider.clone());
    reopened.pipeline = pipeline.clone();
    let receipt = reopened
        .command(send(
            "mcp.interface-continue",
            reopened.history.head().unwrap(),
            "Continue",
        ))
        .expect("a changed MCP interface must not reject the next pass");
    assert!(receipt.accepted);
    assert_eq!(
        pipeline.mcp_opens.load(Ordering::SeqCst),
        0,
        "the pass is assembled without contacting the server"
    );
    let requests = pipeline.preflight_requests.lock().unwrap();
    let advertised = requests
        .last()
        .expect("the pass was preflighted")
        .tools
        .iter()
        .find(|tool| tool.capability_id == capability)
        .expect("the Chat still binds its MCP tool");
    assert_eq!(
        advertised.definition.as_ref().unwrap().input_schema,
        fixture_mcp_saved_schema(),
        "an unreached server is described by the catalog the Chat holds"
    );
    drop(requests);

    // The Chat reaches the server, and the next pass is offered what it now
    // advertises.
    pipeline.open_mcp_session();
    pipeline.preflight_requests.lock().unwrap().clear();
    reopened
        .command(send(
            "mcp.interface-adopted",
            reopened.history.head().unwrap(),
            "Continue",
        ))
        .expect("the adopted interface must not reject the pass");
    let requests = pipeline.preflight_requests.lock().unwrap();
    let advertised = requests
        .last()
        .expect("the pass was preflighted")
        .tools
        .iter()
        .find(|tool| tool.capability_id == capability)
        .expect("the Chat still binds its MCP tool");
    assert_eq!(
        advertised.definition.as_ref().unwrap().input_schema,
        live,
        "once the server has been reached, the pass offers its current interface"
    );
}
