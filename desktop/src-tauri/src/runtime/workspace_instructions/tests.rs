//! Exercise production preparation, native file effects and durable replay.
#[path = "../compaction/integration_tests.rs"]
mod compaction_integration;
#[path = "../compression/integration_tests.rs"]
mod compression_integration;
use super::*;
use crate::runtime::pipeline::WorkflowMessageV1;
use crate::runtime::semantic_events::{
    SemanticEventCommitter, SemanticEventDraft, ephemeral_semantic_event_committer,
};
use aworkit_capability_host::AdapterRegistry;
use aworkit_protocol::{AttestedExtensionSetV1, attested_extension_set_hash_v1};

struct Fixture {
    root: tempfile::TempDir,
    authority: BoundFileToolAuthorityV1,
    agent: AgentContextV1,
    committer: Arc<dyn SemanticEventCommitter>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "ROOT V1").unwrap();
        std::fs::write(workspace.join("src/AGENTS.md"), "NESTED V1").unwrap();
        std::fs::write(workspace.join("src/file.txt"), "file").unwrap();
        let projects = ProjectCoordinator::open(root.path().join("projects")).unwrap();
        let descriptors = file_tool_descriptors().unwrap();
        let mut registry = AdapterRegistry::default();
        for descriptor in descriptors.values() {
            registry.register_capability(descriptor.clone()).unwrap();
        }
        let generation = ProcessGeneration(29);
        let mut attested = AttestedExtensionSetV1 {
            host_id: stable("host.instructions-test").unwrap(),
            host_generation: generation,
            host_protocol: 1,
            extensions: Vec::new(),
            set_hash: String::new(),
        };
        attested.set_hash = attested_extension_set_hash_v1(&attested).unwrap();
        let key = Arc::new(CoreAuthenticationKey::random().unwrap());
        let host = Arc::new(
            CapabilityHost::from_attested_registry(
                registry.materialize_attested_set(&attested).unwrap(),
                key.copy(),
                2,
            )
            .unwrap(),
        );
        let runtime = FileToolAuthorityRuntimeV1::open(
            &root.path().join("events.sqlite3"),
            projects.clone(),
            host,
            descriptors.clone(),
            generation,
            key,
            Arc::new(aworkit_trusted_core::NativeCredentialStore::new()),
        )
        .unwrap();
        let bindings = freeze_file_tool_bindings(
            &[ID, FILE_READ_CAPABILITY_ID]
                .iter()
                .map(|id| {
                    let entry = crate::runtime::tool_registry::native_tool(id).unwrap();
                    let mut configuration = serde_json::to_value(&entry.configuration).unwrap();
                    if *id == ID {
                        configuration["aworkitHome"] = json!(root.path().join("global"));
                    }
                    WorkflowToolBindingV1 {
                        options: Default::default(),
                        capability_id: (*id).into(),
                        configuration,
                        credential_bindings: Vec::new(),
                        definition: None,
                    }
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let manifest = AuthorityManifestV1 {
            manifest_id: stable("manifest.instructions-test").unwrap(),
            manifest_hash: format!("sha256:{}", "1".repeat(64)),
            capability_bindings: bindings
                .iter()
                .map(|tool| {
                    file_tool_capability_binding(tool, &descriptors[&tool.capability_id]).unwrap()
                })
                .collect(),
            summary: "instruction test".into(),
        };
        let committer = ephemeral_semantic_event_committer();
        let context = FrozenFileToolAuthorityContextV1 {
            chat_id: "chat.instructions".into(),
            approvals: super::super::super::approvals::ApprovalContext {
                chat_id: "chat.instructions".into(),
                project_key: Some("project.instructions".into()),
                ..Default::default()
            },
            review_messages: vec![WorkflowMessageV1 {
                role: "user".into(),
                content: "first".into(),
                images: Vec::new(),
            }],
            manifest,
            run_id: stable("run.instructions").unwrap(),
            request_id: stable("request.instructions").unwrap(),
            node_id: stable("agent.1").unwrap(),
            workspace: projects.resolve_workspace_v1(&workspace).unwrap(),
            project_branch: None,
            bindings,
            deadline_epoch_millis: u64::MAX,
            model_gateway: None,
            model_binding_id: None,
            model_version_hash: None,
            model_context: serde_json::json!({}),
            maximum_tool_output_bytes: MAXIMUM_TOOL_RESULT_BYTES,
            mcp_manifests: BTreeMap::new(),
            cancellation: CancellationToken::default(),
        };
        let events = Arc::new(RunEventStream::new(
            context.request_id.to_string(),
            context.run_id.to_string(),
            committer.clone(),
            CancellationToken::default(),
        ));
        let authority = runtime.bind_with_run_events(context, events);
        Self {
            root,
            authority,
            agent: AgentContextV1 {
                node_id: "agent.1".into(),
                tool_ids: vec![ID.into(), FILE_READ_CAPABILITY_ID.into()],
                child: None,
            },
            committer,
        }
    }
    fn request(&self) -> ModelToolRequestV1 {
        ModelToolRequestV1 {
            input: json!({"messages":self.authority.context.review_messages}),
            parameters: BTreeMap::new(),
            tools: self
                .authority
                .context
                .bindings
                .iter()
                .filter(|b| b.is_callable())
                .map(|b| b.definition())
                .collect(),
            exchanges: Vec::new(),
            context_messages: Vec::new(),
            retry_notice: None,
        }
    }
    fn prepare(&self, outer: &str, after: usize, request: &mut ModelToolRequestV1) {
        self.authority
            .prepare_automatic_context(
                &stable(outer).unwrap(),
                after,
                &self.agent,
                request,
                &CancellationToken::default(),
            )
            .unwrap();
    }
    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.authority.context.workspace.root.join(path), text).unwrap();
    }
}

#[test]
fn prepared_replay_reopen_cross_input_and_node_selection() {
    let mut f = Fixture::new();
    let mut first = f.request();
    f.prepare("outer.one", 0, &mut first);
    assert_eq!(first.tools.len(), 1);
    assert!(first.context_messages[0].content.contains("ROOT V1"));
    f.write("AGENTS.md", "ROOT V2");
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    let mut replay = f.request();
    f.prepare("outer.one", 0, &mut replay);
    assert_eq!(first.context_messages, replay.context_messages);
    f.authority.context.review_messages.extend([
        WorkflowMessageV1 {
            role: "assistant".into(),
            content: "answer".into(),
            images: Vec::new(),
        },
        WorkflowMessageV1 {
            role: "user".into(),
            content: "second".into(),
            images: Vec::new(),
        },
    ]);
    let mut next = f.request();
    f.prepare("outer.two", 0, &mut next);
    assert_eq!(next.context_messages.len(), 2);
    assert_eq!(next.context_messages[0].after_input_messages, Some(1));
    assert!(
        next.context_messages[1]
            .content
            .contains("Updated instructions from: AGENTS.md")
    );
    assert!(
        next.projected_input().unwrap()["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("ROOT V1")
    );
    f.agent.tool_ids.clear();
    let mut disabled = f.request();
    f.prepare("outer.off", 0, &mut disabled);
    assert!(disabled.context_messages.is_empty());
    f.agent.tool_ids.push(ID.into());
    f.agent.node_id = "agent.other".into();
    let mut other = f.request();
    f.prepare("outer.other", 0, &mut other);
    assert_eq!(other.context_messages.len(), 1);
    assert!(!other.context_messages[0].content.contains("ROOT V1"));
}

#[test]
fn settled_file_touch_projects_after_exchange_and_compaction_restores_on_first_request() {
    let mut f = Fixture::new();
    let outer = stable("outer.effects").unwrap();
    let mut first = f.request();
    f.prepare(outer.as_str(), 0, &mut first);
    let call = ModelToolCallV1 {
        call_id: "read.1".into(),
        provider_call_id: Some("read.1".into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":"src/file.txt"}),
        provider_context: None,
    };
    let settled = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    assert!(!settled.result.is_error, "{:?}", settled.result);
    let exchange = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall {
            call,
        }],
        results: vec![settled.result],
    };
    let mut unsettled = f.request();
    unsettled.exchanges.push(exchange.clone());
    f.prepare(outer.as_str(), 1, &mut unsettled);
    assert!(
        !unsettled
            .context_messages
            .iter()
            .any(|m| m.content.contains("NESTED")),
        "uncommitted exchanges cannot trigger discovery"
    );
    // A new step after admission observes the committed tool result.
    f.authority.commit_exchange(&outer, 2, &exchange).unwrap();
    assert_eq!(
        f.authority.settled_instruction_touches(&outer, 2).unwrap(),
        vec![PathBuf::from("src/file.txt")],
        "exchanges={:?}; outcomes={:?}",
        f.authority
            .runtime
            .records
            .events("pipeline.model-tool-exchange"),
        f.authority.runtime.records.events("pipeline.tool-outcome")
    );
    let mut next = f.request();
    next.exchanges = vec![exchange.clone(), exchange];
    f.prepare(outer.as_str(), 2, &mut next);
    let nested = next
        .context_messages
        .iter()
        .find(|m| m.content.contains("NESTED"))
        .unwrap_or_else(|| {
            panic!(
                "context={:?}; steps={:?}",
                next.context_messages,
                f.authority.runtime.records.events(KIND)
            )
        });
    assert_eq!(nested.after_exchanges, 2);
    // Replace the actual selection, retaining only a summary and no authentic refs.
    f.committer.commit(vec![SemanticEventDraft::new("context.edited",json!({"nodeId":"agent.1","document":{
        "input":{"messages":[{"role":"user","content":"Compaction summary mentions all rules"}]},"tools":next.tools,"exchanges":[],"contextMessages":[]
    }}))]).unwrap();
    f.write("AGENTS.md", "ROOT V2");
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    let mut restored = f.request();
    f.prepare("outer.after-compaction", 0, &mut restored);
    assert_eq!(restored.context_messages.len(), 1);
    assert!(restored.context_messages[0].content.contains("ROOT V2"));
    assert!(restored.context_messages[0].content.contains("NESTED V1"));
    let mut replay = f.request();
    f.prepare("outer.after-compaction", 0, &mut replay);
    assert_eq!(restored.context_messages, replay.context_messages);
}

#[test]
fn activation_contract_rejects_fake_calls_and_model_fields() {
    let f = Fixture::new();
    let binding = &f.authority.context.bindings[0];
    assert!(!binding.is_callable());
    assert!(validate_call_arguments(binding, &json!({})).is_err());
    let mut manifest: Value = serde_json::from_str(include_str!(
        "../../../../tool-plugins/aworkit-native/tool-plugin.json"
    ))
    .unwrap();
    let entry = manifest["tools"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["id"] == ID)
        .unwrap();
    entry["providerName"] = json!("instructions");
    assert!(crate::runtime::tool_registry::NativeToolPlugin::parse(&manifest.to_string()).is_err());
}

#[test]
fn parallel_owners_are_isolated_and_same_request_admits_once() {
    let f = Fixture::new();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let mut request = f.request();
            f.prepare("parallel.same", 0, &mut request);
            request
        });
        let second = scope.spawn(|| {
            let mut request = f.request();
            f.prepare("parallel.same", 0, &mut request);
            request
        });
        assert_eq!(
            first.join().unwrap().context_messages,
            second.join().unwrap().context_messages
        );
        let other = scope.spawn(|| {
            let agent = AgentContextV1 {
                node_id: "agent.parallel".into(),
                ..f.agent.clone()
            };
            let mut request = f.request();
            f.authority
                .prepare_automatic_context(
                    &stable("parallel.other").unwrap(),
                    0,
                    &agent,
                    &mut request,
                    &CancellationToken::default(),
                )
                .unwrap();
            request
        });
        assert_eq!(other.join().unwrap().context_messages.len(), 1);
    });
    let steps = f.authority.runtime.records.events(KIND).unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps
            .iter()
            .filter(|s| s["owner"]["node"] == "agent.1")
            .count(),
        1
    );
}

#[test]
fn successful_child_touch_survives_failed_parent_but_waits_for_its_exchange() {
    let f = Fixture::new();
    let outer = stable("outer.composite").unwrap();
    let mut request = f.request();
    f.prepare(outer.as_str(), 0, &mut request);
    let call = |id: &str, path: &str| ModelToolCallV1 {
        call_id: id.into(),
        provider_call_id: Some(id.into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":path}),
        provider_context: None,
    };
    // Use real durable invocations to construct a trusted parent/child failure
    // fixture. No unstructured tool result can manufacture this relationship.
    let parent_call = call("parent.failed", "missing.txt");
    let parent = f
        .authority
        .invoke(&outer, 1, &parent_call, &CancellationToken::default())
        .unwrap();
    assert!(parent.result.is_error);
    let parent_id = parent.activity.invocation_id.clone();
    let child_call = call("child.success", "src/file.txt");
    let child = f
        .authority
        .invoke(&parent_id, 1, &child_call, &CancellationToken::default())
        .unwrap();
    assert!(!child.result.is_error);
    assert!(
        f.authority
            .settled_instruction_touches(&outer, 1)
            .unwrap()
            .is_empty()
    );
    f.authority
        .commit_exchange(
            &outer,
            1,
            &aworkit_capability_host::ModelToolExchangeV1 {
                assistant_content: vec![
                    aworkit_capability_host::ModelAssistantContentV1::ToolCall {
                        call: parent_call,
                    },
                ],
                results: vec![parent.result],
            },
        )
        .unwrap();
    assert_eq!(
        f.authority.settled_instruction_touches(&outer, 1).unwrap(),
        vec![PathBuf::from("src/file.txt")]
    );
}
