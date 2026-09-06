//! Registry and migration contracts, independent of installed third-party programs.
use super::*;
use serde_json::json;

#[test]
fn bundled_manifest_drives_valid_settings_and_unique_model_definitions() {
    let defaults = native_defaults();
    let mut names = BTreeSet::new();
    for entry in &native_plugin().tools {
        assert!(names.insert(&entry.provider_name));
        let settings = defaults.iter().find(|tool| tool.id == entry.id).unwrap();
        let frozen = super::super::tool_loop::freeze_file_tool_bindings(&[
            super::super::tool_loop::WorkflowToolBindingV1 {
                capability_id: entry.id.clone(),
                configuration: serde_json::to_value(&settings.configuration).unwrap(),
                credential_bindings: vec![],
                definition: None,
                options: ToolOptions::default(),
            },
        ])
        .unwrap();
        assert_eq!(frozen[0].provider_name, entry.provider_name);
        assert_eq!(frozen[0].input_schema, entry.input_schema);
    }
    let mut malformed: Value = serde_json::from_str(BUNDLED).unwrap();
    malformed["tools"][1] = malformed["tools"][0].clone();
    assert!(NativeToolPlugin::parse(&malformed.to_string()).is_err());
    malformed = serde_json::from_str(BUNDLED).unwrap();
    malformed["tools"][0]["fields"] = json!([]);
    assert!(NativeToolPlugin::parse(&malformed.to_string()).is_err());
}

#[test]
fn instructions_are_selected_frozen_and_legacy_serialization_is_unchanged() {
    let mut tool = native_defaults()
        .into_iter()
        .find(|tool| tool.id == "tool.web_fetch")
        .unwrap();
    assert!(
        serde_json::to_value(&tool)
            .unwrap()
            .get("options")
            .is_none()
    );
    let frozen = freeze_settings(&tool).unwrap();
    tool.options.instructions = Some("Custom retrieval guidance".into());
    let customized = freeze_settings(&tool).unwrap();
    assert_ne!(frozen.options.instructions, customized.options.instructions);
    assert_eq!(
        customized.options.instructions.as_deref(),
        Some("Custom retrieval guidance")
    );
    let block = instruction_block([("tool.web_fetch", &frozen.options)]);
    assert!(block.contains("Tool instructions for tool.web_fetch:"));
    assert!(!block.contains("Tool instructions for tool.web_search:"));
    tool.options.instructions = Some(String::new());
    assert!(
        instruction_block([("tool.web_fetch", &freeze_settings(&tool).unwrap().options)])
            .is_empty()
    );
    assert!(instruction_block([("legacy", &ToolOptions::default())]).is_empty());
}

#[test]
fn prompt_migration_preserves_custom_workflows_and_is_idempotent() {
    let mut workflow = json!({"nodes":[{"type":"agent","configuration":{"instructions":
        "You are Aworkit's standard agent. Keep the todo list current, inspect project evidence with the file tools, use web_search and web_fetch when current information is required, and produce a final answer with citations. Do not claim tool results you did not receive."}}]});
    assert!(migrate_persona(&mut workflow));
    assert!(!workflow.to_string().contains("web_search"));
    assert!(!migrate_persona(&mut workflow));
    let mut custom = json!({"nodes":[{"type":"agent","configuration":{"instructions":"Keep our custom persona."}}]});
    assert!(!migrate_persona(&mut custom));
    assert_eq!(
        custom["nodes"][0]["configuration"]["instructions"],
        "Keep our custom persona."
    );
}

#[test]
fn tool_options_reject_invalid_execution_and_preserve_the_resolved_path() {
    assert!(
        ToolOptions {
            executable: Some("relative.exe".into()),
            ..Default::default()
        }
        .validate("shell")
        .is_err()
    );
    let path = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let options = ToolOptions {
        executable: Some(path.clone()),
        ..Default::default()
    };
    assert!(options.validate("mcp").is_err());
    let mut tool = native_defaults()
        .into_iter()
        .find(|tool| tool.id == "tool.shell.host")
        .unwrap();
    tool.options = options;
    let frozen = freeze_settings(&tool).unwrap();
    assert!(std::path::Path::new(frozen.options.executable.as_ref().unwrap()).is_absolute());
    assert_eq!(freeze_settings(&frozen).unwrap(), frozen);
}

#[test]
fn plugin_discovery_is_inert_and_changed_packages_fail_verification() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("fixture");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("tool-plugin.json");
    let manifest = json!({"schemaVersion":1,"id":"plugin.fixture","name":"Fixture","version":"1.0.0",
        "execution":{"transport":"stdio","command":"missing-server.exe","args":[],"env":[]},
        "tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"},"enabled":true}]});
    std::fs::write(&path, manifest.to_string()).unwrap();
    let discoveries = discovery::discover(root.path());
    let server = discoveries[0].server.as_ref().unwrap();
    assert!(!server.enabled && !server.auto_connect);
    let super::super::settings_v2::IntegrationTransportV2::Stdio { command, cwd, .. } =
        &server.transport
    else {
        panic!("stdio")
    };
    assert!(std::path::Path::new(command).is_absolute());
    assert_eq!(
        std::path::Path::new(cwd.as_ref().unwrap()),
        std::fs::canonicalize(&folder).unwrap()
    );
    let pin = server.plugin.as_ref().unwrap();
    discovery::verify(pin).unwrap();
    std::fs::write(&path, manifest.to_string() + "\n").unwrap();
    assert!(discovery::verify(pin).unwrap_err().contains("changed"));
    let second = root.path().join("duplicate");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(second.join("tool-plugin.json"), manifest.to_string()).unwrap();
    assert_eq!(
        discovery::discover(root.path())
            .iter()
            .filter(|entry| entry.error.is_some())
            .count(),
        1
    );
    let mut traversal = manifest.clone();
    traversal["execution"]["command"] = "../outside.exe".into();
    std::fs::write(&path, traversal.to_string()).unwrap();
    assert!(discovery::inspect(&path).unwrap_err().contains("within"));
}

#[test]
fn mcp_working_directory_affects_frozen_transport_and_rejects_relative_paths() {
    use super::super::{
        mcp::prepare_mcp_server,
        settings_v2::{IntegrationTransportV2, McpServerConfigurationV2},
    };
    let mut server = McpServerConfigurationV2 {
        id: "mcp.fixture".into(),
        name: "Fixture".into(),
        enabled: true,
        auto_connect: false,
        tools: vec![],
        plugin: None,
        transport: IntegrationTransportV2::Stdio {
            command: "fixture".into(),
            args: vec![],
            cwd: None,
            env: vec![],
        },
    };
    let original = prepare_mcp_server(&server, &[]).unwrap();
    let IntegrationTransportV2::Stdio { cwd, .. } = &mut server.transport else {
        unreachable!()
    };
    *cwd = Some(std::env::temp_dir().to_string_lossy().into_owned());
    let updated = prepare_mcp_server(&server, &[]).unwrap();
    assert_ne!(
        serde_json::to_value(&original.endpoint).unwrap(),
        serde_json::to_value(&updated.endpoint).unwrap()
    );
    let IntegrationTransportV2::Stdio { cwd, .. } = &mut server.transport else {
        unreachable!()
    };
    *cwd = Some("relative".into());
    assert!(
        prepare_mcp_server(&server, &[])
            .err()
            .unwrap()
            .contains("absolute")
    );
}
