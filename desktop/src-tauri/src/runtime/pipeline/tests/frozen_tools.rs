//! A later pass of an existing Chat adopts the current documents: the graph, the
//! bound tool set and the interfaces this build actually offers. Authority, the
//! workspace binding and committed evidence travel from the saved Chat.
use super::*;

/// Counts provider work and records the tool definitions the model was offered.
#[derive(Default)]
struct ContinuationControl {
    calls: AtomicUsize,
    offered_descriptions: Mutex<Vec<String>>,
}

struct ContinuationProviderFactory(Arc<ContinuationControl>);

struct ContinuationProvider {
    binding: String,
    version: String,
    control: Arc<ContinuationControl>,
}

impl ProviderFactoryV1 for ContinuationProviderFactory {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(ContinuationProvider {
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
            control: self.0.clone(),
        }))
    }
}

impl ProviderEnginePortV1 for ContinuationProvider {
    fn binding_id(&self) -> &str {
        &self.binding
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.control.calls.fetch_add(1, Ordering::SeqCst);
        emit(ModelEventV1::AssistantOutput(
            "adopted the current documents".into(),
        ))?;
        emit(ModelEventV1::Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.control
            .offered_descriptions
            .lock()
            .unwrap()
            .extend(request.tools.iter().map(|tool| tool.description.clone()));
        self.control.calls.fetch_add(1, Ordering::SeqCst);
        emit(ModelToolEventV1::AssistantOutput {
            text: "adopted the current documents".into(),
        })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache: Default::default(),
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

/// The interface the model is offered re-resolves from this build, so a Chat
/// frozen before a tool improved adopts the improvement in its next pass while
/// the authority it was frozen with does not change.
#[test]
fn later_pass_re_resolves_the_interface_and_keeps_saved_authority() {
    let root = TempDir::new().unwrap();
    let control = Arc::new(ContinuationControl::default());
    let (pipeline, credentials, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    drop(pipeline);
    let pipeline = WorkflowExecutionPipeline::compose(
        root.path(),
        credentials,
        Arc::new(ContinuationProviderFactory(control.clone())),
    )
    .unwrap();
    let first = native_request(metadata);
    let protocol = ProviderProtocolV1::parse(&first.provider.kind).unwrap();
    let descriptor = &pipeline.descriptors[&protocol];
    let current = pipeline
        .prepare(&first, protocol, descriptor, None)
        .unwrap();
    // The saved Chat carries the catalog text it was frozen with.
    let mut old_catalog = current.clone();
    for tool in &mut old_catalog.tool_bindings {
        tool.description = format!("Original frozen description for {}", tool.capability_id);
    }
    pipeline.records.record_execution(&old_catalog).unwrap();

    let mut followup = first.clone();
    followup.request_id = stable("command.later-pass").unwrap();
    followup.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Continue after the capability edit".into(),
        images: Vec::new(),
    });
    let (prepared, replayed) = pipeline.validated_prepared(&followup).unwrap();
    assert!(!replayed);
    // The interface comes from this build, not from the saved catalog text.
    for tool in &prepared.tool_bindings {
        let fresh = current
            .tool_bindings
            .iter()
            .find(|fresh| fresh.capability_id == tool.capability_id)
            .unwrap();
        assert_eq!(tool.description, fresh.description);
        assert_eq!(tool.input_schema, fresh.input_schema);
        assert_eq!(tool.provider_name, fresh.provider_name);
    }
    // Authority and the saved configuration stay with the Chat.
    for saved in &old_catalog.tool_bindings {
        let carried = prepared
            .tool_bindings
            .iter()
            .find(|tool| tool.capability_id == saved.capability_id)
            .unwrap();
        assert_eq!(carried.options, saved.options);
        assert_eq!(carried.requires_approval, saved.requires_approval);
        assert_eq!(carried.secret, saved.secret);
        assert_eq!(carried.file_access_version, saved.file_access_version);
        assert_eq!(carried.internal_id, saved.internal_id);
        assert_eq!(carried.configuration, saved.configuration);
        assert_eq!(carried.limit, saved.limit);
    }
    assert_eq!(prepared.snapshot, old_catalog.snapshot);
    assert_eq!(prepared.provider, old_catalog.provider);

    // An echoed approval mode, provider or context hash is not new authority and
    // cannot replace the saved configuration.
    let mut drifted = followup.clone();
    drifted.request_id = stable("command.later-pass-drift").unwrap();
    drifted.tools[0].options.approval_mode =
        Some(crate::runtime::approvals::ApprovalMode::FullAccess);
    drifted.provider.model = "another-model".into();
    drifted.frozen_context_hash = format!("sha256:{}", "c".repeat(64));
    let (kept, _) = pipeline.validated_prepared(&drifted).unwrap();
    assert_eq!(kept.tool_bindings[0].options, old_catalog.tool_bindings[0].options);
    assert_eq!(kept.provider, old_catalog.provider);
    assert_eq!(kept.snapshot, old_catalog.snapshot);

    // An exact command replay keeps its identity and does no provider work.
    let (_, replayed) = pipeline.validated_prepared(&first).unwrap();
    assert!(replayed);
    assert_eq!(control.calls.load(Ordering::SeqCst), 0);
}

/// The current document decides which capabilities a pass may call: a tool the
/// user binds mid-Chat is frozen for it, offered to the model immediately, bound
/// in the broker's manifest and listed for the worker, while everything the Chat
/// already held keeps the authority it was frozen with.
#[test]
fn later_pass_adopts_a_capability_bound_mid_chat() {
    let root = TempDir::new().unwrap();
    let control = Arc::new(ContinuationControl::default());
    let (pipeline, credentials, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    drop(pipeline);
    let pipeline = WorkflowExecutionPipeline::compose(
        root.path(),
        credentials,
        Arc::new(ContinuationProviderFactory(control.clone())),
    )
    .unwrap();
    let first = native_request(metadata);
    let (original, _) = pipeline.validated_prepared(&first).unwrap();
    pipeline.records.record_execution(&original).unwrap();

    let bound = ["tool.shell.host", "tool.python.host", "tool.todo"];
    let mut followup = first.clone();
    followup.request_id = stable("command.bound-mid-chat").unwrap();
    followup.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Use the task list you reported missing".into(),
        images: Vec::new(),
    });
    followup.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(bound);
    followup.tools.push(WorkflowToolBindingV1 {
        capability_id: "tool.todo".into(),
        configuration: json!({"authorityMode": "run_todo"}),
        options: Default::default(),
        credential_bindings: Vec::new(),
        definition: None,
    });
    let (prepared, _) = pipeline.validated_prepared(&followup).unwrap();
    // The pass runs the current document.
    assert_eq!(
        prepared.worker_proposal.payload["config"]["workflow"],
        followup.workflow_snapshot
    );
    assert_eq!(prepared.snapshot, original.snapshot);
    // The capability is bound, listed for the worker and bound for the broker.
    assert!(
        prepared
            .tool_bindings
            .iter()
            .any(|tool| tool.capability_id == "tool.todo")
    );
    let todo_ref = stable("tool.todo").unwrap();
    assert!(
        prepared
            .agent_checkpoint
            .config
            .allowed_tool_capability_refs
            .contains(&todo_ref)
    );
    let manifest_binding = prepared
        .manifest
        .capability_bindings
        .iter()
        .find(|binding| binding.capability_id == todo_ref)
        .expect("the broker must be able to settle the newly bound capability");
    assert!(manifest_binding.enabled && manifest_binding.compatible);
    // An already held capability keeps the authority it was frozen with.
    for saved in original
        .tool_bindings
        .iter()
        .filter(|tool| tool.capability_id != "tool.todo")
    {
        let carried = prepared
            .tool_bindings
            .iter()
            .find(|tool| tool.capability_id == saved.capability_id)
            .unwrap();
        assert_eq!(carried.options, saved.options);
        assert_eq!(carried.requires_approval, saved.requires_approval);
        assert_eq!(carried.secret, saved.secret);
        assert_eq!(carried.limit, saved.limit);
    }

    // A capability the document binds but the desktop cannot resolve blocks the
    // pass instead of running with an unresolved tool.
    let mut unresolved = followup.clone();
    unresolved.request_id = stable("command.unresolved-tool").unwrap();
    unresolved.tools.pop();
    assert!(pipeline.validated_prepared(&unresolved).is_err());

    // A document that is not an executable v1 workflow blocks the pass too.
    let mut broken = followup.clone();
    broken.request_id = stable("command.broken-document").unwrap();
    broken.workflow_snapshot = json!({"obsoleteEditorFormat": true});
    assert!(pipeline.validated_prepared(&broken).is_err());
    assert_eq!(control.calls.load(Ordering::SeqCst), 0);
}

/// An MCP definition change is adopted as the current interface, never hidden:
/// the invocation keeps its own record of the interface it actually used.
#[test]
fn dynamic_mcp_definition_change_is_adopted_rather_than_hidden() {
    let mut requested = vec![WorkflowToolBindingV1 {
        capability_id: MCP_FIXTURE_CAPABILITY.into(),
        configuration: json!({"serverId": MCP_FIXTURE_SERVER,"tool": MCP_FIXTURE_TOOL}),
        options: Default::default(),
        credential_bindings: Vec::new(),
        definition: Some(mcp_echo_definition()),
    }];
    let saved = freeze_file_tool_bindings(&requested).unwrap();
    requested[0].definition.as_mut().unwrap().description = "Changed remote tool".into();
    let changed = super::super::frozen_tools::effective(&requested, Some(&saved)).unwrap();
    assert_ne!(changed, saved, "MCP definition drift must not be hidden");
}

/// Diagnostic input is a read-only export of one prepared record, never a live
/// database. Preflight is exercised in a temporary store without provider effects.
#[test]
#[ignore = "set AWORKIT_CAPTURED_EXECUTION to a locally exported prepared record"]
fn captured_chat_continuation_adopts_the_current_document_without_widening_authority() {
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
    // The current document is the one the capture recorded for this Chat.
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
    let (resumed, _) = pipeline
        .validated_prepared(&followup)
        .expect("the captured Chat must continue");
    // Authority, Chat identity and the recorded workspace carry over.
    assert_eq!(resumed.snapshot, original.snapshot);
    assert_eq!(resumed.workspace, original.workspace);
    assert_eq!(resumed.provider, original.provider);
    for binding in &original.manifest.capability_bindings {
        assert!(
            resumed
                .manifest
                .capability_bindings
                .iter()
                .any(|candidate| candidate == binding),
            "the authority ceiling must not shrink"
        );
    }
    // The interface and the document are the current ones.
    assert_eq!(
        resumed.worker_proposal.payload["config"]["workflow"],
        followup.workflow_snapshot
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "preflight must not invoke any provider"
    );
}
