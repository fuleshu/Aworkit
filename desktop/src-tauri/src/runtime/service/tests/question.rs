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
    /// One entry per frozen-endpoint preparation, so a test can prove the Chat's
    /// MCP transport was reconnected before a resumed pass accepted work.
    mcp_preparations: Mutex<Vec<String>>,
    /// The tool name and schema the fixture server advertises, so a test can
    /// edit the server between two application generations.
    mcp_tool_name: Mutex<String>,
    mcp_input_schema: Mutex<Value>,
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
            preflight_requests: Mutex::new(Vec::new()),
        }
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

    /// Records every frozen-endpoint preparation for this Run and answers with
    /// the exact discovery snapshot the fixture server would hand back.
    fn prepare_mcp_sessions(
        &self,
        run_id: &StableId,
        servers: &mut [McpRunServerPreparationV1],
    ) -> Result<(Vec<McpCapabilitySnapshotV1>, Vec<String>), String> {
        self.mcp_preparations
            .lock()
            .unwrap()
            .push(run_id.as_str().to_owned());
        let tool_name = self.mcp_tool_name.lock().unwrap().clone();
        let input_schema = self.mcp_input_schema.lock().unwrap().clone();
        Ok((servers
            .iter()
            .map(|server| frozen_mcp_snapshot(&server.manifest, &tool_name, &input_schema))
            .collect(), Vec::new()))
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
            input_schema: json!({"type":"object"}),
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
/// pass in a process that never opened the Chat's frozen MCP endpoints. That
/// resume used to accept work without reconnecting them, so every MCP call in
/// the resumed Run failed with "no MCP transport peer is installed for this
/// application generation" until a fresh turn rebuilt the request.
#[test]
fn answering_a_question_reconnects_the_chats_frozen_mcp_endpoints() {
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
        "starting the Chat opens its frozen MCP endpoints"
    );
    assert_eq!(core.snapshot(0).unwrap().chat.phase, "awaiting_answer");
    drop(core);

    // A new application generation: this process has no sessions for the Chat.
    let mut reopened = runtime(&root, pipeline.provider.clone());
    reopened.pipeline = pipeline.clone();
    pipeline.mcp_preparations.lock().unwrap().clear();
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
        "answering a question must reconnect the Chat's frozen MCP endpoints before the pass resumes"
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

/// The frozen Chat's schema is metadata, not authority: a server edit reaches
/// the Chat instead of failing it with the removed lockout
/// ("MCP schema changed ... start a New Chat").
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
    assert_eq!(frozen_definition.input_schema, fixture_mcp_input_schema());

    // The server now advertises the same tool with an added parameter.
    let live = json!({
        "type": "object",
        "properties": {"text": {"type": "string"}, "loud": {"type": "boolean"}},
        "required": ["text"]
    });
    *pipeline.mcp_input_schema.lock().unwrap() = live.clone();

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

/// A tool the server no longer advertises is a missing capability, not metadata
/// drift: it warns and leaves other capabilities available.
#[test]
fn an_mcp_tool_the_server_no_longer_provides_warns_and_continues() {
    let root = TempDir::new().unwrap();
    let (mut core, pipeline, frozen, _) = started_mcp_chat(&root);
    *pipeline.mcp_tool_name.lock().unwrap() = "echo.renamed".into();

    let restored = core
        .restore_frozen_mcp(&frozen.context)
        .expect("a vanished MCP tool must not block execution");
    assert!(restored.warnings.iter().any(|w| w.contains("no longer provided")));
    assert_eq!(restored.unavailable.len(), 1);
    assert!(restored.definitions.is_empty());
}

/// The whole point: after the server changes, the next pass of the same Chat is
/// accepted and offered the live interface, without a New Chat.
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
        "the pass is offered the server's current interface"
    );
}
