//! Regression coverage for freezing credential-backed built-in web search.

use super::*;

#[test]
fn freezes_only_opaque_tool_binding_metadata() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut runtime = runtime(&root, provider.clone());
    configure(&mut runtime);

    runtime
        .settings_v2_store_credential(CredentialStoreInputV2 {
            command_id: "settings.web-search-credential.create".into(),
            expected_version: runtime.settings_v2_snapshot().version,
            replace_credential_ref: None,
            label: "DeepSeek search API key".into(),
            kind: "api_key".into(),
            bound_provider_id: None,
            bound_endpoint: None,
            fields: BTreeMap::from([(
                "api_key".into(),
                "must-not-enter-history".to_owned().into(),
            )]),
        })
        .expect("credential metadata");
    let credential_ref = runtime.settings_v2_snapshot().settings.credentials[0]
        .credential_ref
        .clone();

    let mut settings = runtime.settings_v2_snapshot().settings;
    let model = settings
        .providers
        .iter_mut()
        .flat_map(|provider| provider.models.iter_mut())
        .find(|model| model.remote_id == "fixture-model")
        .expect("fixture model");
    if !model
        .capabilities
        .iter()
        .any(|capability| capability == "tools")
    {
        model.capabilities.push("tools".into());
    }
    let web_search = settings
        .tools
        .iter_mut()
        .find(|tool| tool.id == "tool.web_search")
        .expect("web search tool");
    web_search.enabled = true;
    web_search
        .configuration
        .insert("backend".into(), Value::String("deepseek".into()));
    web_search.credential_bindings = vec![
        super::super::super::settings_v2::NamedCredentialBindingV2 {
            name: "api_key".into(),
            credential_ref: credential_ref.clone(),
            field: "api_key".into(),
        },
    ];
    runtime
        .settings_v2_commit(SettingsV2CommitInput {
            command_id: "settings.web-search-credential.bind".into(),
            expected_version: runtime.settings_v2_snapshot().version,
            settings,
        })
        .expect("credentialed web-search Settings");

    let mut workflow = runtime.workflow_snapshot_for("workflow.simple-chat".into());
    workflow.document["nodes"][1]["configuration"]["toolIds"] = json!(["tool.web_search"]);
    runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.web-search-credential".into(),
            expected_version: workflow.version,
            document: workflow.document,
            workflow_id: Some("workflow.simple-chat".into()),
        })
        .expect("credentialed web-search workflow");

    runtime
        .command(send(
            "chat.credentialed-web-search",
            0,
            "Find today's price",
        ))
        .expect("credentialed web search must pass durable context freeze");

    let frozen = runtime
        .history
        .current_frozen_context()
        .expect("frozen context read")
        .expect("frozen context");
    let web_search = frozen
        .context
        .tools
        .iter()
        .find(|tool| tool.tool_id == "tool.web_search")
        .expect("frozen web search");
    assert_eq!(web_search.credentials.len(), 1);
    assert_eq!(
        web_search.credentials[0].credential_ref.as_str(),
        credential_ref
    );
    let durable = serde_json::to_value(web_search).expect("durable tool binding");
    assert!(durable.get("opaqueBindings").is_some());
    assert!(durable.get("credentials").is_none());
    assert_profile_excludes(root.path(), "must-not-enter-history");
}
