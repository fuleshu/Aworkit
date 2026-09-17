//! Upgrade regression: a stopped Run keeps its original native tool definitions.
use super::*;

struct ContinuationProviderFactory(Arc<AtomicUsize>);
impl ProviderFactoryV1 for ContinuationProviderFactory {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(ContinuationProvider(ScriptedProvider {
            calls: self.0.clone(),
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
            behavior: ScriptedBehavior::Succeed,
            observed_inputs: None,
        })))
    }
}
struct ContinuationProvider(ScriptedProvider);
impl ProviderEnginePortV1 for ContinuationProvider {
    fn binding_id(&self) -> &str {
        self.0.binding_id()
    }
    fn version_hash(&self) -> &str {
        self.0.version_hash()
    }
    fn execute(
        &self,
        request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.0.execute(request, emit)
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        assert!(
            request
                .tools
                .iter()
                .all(|tool| tool.description.starts_with("Original frozen description"))
        );
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        emit(ModelToolEventV1::AssistantOutput {
            text: "continued with original tool definitions".into(),
        })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 7,
            output_tokens: 3,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

fn native_request(metadata: CredentialMetadataV1) -> WorkflowExecutionRequestV1 {
    let mut request = request(metadata);
    let ids = ["tool.shell.host", "tool.python.host"];
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(ids);
    request.tools = ids
        .into_iter()
        .map(|id| {
            let native = crate::runtime::tool_registry::native_tool(id).unwrap();
            WorkflowToolBindingV1 {
                capability_id: id.into(),
                configuration: serde_json::to_value(&native.configuration).unwrap(),
                options: Default::default(),
                credential_bindings: Vec::new(),
                definition: None,
            }
        })
        .collect();
    request
}

#[test]
fn continuation_and_replay_keep_native_descriptions_across_catalog_upgrade() {
    let root = TempDir::new().unwrap();
    let (pipeline, credentials, metadata, calls, _) = setup(&root, ScriptedBehavior::Succeed);
    drop(pipeline);
    let pipeline = WorkflowExecutionPipeline::compose(
        root.path(),
        credentials.clone(),
        Arc::new(ContinuationProviderFactory(calls.clone())),
    )
    .unwrap();
    let first = native_request(metadata);
    let protocol = ProviderProtocolV1::parse(&first.provider.kind).unwrap();
    let descriptor = &pipeline.descriptors[&protocol];
    let current = pipeline
        .prepare(&first, protocol, descriptor, None)
        .unwrap();
    let mut old_catalog = current.clone();
    for tool in &mut old_catalog.tool_bindings {
        tool.description = format!("Original frozen description for {}", tool.capability_id);
    }
    // Materialize the valid graph/manifest that the old catalog would have produced.
    let original = pipeline
        .prepare(&first, protocol, descriptor, Some(&old_catalog))
        .unwrap();
    assert!(
        !current.same_frozen_run(&original),
        "re-freezing catalog text reproduced the mismatch"
    );
    pipeline.records.record_execution(&original).unwrap();
    let result = pipeline.execute(first.clone()).unwrap();
    assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(pipeline);

    let pipeline = WorkflowExecutionPipeline::compose(
        root.path(),
        credentials,
        Arc::new(ContinuationProviderFactory(calls.clone())),
    )
    .unwrap();
    assert!(pipeline.execute(first.clone()).unwrap().replayed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "restart must not replay settled effects"
    );
    let mut followup = first.clone();
    followup.request_id = stable("command.after-stop").unwrap();
    followup.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Continue after Stop".into(),
        images: Vec::new(),
    });
    pipeline.preflight(&followup).unwrap();
    let second = pipeline.execute(followup.clone()).unwrap();
    assert_eq!(second.snapshot_hash, result.snapshot_hash);
    assert_eq!(second.authority_manifest_id, result.authority_manifest_id);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let saved = pipeline
        .records
        .execution(&followup.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.tool_bindings, original.tool_bindings);

    for mutation in 0..4 {
        let mut drift = followup.clone();
        drift.request_id = stable(&format!("command.real-drift-{mutation}")).unwrap();
        match mutation {
            0 => drift.tools[0].configuration["timeoutSeconds"] = json!(2),
            1 => {
                drift.tools[0].options.approval_mode =
                    Some(crate::runtime::approvals::ApprovalMode::FullAccess)
            }
            2 => drift.provider.model = "another-model".into(),
            _ => drift.frozen_context_hash = format!("sha256:{}", "c".repeat(64)),
        }
        assert!(
            pipeline.preflight(&drift).is_err(),
            "real authority drift {mutation} must remain rejected"
        );
        // Reusing a command ID is equally strict.
        drift.request_id = first.request_id.clone();
        assert!(pipeline.preflight(&drift).is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn dynamic_mcp_descriptions_still_require_the_exact_frozen_definition() {
    let mut requested = vec![WorkflowToolBindingV1 {
        capability_id: MCP_FIXTURE_CAPABILITY.into(),
        configuration: json!({"serverId": MCP_FIXTURE_SERVER,"tool": MCP_FIXTURE_TOOL}),
        options: Default::default(),
        credential_bindings: Vec::new(),
        definition: Some(mcp_echo_definition()),
    }];
    let saved = freeze_file_tool_bindings(&requested).unwrap();
    requested[0].definition.as_mut().unwrap().description = "Changed remote tool".into();
    let changed = super::super::frozen_tools::freeze(&requested, Some(&saved)).unwrap();
    assert_ne!(changed, saved, "MCP definition drift must not be hidden");
}

/// Diagnostic input is a read-only export of one prepared record, never a live
/// database. Preflight is exercised in a temporary store without provider effects.
#[test]
#[ignore = "set AWORKIT_CAPTURED_EXECUTION to a locally exported prepared record"]
fn captured_chat_continuation_preflight_preserves_original_authority() {
    let path = std::env::var("AWORKIT_CAPTURED_EXECUTION").unwrap();
    let original: PreparedExecutionRecordV1 =
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let root = TempDir::new().unwrap();
    let (pipeline, _, _, calls, _) = setup(&root, ScriptedBehavior::Succeed);
    pipeline.records.record_execution(&original).unwrap();
    let mut followup = WorkflowExecutionRequestV1::bounded(
        stable("command.captured-continuation-check").unwrap(),
        original.snapshot.chat_id.clone(),
        original.snapshot.run_id.clone(),
        WorkflowProviderBindingV1 {
            kind: original.provider.kind.clone(),
            base_url: original.provider.base_url.clone(),
            model: original.provider.model.clone(),
            request_timeout_seconds: original.provider.request_timeout_seconds,
            maximum_tool_output_bytes: original.provider.maximum_tool_output_bytes,
            credential: original
                .secret
                .as_ref()
                .map(StoredSecretBindingV1::metadata),
        },
        vec![WorkflowMessageV1 {
            role: "user".into(),
            content: "Continue".into(),
            images: Vec::new(),
        }],
        current_epoch_millis(),
    );
    followup.approvals = original.approvals.clone();
    followup.model_parameters = original.provider.parameters.clone();
    followup.model_context = original.provider.model_context.clone();
    followup.workspace = original.workspace.clone();
    followup.project_branch = original.project_branch.clone();
    followup.budget = original.snapshot.budget.clone();
    followup.maximum_timeout_recoveries = original.maximum_timeout_recoveries;
    followup.mcp_servers = original.mcp_manifests.values().cloned().collect();
    followup.workflow_snapshot = original.worker_proposal.payload["config"]["workflow"].clone();
    followup.frozen_context_hash = original.snapshot.nodes[0].config["frozenContextHash"]
        .as_str()
        .unwrap()
        .into();
    followup.tools = original
        .tool_bindings
        .iter()
        .map(|tool| WorkflowToolBindingV1 {
            capability_id: tool.capability_id.clone(),
            configuration: tool.configuration.clone(),
            options: tool.options.clone(),
            definition: tool
                .capability_id
                .starts_with(MCP_CAPABILITY_PREFIX)
                .then(|| tool.definition()),
            credential_bindings: tool
                .secret
                .iter()
                .map(
                    |secret| crate::runtime::tool_loop::WorkflowToolCredentialBindingV1 {
                        name: secret.name.clone(),
                        credential_ref: secret.credential_ref.clone(),
                        field: secret.field.clone(),
                        field_names: secret.field_names.clone(),
                        revision: secret.revision,
                    },
                )
                .collect(),
        })
        .collect();
    let protocol = ProviderProtocolV1::parse(&followup.provider.kind).unwrap();
    let freshly_refrozen = pipeline
        .prepare(&followup, protocol, &pipeline.descriptors[&protocol], None)
        .unwrap();
    assert!(
        !freshly_refrozen.same_frozen_run(&original),
        "capture reproduces catalog drift"
    );
    let (resumed, _) = pipeline
        .validated_prepared(&followup)
        .expect("original Chat must continue");
    assert!(resumed.same_frozen_run(&original));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "preflight must not invoke any provider"
    );
}
