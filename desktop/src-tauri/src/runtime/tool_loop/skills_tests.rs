//! Exercise catalog durability through the real native authority and history store.
use super::*;
use aworkit_capability_host::AdapterRegistry;
use aworkit_protocol::{AttestedExtensionSetV1, attested_extension_set_hash_v1};

#[test]
fn skill_catalog_replay_refresh_visibility_and_human_invocation_are_durable() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let skills_dir = workspace.join(".aworkit/skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let write = |name: &str, description: &str, flags: &str, body: &str| {
        std::fs::write(
            skills_dir.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: {description}\n{flags}---\n{body}"),
        )
        .unwrap();
    };
    write("test", "first description", "", "first body");
    write(
        "manual",
        "hidden manual",
        "disable-model-invocation: true\n",
        "direct user body",
    );
    let projects = ProjectCoordinator::open(root.path().join("projects")).unwrap();
    let descriptors = file_tool_descriptors().unwrap();
    let mut registry = AdapterRegistry::default();
    for descriptor in descriptors.values() {
        registry.register_capability(descriptor.clone()).unwrap();
    }
    let generation = ProcessGeneration(19);
    let mut attested = AttestedExtensionSetV1 {
        host_id: stable("host.skill-test").unwrap(),
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
    let database = root.path().join("invocations.sqlite3");
    let runtime = FileToolAuthorityRuntimeV1::open(
        &database,
        crate::runtime::images::ChatImageStore::new(root.path()),
        projects.clone(),
        host,
        descriptors.clone(),
        generation,
        key,
        Arc::new(aworkit_trusted_core::NativeCredentialStore::new()),
    )
    .unwrap();
    let config = json!({"includeDefaultRoots":true,"aworkitHome":root.path().join("global"),"agentsHome":root.path().join("shared"),"customSkillDirs":[],"bundledSkillDir":"","catalogDescriptionMaxLength":500});
    let tool = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        options: Default::default(),
        capability_id: SKILL_CAPABILITY_ID.into(),
        configuration: config,
        credential_bindings: Vec::new(),
        definition: None,
    }])
    .unwrap()
    .remove(0);
    let definitions = vec![ModelToolDefinitionV1 {
        capability_id: tool.capability_id.clone(),
        name: tool.provider_name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }];
    let binding = file_tool_capability_binding(&tool, &descriptors[SKILL_CAPABILITY_ID]).unwrap();
    assert!(!tool.requires_approval);
    let mut authority = runtime.bind(FrozenFileToolAuthorityContextV1 {
        chat_id: "chat.skills".into(),
        approvals: Default::default(),
        review_messages: vec![super::super::pipeline::WorkflowMessageV1 {
            role: "user".into(),
            content: "Use /manual /manual /unknown".into(),
            images: Vec::new(),
        }],
        manifest: AuthorityManifestV1 {
            manifest_id: stable("manifest.skill-test").unwrap(),
            manifest_hash: format!("sha256:{}", "1".repeat(64)),
            capability_bindings: vec![binding],
            summary: "skill test".into(),
        },
        run_id: stable("run.skill-test").unwrap(),
        request_id: stable("command.skill-test").unwrap(),
        node_id: stable("agent.1").unwrap(),
        workspace: projects.resolve_workspace_v1(&workspace).unwrap(),
        project_branch: None,
        bindings: vec![tool],
        deadline_epoch_millis: u64::MAX,
        model_gateway: None,
        model_binding_id: None,
        model_version_hash: None,
        model_context: serde_json::json!({}),
        maximum_tool_output_bytes: MAXIMUM_TOOL_RESULT_BYTES,
        mcp_manifests: BTreeMap::new(),
        cancellation: CancellationToken::default(),
    });
    let outer = stable("invocation.skill-test").unwrap();
    let cancel = CancellationToken::default();
    let initial = authority
        .prepare_context(&outer, 0, &definitions, &cancel)
        .unwrap();
    assert_eq!(initial.len(), 2);
    assert!(initial[0].content.contains("first description"));
    assert!(!initial[0].content.contains("hidden manual"));
    assert!(initial[1].content.contains("direct user body"));
    write("test", "second description", "", "second body");
    authority.runtime.records = Arc::new(ToolRecordStore::open(&database).unwrap());
    assert_eq!(
        authority
            .prepare_context(&outer, 0, &definitions, &cancel)
            .unwrap(),
        initial,
        "reopened step must retain original context"
    );
    let next = authority
        .prepare_context(&outer, 1, &definitions, &cancel)
        .unwrap();
    assert_eq!(&next[..2], &initial);
    assert_eq!(next.len(), 3);
    assert_eq!(next[2].after_exchanges, 1);
    assert!(next[2].content.contains("second description"));
    assert_eq!(
        authority
            .prepare_context(&outer, 2, &definitions, &cancel)
            .unwrap(),
        next,
        "unchanged catalog does not republish"
    );
    let hidden = authority.prepare_context(&outer, 3, &[], &cancel).unwrap();
    assert!(
        hidden
            .last()
            .unwrap()
            .content
            .contains("No skills are currently available")
    );
    let call = ModelToolCallV1 {
        call_id: "skill.load".into(),
        provider_call_id: Some("skill.load".into()),
        capability_id: SKILL_CAPABILITY_ID.into(),
        name: "skill".into(),
        arguments: json!({"name":"test"}),
        provider_context: None,
    };
    let result = authority.invoke(&outer, 1, &call, &cancel).unwrap();
    assert_eq!(result.result.content["content"], "second body");
    assert!(
        skills::model_result(&result.result, SKILL_CAPABILITY_ID)
            .content
            .as_str()
            .unwrap()
            .contains("<skill_instructions>\nsecond body")
    );
    write("test", "third description", "", "third body");
    assert_eq!(
        authority.invoke(&outer, 1, &call, &cancel).unwrap().result,
        result.result,
        "settled tool result does not reload during replay"
    );
    let manual = ModelToolCallV1 {
        call_id: "skill.manual".into(),
        provider_call_id: Some("skill.manual".into()),
        arguments: json!({"name":"manual"}),
        ..call.clone()
    };
    let rejected = authority.invoke(&outer, 2, &manual, &cancel).unwrap();
    assert!(rejected.result.is_error);
    assert_eq!(
        skills::model_result(&rejected.result, SKILL_CAPABILITY_ID).content,
        "Error: skill \"manual\" is not available for model invocation"
    );
}
