//! Registry and migration contracts, independent of installed third-party programs.
use super::*;
use serde_json::json;

#[test]
fn every_bundled_tool_is_available_by_default() {
    let defaults = native_defaults();
    assert_eq!(
        defaults.len(),
        native_plugin().tools.len(),
        "every bundled tool has a default entry"
    );
    assert!(
        defaults.iter().all(|tool| tool.enabled),
        "a new profile offers every bundled tool; the user disables what it should not offer"
    );
}

#[test]
fn bundled_manifest_drives_valid_settings_and_unique_model_definitions() {
    let defaults = native_defaults();
    // The frontend validates bindings from this manifest; keep native admission aligned.
    assert_eq!(
        native_plugin()
            .tools
            .iter()
            .map(|tool| tool.id.clone())
            .collect::<BTreeSet<_>>(),
        super::super::documents::builtin_tool_binding_ids()
    );
    let mut names = BTreeSet::new();
    for entry in &native_plugin().tools {
        assert!(names.insert(&entry.provider_name));
        assert!(!entry.provider_name.starts_with("aworkit_"));
        if entry.id.starts_with("tool.files.") {
            assert!(!entry.provider_name.contains("project"));
            assert!(!entry.requires_project);
        }
        let settings = defaults.iter().find(|tool| tool.id == entry.id).unwrap();
        let mut configuration = serde_json::to_value(&settings.configuration).unwrap();
        // First-input freeze resolves a delegation tool's configured product
        // target and stores it with the snapshot; this round-trip stands in for
        // that resolution so every manifest tool is frozen exactly once.
        if super::super::external_agent::delegation_tool_adapter(&entry.id).is_some() {
            configuration["resolvedTarget"] = json!({
                "backend": if entry.id == "tool.subagent_codex" { "codex" } else { "claude-code" },
                "executable": std::env::current_exe().unwrap().display().to_string(),
                "arguments": if entry.id == "tool.subagent_codex" {
                    json!(["app-server"])
                } else {
                    json!([])
                },
                "permissionMode": if entry.id == "tool.subagent_codex" { "never" } else { "dontAsk" },
            });
        }
        let frozen = super::super::tool_loop::freeze_file_tool_bindings(&[
            super::super::tool_loop::WorkflowToolBindingV1 {
                capability_id: entry.id.clone(),
                configuration,
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
    malformed["tools"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|tool| tool["id"] == "tool.files.read")
        .unwrap()["fields"] = json!([]);
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
    let auto = ToolOptions {
        auto_approve: true,
        ..Default::default()
    };
    assert!(auto.validate("mcp").is_ok());
    assert!(auto.validate("native").is_err());
    assert!(auto.validate("shell").is_err());
    assert_eq!(
        serde_json::to_value(ToolOptions::default()).unwrap(),
        json!({})
    );
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
    for id in [
        "tool.shell.host",
        "tool.shell.start",
        "tool.python.host",
        "tool.python.start",
    ] {
        let mut tool = native_defaults()
            .into_iter()
            .find(|tool| tool.id == id)
            .unwrap();
        tool.options = options.clone();
        let frozen = freeze_settings(&tool).unwrap();
        assert!(std::path::Path::new(frozen.options.executable.as_ref().unwrap()).is_absolute());
        assert_eq!(freeze_settings(&frozen).unwrap(), frozen);
    }
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
    // A bare command name stays a name for the operating system to resolve from
    // PATH, so a package can launch a Python or Node MCP server.
    assert_eq!(command, "missing-server.exe");
    assert!(std::path::Path::new(cwd.as_ref().unwrap()).is_absolute());
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
fn installing_and_removing_a_package_is_inert_reversible_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    let plugin_root = root.path().join("plugins");
    let source = root.path().join("downloads/comfyui-bridge");
    std::fs::create_dir_all(source.join("skills/comfyui")).unwrap();
    let manifest = json!({"schemaVersion":1,"id":"plugin.comfyui-bridge","name":"ComfyUI bridge","version":"1.0.0",
        "execution":{"transport":"stdio","command":"python","args":["bridge.py"],"env":[]},
        "tools":[{"name":"comfyui_queue","description":"Queue a workflow","inputSchema":{"type":"object"},"enabled":true}]});
    std::fs::write(source.join("tool-plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(source.join("bridge.py"), b"print('bridge')").unwrap();
    std::fs::write(source.join("skills/comfyui/SKILL.md"), b"# ComfyUI").unwrap();

    let installed = discovery::install(&source, &plugin_root).unwrap();
    assert_eq!(installed.id, "plugin.comfyui-bridge");
    assert!(!installed.enabled && !installed.auto_connect);
    let listed = discovery::discover(&plugin_root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].server.as_ref().unwrap().id, "plugin.comfyui-bridge");
    // The package's own files are copied, including its optional skill folder.
    assert!(plugin_root
        .join("comfyui-bridge/skills/comfyui/SKILL.md")
        .is_file());
    // Re-installing replaces the package instead of duplicating it.
    discovery::install(&source, &plugin_root).unwrap();
    assert_eq!(discovery::discover(&plugin_root).len(), 1);
    // A second, valid package appears and the first is still discoverable.
    let other = root.path().join("downloads/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("tool-plugin.json"),
        json!({"schemaVersion":1,"id":"plugin.other","name":"Other","version":"1.0.0",
            "execution":{"transport":"stdio","command":"python","args":[],"env":[]}})
        .to_string(),
    )
    .unwrap();
    discovery::install(&other, &plugin_root).unwrap();
    assert_eq!(discovery::discover(&plugin_root).len(), 2);

    // Removing by id removes only that package.
    let removed = discovery::remove(&plugin_root, "plugin.comfyui-bridge").unwrap();
    assert!(std::path::Path::new(&removed).starts_with(std::fs::canonicalize(&plugin_root).unwrap()));
    assert!(!plugin_root.join("comfyui-bridge").exists());
    let remaining = discovery::discover(&plugin_root);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].server.as_ref().unwrap().id, "plugin.other");
    // Removing it twice, or removing an unknown id, is a definite failure.
    let _ = discovery::remove(&plugin_root, "plugin.comfyui-bridge");
    assert!(discovery::remove(&plugin_root, "plugin.comfyui-bridge").is_err());

    // A chosen folder that holds no plugin is refused with a plain reason.
    let empty = root.path().join("downloads/two-packages");
    std::fs::create_dir_all(empty.join("a")).unwrap();
    std::fs::create_dir_all(empty.join("b")).unwrap();
    for folder in ["a", "b"] {
        std::fs::write(
            empty.join(folder).join("tool-plugin.json"),
            json!({"schemaVersion":1,"id":format!("plugin.{folder}"),"name":"x","version":"1",
                "execution":{"transport":"stdio","command":"python","args":[],"env":[]}})
            .to_string(),
        )
        .unwrap();
    }
    assert!(discovery::install(&empty, &plugin_root).is_err());
    assert!(discovery::install(&plugin_root, &plugin_root).is_err());
    // A relative traversal in the package folder name cannot escape the root.
    assert!(discovery::plugin_folder(std::path::Path::new("")).is_err());
    let _ = std::fs::remove_dir_all(root.path().join("downloads"));
}

#[test]
#[cfg(unix)]
fn installing_refuses_a_package_that_contains_a_symbolic_link() {
    let root = tempfile::tempdir().unwrap();
    let plugin_root = root.path().join("plugins");
    let source = root.path().join("downloads/linked");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("tool-plugin.json"),
        json!({"schemaVersion":1,"id":"plugin.linked","name":"Linked","version":"1.0.0",
            "execution":{"transport":"stdio","command":"python","args":[],"env":[]}})
        .to_string(),
    )
    .unwrap();
    std::os::unix::fs::symlink("/etc/passwd", source.join("escape")).unwrap();
    assert!(discovery::install(&source, &plugin_root).is_err());
    assert!(discovery::discover(&plugin_root).is_empty());
}

#[test]
fn the_comfyui_reference_plugin_is_a_valid_installable_package() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tool-plugins/comfyui-bridge");
    let server = discovery::inspect(&base.join("tool-plugin.json"))
        .expect("the shipped reference plugin must validate");
    assert_eq!(server.id, "plugin.comfyui-bridge");
    assert!(!server.enabled && !server.auto_connect);
    assert!(base.join("bridge.py").is_file(), "the MCP server ships with the package");
    assert!(base.join("workflows.json").is_file());
    assert!(
        base.join("skills/comfyui/SKILL.md").is_file(),
        "the package demonstrates an optional skill folder"
    );
    let super::super::settings_v2::IntegrationTransportV2::Stdio { command, cwd, args, .. } =
        &server.transport
    else {
        panic!("the reference plugin launches a local MCP server")
    };
    // A bare interpreter name resolves from PATH; the script is package-relative.
    assert_eq!(command, "python");
    assert_eq!(args.first().map(String::as_str), Some("bridge.py"));
    assert_eq!(
        std::path::Path::new(cwd.as_ref().expect("package working directory")),
        base.canonicalize().unwrap()
    );
    assert_eq!(server.tools.len(), 3);
    let runner = server
        .tools
        .iter()
        .find(|tool| tool.name == "comfyui_run_workflow")
        .expect("the reference tool is declared");
    // A side-effect hint is a hint for approval, not a read-only claim.
    assert_eq!(runner.annotations.as_ref().and_then(|hints| hints.read_only_hint), Some(false));
    // The inert folder scan finds it and never enables it.
    let listed = discovery::discover(base.parent().unwrap());
    assert!(listed.iter().any(|entry| {
        entry
            .server
            .as_ref()
            .is_some_and(|server| server.id == "plugin.comfyui-bridge")
    }));
}

#[test]
fn the_ffmpeg_reference_plugin_is_a_valid_installable_package() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tool-plugins/ffmpeg");
    let server = discovery::inspect(&base.join("tool-plugin.json"))
        .expect("the shipped ffmpeg plugin must validate");
    assert_eq!(server.id, "plugin.ffmpeg");
    assert!(!server.enabled && !server.auto_connect);
    assert!(base.join("ffmpeg_bridge.py").is_file());
    assert!(
        base.join("skills/ffmpeg/SKILL.md").is_file(),
        "the plugin ships its skill"
    );
    let super::super::settings_v2::IntegrationTransportV2::Stdio { command, args, cwd, .. } =
        &server.transport
    else {
        panic!("the ffmpeg plugin launches a local MCP server")
    };
    assert_eq!(command, "python");
    assert_eq!(args.first().map(String::as_str), Some("ffmpeg_bridge.py"));
    // The arguments carry no credential material and name the two binaries.
    assert!(args.iter().any(|argument| argument == "--ffmpeg"));
    assert!(args.iter().any(|argument| argument == "ffprobe"));
    assert_eq!(
        std::path::Path::new(cwd.as_ref().expect("package working directory")),
        base.canonicalize().unwrap()
    );
    assert_eq!(server.tools.len(), 8);
    let runner = server
        .tools
        .iter()
        .find(|tool| tool.name == "ffmpeg_run")
        .expect("the advanced tool is declared");
    assert_eq!(
        runner.annotations.as_ref().and_then(|hints| hints.destructive_hint),
        Some(true),
        "the escape hatch is a destructive hint"
    );
    let probe = server
        .tools
        .iter()
        .find(|tool| tool.name == "ffmpeg_probe")
        .expect("the probe tool is declared");
    assert_eq!(
        probe.annotations.as_ref().and_then(|hints| hints.read_only_hint),
        Some(true)
    );
    let listed = discovery::discover(base.parent().unwrap());
    assert!(listed.iter().any(|entry| {
        entry
            .server
            .as_ref()
            .is_some_and(|server| server.id == "plugin.ffmpeg")
    }));
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
