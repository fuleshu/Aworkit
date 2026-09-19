//! Delegation crosses the real gateway, broker, filesystem and process hosts.
use super::*;
use crate::runtime::{approvals::ApprovalMode, tool_registry};

#[derive(Clone)]
struct Scenario {
    calls: Vec<ModelToolCallV1>,
    read_only: bool,
    parent_retries: bool,
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
}
impl ProviderFactoryV1 for Scenario {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(DelegationProvider {
            scenario: self.clone(),
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
        }))
    }
}
struct DelegationProvider {
    scenario: Scenario,
    binding: String,
    version: String,
}
impl ProviderEnginePortV1 for DelegationProvider {
    fn binding_id(&self) -> &str {
        &self.binding
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        _: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        panic!("Delegation must never invoke an approval reviewer")
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.scenario.requests.lock().unwrap().push(request.clone());
        let parent = request
            .tools
            .iter()
            .any(|t| t.capability_id == SUBAGENT_CAPABILITY_ID);
        if parent && request.exchanges.is_empty() {
            emit(tool_call(
                "delegate",
                SUBAGENT_CAPABILITY_ID,
                "spawn_subagent",
                json!({"task":"Implement the assigned files and report evidence.","readOnly":self.scenario.read_only}),
            ))?;
        } else if !parent && request.exchanges.is_empty() {
            for call in &self.scenario.calls {
                emit(ModelToolEventV1::ToolCall { call: call.clone() })?;
            }
        } else if parent && request.exchanges.len() == 1 && self.scenario.parent_retries {
            let result = &request.exchanges[0].results[0].content;
            assert_eq!(result["status"], "parent_approval_required");
            let mut call: ModelToolCallV1 =
                serde_json::from_value(result["blockedActions"][0].clone()).unwrap();
            call.call_id = "parent-retry".into();
            call.provider_call_id = Some(call.call_id.clone());
            emit(ModelToolEventV1::ToolCall { call })?;
        } else {
            let mut jobs = BTreeMap::new();
            if !parent {
                for result in request.exchanges.iter().flat_map(|e| &e.results) {
                    if let Some(id) = result.content["jobId"].as_str() {
                        jobs.insert(id, &result.content);
                    }
                }
            }
            let pending: Vec<_> = jobs
                .into_iter()
                .filter(|(_, v)| v["running"] == true || v["moreOutput"] == true)
                .collect();
            if pending.is_empty() {
                emit(ModelToolEventV1::AssistantOutput {
                    text: "Finished the assigned work.".into(),
                })?;
            } else {
                for (index, (id, _)) in pending.iter().enumerate() {
                    emit(ModelToolEventV1::ToolCall {
                        call: call(
                            &format!("collect-{}-{index}", request.exchanges.len()),
                            "tool.job.output",
                            json!({"jobId":id,"waitMs":1000}),
                        ),
                    })?;
                }
            }
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 5,
            output_tokens: 2,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}
fn call(id: &str, tool: &str, args: Value) -> ModelToolCallV1 {
    let name = tool_registry::native_tool(tool)
        .unwrap()
        .provider_name
        .as_str();
    match tool_call(id, tool, name, args) {
        ModelToolEventV1::ToolCall { call } => call,
        _ => unreachable!(),
    }
}
fn native(id: &str) -> WorkflowToolBindingV1 {
    let mut setting = tool_registry::native_defaults()
        .into_iter()
        .find(|t| t.id == id)
        .unwrap();
    #[cfg(windows)]
    if id == "tool.python.host" {
        setting.options.executable = Some("C:\\Python313\\python.exe".into());
    }
    let mut frozen = tool_registry::freeze_settings(&setting).unwrap();
    if id == SUBAGENT_CAPABILITY_ID {
        frozen.options.approval_mode = Some(ApprovalMode::FullAccess);
    }
    WorkflowToolBindingV1 {
        capability_id: id.into(),
        configuration: serde_json::to_value(frozen.configuration).unwrap(),
        options: frozen.options,
        credential_bindings: Vec::new(),
        definition: None,
    }
}
fn prepare(
    root: &TempDir,
    selected: &[&str],
    calls: Vec<ModelToolCallV1>,
    read_only: bool,
    parent_retries: bool,
) -> (
    WorkflowExecutionPipeline,
    WorkflowExecutionRequestV1,
    Scenario,
) {
    let project = subagent_project(root);
    let (mut pipeline, _, metadata, _, _) = setup(root, ScriptedBehavior::Succeed);
    let scenario = Scenario {
        calls,
        read_only,
        parent_retries,
        requests: Arc::default(),
    };
    pipeline.provider_factory = Arc::new(scenario.clone());
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = selected.iter().map(|id| native(id)).collect();
    // Another workflow binding must not leak into this Agent's child.
    request.tools.push(native(TODO_CAPABILITY_ID));
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(selected);
    (pipeline, request, scenario)
}

#[test]
fn subagent_inherits_exact_parent_selection_and_executes_write_edit_shell_python_and_mcp() {
    let root = TempDir::new().unwrap();
    let ids = [
        SUBAGENT_CAPABILITY_ID,
        "tool.files.write",
        FILE_EDIT_CAPABILITY_ID,
        "tool.shell.host",
        "tool.python.host",
        "tool.job.output",
        "tool.job.input",
        "tool.job.stop",
        "tool.job.list",
    ];
    let calls = vec![
        call(
            "write",
            "tool.files.write",
            json!({"path":"created.txt","content":"alpha"}),
        ),
        call(
            "edit",
            FILE_EDIT_CAPABILITY_ID,
            json!({"path":"created.txt","old_string":"alpha","new_string":"beta"}),
        ),
        call(
            "shell",
            "tool.shell.host",
            json!({"command": "echo shell-ok > shell.txt"}),
        ),
        call(
            "python",
            "tool.python.host",
            json!({"script":"from pathlib import Path\nPath('python.txt').write_text('python-ok')\nprint('python-ok')"}),
        ),
        ModelToolCallV1 {
            call_id: "mcp".into(),
            provider_call_id: Some("mcp".into()),
            capability_id: MCP_FIXTURE_CAPABILITY.into(),
            name: MCP_FIXTURE_NAME.into(),
            arguments: json!({"text":"child-mcp-ok"}),
            provider_context: None,
        },
    ];
    let (pipeline, mut request, scenario) = prepare(&root, &ids, calls, false, false);
    let peer_calls = Arc::new(AtomicUsize::new(0));
    pipeline
        .install_mcp_peer(Arc::new(ScriptedMcpPeer {
            behavior: ScriptedMcpBehavior::Echo,
            calls: peer_calls.clone(),
            observed_arguments: Arc::default(),
        }))
        .unwrap();
    request.tools.push(WorkflowToolBindingV1 {
        capability_id: MCP_FIXTURE_CAPABILITY.into(),
        configuration: json!({"serverId":MCP_FIXTURE_SERVER,"tool":MCP_FIXTURE_TOOL}),
        options: Default::default(),
        credential_bindings: vec![],
        definition: Some(mcp_echo_definition()),
    });
    request.mcp_servers = vec![mcp_fixture_manifest(pipeline.generation)];
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"]
        .as_array_mut()
        .unwrap()
        .push(json!(MCP_FIXTURE_CAPABILITY));
    let result = pipeline.execute(request.clone()).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project/created.txt")).unwrap(),
        "beta"
    );
    assert!(
        fs::read_to_string(root.path().join("project/shell.txt"))
            .unwrap()
            .contains("shell-ok")
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project/python.txt")).unwrap(),
        "python-ok"
    );
    assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
    let observed = scenario.requests.lock().unwrap();
    let turn_count = observed.len();
    assert!(
        (4..=8).contains(&turn_count),
        "bounded process collection plus parent/child turns: {turn_count}"
    );
    let child = &observed[1];
    let mut expected: Vec<_> = ids[1..].to_vec();
    expected.push(MCP_FIXTURE_CAPABILITY);
    assert_eq!(
        child
            .tools
            .iter()
            .map(|t| t.capability_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    for tool in &child.tools {
        assert_eq!(
            serde_json::to_value(tool).unwrap(),
            serde_json::to_value(
                observed[0]
                    .tools
                    .iter()
                    .find(|t| t.capability_id == tool.capability_id)
                    .unwrap()
            )
            .unwrap()
        );
    }
    for turn in &observed[2..turn_count - 1] {
        assert_eq!(turn.input, child.input, "stable child prompt prefix");
        assert_eq!(
            serde_json::to_value(&turn.tools).unwrap(),
            serde_json::to_value(&child.tools).unwrap()
        );
    }
    assert!(
        observed[2].exchanges[0].results.iter().all(|r| !r.is_error),
        "{:?}",
        observed[2].exchanges[0].results
    );
    drop(observed);
    assert!(pipeline.execute(request).unwrap().replayed);
    assert_eq!(scenario.requests.lock().unwrap().len(), turn_count);
}

#[test]
fn subagent_hands_off_without_review_or_later_batch_effects_and_parent_can_approve() {
    for parent_retries in [false, true] {
        let root = TempDir::new().unwrap();
        let external = root.path().join("external.txt");
        let blocked = call(
            "blocked",
            "tool.files.write",
            json!({"path":external,"content":"parent-approved"}),
        );
        let calls = vec![
            call(
                "before",
                "tool.files.write",
                json!({"path":"before.txt","content":"child-permitted"}),
            ),
            blocked.clone(),
            call(
                "after",
                "tool.files.write",
                json!({"path":"after.txt","content":"must not execute"}),
            ),
        ];
        let (pipeline, mut request, scenario) = prepare(
            &root,
            &[SUBAGENT_CAPABILITY_ID, "tool.files.write"],
            calls,
            false,
            parent_retries,
        );
        request.approvals.mode = if parent_retries {
            ApprovalMode::AskForApproval
        } else {
            ApprovalMode::ApproveForMe
        };
        let result = pipeline.execute(request).unwrap();
        assert!(root.path().join("project/before.txt").exists());
        assert!(!root.path().join("project/after.txt").exists());
        assert!(!external.exists());
        {
            let observed = scenario.requests.lock().unwrap();
            assert_eq!(
                observed.len(),
                3,
                "no child model turn after a blocked action"
            );
            let handoff = &observed[2].exchanges[0].results[0].content;
            assert_eq!(handoff["status"], "parent_approval_required");
            assert_eq!(handoff["blockedActions"], json!([blocked]));
            assert_eq!(handoff["modelTurns"], 1);
            assert_eq!(handoff["toolCalls"], 2);
        }
        if parent_retries {
            assert_eq!(
                result.status,
                WorkflowExecutionStatusV1::AwaitingApproval,
                "{:?}",
                result.error
            );
            let approval = result.approval.unwrap();
            let resumed = pipeline
                .resume_approval(&approval.decision_id, true)
                .unwrap();
            assert_eq!(
                resumed.status,
                WorkflowExecutionStatusV1::Succeeded,
                "{:?}",
                resumed.error
            );
            assert_eq!(fs::read_to_string(external).unwrap(), "parent-approved");
            assert_eq!(
                scenario.requests.lock().unwrap().len(),
                4,
                "resume must not rerun delegation"
            );
        } else {
            assert_eq!(
                result.status,
                WorkflowExecutionStatusV1::Succeeded,
                "{:?}",
                result.error
            );
        }
    }
}

#[test]
fn subagent_read_only_filters_effects_and_uses_mcp_hints_not_auto_approve() {
    for safe in [false, true] {
        let root = TempDir::new().unwrap();
        let (pipeline, mut request, scenario) = prepare(
            &root,
            &[
                SUBAGENT_CAPABILITY_ID,
                FILE_READ_CAPABILITY_ID,
                "tool.files.write",
                "tool.shell.host",
                "tool.job.stop",
            ],
            vec![call(
                "read",
                FILE_READ_CAPABILITY_ID,
                json!({"path":"notes.txt"}),
            )],
            true,
            false,
        );
        request.tools.push(WorkflowToolBindingV1 { capability_id:MCP_FIXTURE_CAPABILITY.into(), configuration:json!({"serverId":MCP_FIXTURE_SERVER,"tool":MCP_FIXTURE_TOOL,"annotations":{"readOnlyHint":safe,"destructiveHint":false}}), options:Default::default(), credential_bindings:vec![], definition:Some(mcp_echo_definition()) });
        request.tools.last_mut().unwrap().options.auto_approve = true;
        request.mcp_servers = vec![mcp_fixture_manifest(pipeline.generation)];
        request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"]
            .as_array_mut()
            .unwrap()
            .push(json!(MCP_FIXTURE_CAPABILITY));
        pipeline
            .install_mcp_peer(Arc::new(ScriptedMcpPeer {
                behavior: ScriptedMcpBehavior::Echo,
                calls: Arc::default(),
                observed_arguments: Arc::default(),
            }))
            .unwrap();
        let result = pipeline.execute(request).unwrap();
        assert_eq!(
            result.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        let observed = scenario.requests.lock().unwrap();
        let expected = if safe {
            vec![FILE_READ_CAPABILITY_ID, MCP_FIXTURE_CAPABILITY]
        } else {
            vec![FILE_READ_CAPABILITY_ID]
        };
        assert_eq!(
            observed[1]
                .tools
                .iter()
                .map(|t| t.capability_id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(!observed[2].exchanges[0].results[0].is_error);
    }
}
