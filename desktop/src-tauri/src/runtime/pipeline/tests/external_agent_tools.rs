//! External-agent delegation runs a real product process behind the frozen tool.
//!
//! The product executable is the scripted Codex App Server fixture, so this
//! suite covers the whole path a Chat takes: the frozen tool binding, the
//! dispatch, the one-shot backend process, the durable child frame and the
//! answer that returns to the parent model.
use super::*;
use crate::runtime::{
    approvals::ApprovalMode, tool_loop::SUBAGENT_CODEX_CAPABILITY_ID, tool_registry,
};

/// Serves the parent model: one delegation call, then a final answer.
struct DelegationProvider {
    capability_id: String,
    provider_name: String,
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
}

impl ProviderFactoryV1 for DelegationProvider {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(ScriptedDelegation {
            capability_id: self.capability_id.clone(),
            provider_name: self.provider_name.clone(),
            requests: Arc::clone(&self.requests),
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
        }))
    }
}

struct ScriptedDelegation {
    capability_id: String,
    provider_name: String,
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    binding: String,
    version: String,
}

impl ProviderEnginePortV1 for ScriptedDelegation {
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
        panic!("an external delegation must never invoke an approval reviewer")
    }

    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.exchanges.is_empty() {
            emit(tool_call(
                "delegate",
                &self.capability_id,
                &self.provider_name,
                json!({"task":"Summarize the delegation fixture."}),
            ))?;
        } else {
            emit(ModelToolEventV1::AssistantOutput {
                text: "The external agent answered.".into(),
            })?;
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 5,
            output_tokens: 2,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

/// The scripted product process, skipped where a POSIX fixture cannot run.
fn fixture_target() -> Option<crate::runtime::tool_loop::ResolvedExternalAgentTargetV1> {
    if cfg!(windows) {
        return None;
    }
    let executable = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/aworkit-capability-host/tests/fixtures/codex_one_shot_fixture.py");
    executable.is_file().then(
        || crate::runtime::tool_loop::ResolvedExternalAgentTargetV1 {
            backend: "codex".to_owned(),
            executable: executable.display().to_string(),
            arguments: vec!["app-server".to_owned(), "--stdio".to_owned()],
            working_directory: None,
            permission_mode: "never".to_owned(),
            model: None,
            reasoning_effort: None,
        },
    )
}

/// Freezes one external delegation tool exactly like a first-input freeze does.
fn external_binding(
    capability_id: &str,
    target: &crate::runtime::tool_loop::ResolvedExternalAgentTargetV1,
) -> WorkflowToolBindingV1 {
    let setting = tool_registry::native_defaults()
        .into_iter()
        .find(|tool| tool.id == capability_id)
        .expect("installed external delegation tool");
    let mut frozen = tool_registry::freeze_settings(&setting).expect("freezes");
    frozen.configuration.insert(
        "resolvedTarget".into(),
        serde_json::to_value(target).expect("target"),
    );
    WorkflowToolBindingV1 {
        options: frozen.options,
        capability_id: capability_id.into(),
        configuration: serde_json::to_value(frozen.configuration).expect("configuration"),
        credential_bindings: Vec::new(),
        definition: None,
    }
}

/// A branch workflow: the condition always routes true, so the external-agent
/// node runs and the Agent node on the false branch stays inactive. The Agent
/// node keeps the document executable under the native rule that a workflow
/// must contain an Agent or Model Call node.
fn external_agent_workflow(tool_id: &str, extras: serde_json::Value) -> serde_json::Value {
    let mut configuration = json!({"toolId": tool_id});
    if let Some(extras) = extras.as_object() {
        for (key, value) in extras {
            configuration[key] = value.clone();
        }
    }
    json!({
        "schemaVersion": 1,
        "nodes": [
            {"id": "input.1", "type": "input", "label": "In"},
            {
                "id": "cond.1",
                "type": "condition",
                "label": "Branch",
                "configuration": {"predicate": {"kind": "always"}},
            },
            {
                "id": "ext.1",
                "type": "external_agent",
                "label": "External",
                "configuration": configuration,
            },
            {
                "id": "agent.1",
                "type": "agent",
                "label": "Fallback",
                "configuration": {"modelTierId": "tier:balanced", "toolIds": []},
            },
            {"id": "output.1", "type": "output", "label": "Out"},
            {"id": "wait.1", "type": "wait", "label": "Wait"},
        ],
        "edges": [
            {"id": "e.1", "source": "input.1", "target": "cond.1"},
            {
                "id": "e.2",
                "source": "cond.1",
                "target": "ext.1",
                "sourcePort": "true",
                "configuration": {"route": "true"},
            },
            {"id": "e.3", "source": "ext.1", "target": "output.1"},
            {"id": "e.4", "source": "output.1", "target": "wait.1"},
            {
                "id": "e.5",
                "source": "cond.1",
                "target": "agent.1",
                "sourcePort": "false",
                "configuration": {"route": "false"},
            },
            {"id": "e.6", "source": "agent.1", "target": "wait.1"},
        ],
    })
}

#[test]
fn an_external_agent_node_runs_one_delegation_and_feeds_the_answer_downstream() {
    let Some(target) = fixture_target() else {
        return;
    };
    let root = TempDir::new().unwrap();
    let project = subagent_project(&root);
    let (mut pipeline, _, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    let requests = Arc::new(Mutex::new(Vec::new()));
    pipeline.provider_factory = Arc::new(DelegationProvider {
        capability_id: SUBAGENT_CODEX_CAPABILITY_ID.into(),
        provider_name: "spawn_codex_subagent".into(),
        requests: Arc::clone(&requests),
    });
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = vec![external_binding(SUBAGENT_CODEX_CAPABILITY_ID, &target)];
    request.workflow_snapshot = external_agent_workflow("tool.subagent_codex", json!({}));

    let result = pipeline.execute(request).expect("the node executes");
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    // The branch ran the product once and never reached the model fallback.
    assert_eq!(result.tool_calls, 1);
    assert_eq!(result.model_turns, 0);
    let text = result.assistant_text.unwrap_or_default();
    assert!(text.contains("fixture final answer"), "{text}");
    assert!(
        requests.lock().unwrap().is_empty(),
        "an external-agent node must not require a parent model turn"
    );
}

#[test]
fn an_external_agent_node_refuses_a_node_route_its_product_cannot_run() {
    let Some(target) = fixture_target() else {
        return;
    };
    let root = TempDir::new().unwrap();
    let project = subagent_project(&root);
    let (mut pipeline, _, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    let requests = Arc::new(Mutex::new(Vec::new()));
    pipeline.provider_factory = Arc::new(DelegationProvider {
        capability_id: SUBAGENT_CODEX_CAPABILITY_ID.into(),
        provider_name: "spawn_codex_subagent".into(),
        requests: Arc::clone(&requests),
    });
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = vec![external_binding(SUBAGENT_CODEX_CAPABILITY_ID, &target)];
    request.workflow_snapshot = external_agent_workflow(
        "tool.subagent_codex",
        json!({"reasoningEffort": "invented"}),
    );

    let error = pipeline
        .execute(request)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains("reasoningEffort") || error.contains("reasoning effort"),
        "unexpected error: {error}"
    );
}

#[test]
fn an_external_delegation_returns_only_the_products_final_answer() {
    let Some(target) = fixture_target() else {
        return;
    };
    let root = TempDir::new().unwrap();
    let project = subagent_project(&root);
    let (mut pipeline, _, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    let requests = Arc::new(Mutex::new(Vec::new()));
    pipeline.provider_factory = Arc::new(DelegationProvider {
        capability_id: SUBAGENT_CODEX_CAPABILITY_ID.into(),
        provider_name: "spawn_codex_subagent".into(),
        requests: Arc::clone(&requests),
    });
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = vec![external_binding(SUBAGENT_CODEX_CAPABILITY_ID, &target)];
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] =
        json!(vec![SUBAGENT_CODEX_CAPABILITY_ID]);

    let result = pipeline.execute(request).expect("the delegation executes");
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );

    // The parent's next turn received the product's answer and nothing else.
    let observed = requests.lock().unwrap();
    assert_eq!(observed.len(), 2, "one delegation turn and one final turn");
    let delegated = &observed[1].exchanges[0].results[0];
    assert!(!delegated.is_error, "{delegated:?}");
    assert_eq!(delegated.content["answer"], "fixture final answer");
    assert!(
        delegated.content["childId"].as_str().is_some(),
        "the child identity returns with the answer: {delegated:?}"
    );
    // No product commentary, reasoning or process detail crosses the boundary.
    assert!(
        !serde_json::to_string(&delegated.content)
            .unwrap()
            .contains("fixture-model"),
        "{delegated:?}"
    );
}

#[test]
fn an_unresolved_target_fails_the_delegation_before_any_process_starts() {
    let Some(target) = fixture_target() else {
        return;
    };
    let root = TempDir::new().unwrap();
    let project = subagent_project(&root);
    let (mut pipeline, _, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    let requests = Arc::new(Mutex::new(Vec::new()));
    pipeline.provider_factory = Arc::new(DelegationProvider {
        capability_id: SUBAGENT_CODEX_CAPABILITY_ID.into(),
        provider_name: "spawn_codex_subagent".into(),
        requests: Arc::clone(&requests),
    });
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    let mut binding = external_binding(SUBAGENT_CODEX_CAPABILITY_ID, &target);
    // A tool whose target never resolved is refused, not guessed at.
    binding.configuration["resolvedTarget"] = serde_json::Value::Null;
    request.tools = vec![binding];
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] =
        json!(vec![SUBAGENT_CODEX_CAPABILITY_ID]);

    let error = pipeline
        .execute(request)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains("external delegation target was not resolved at freeze")
            || error.contains("malformed"),
        "unexpected error: {error}"
    );
}
