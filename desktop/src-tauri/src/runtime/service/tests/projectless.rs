//! Service-level regressions for private Chat workspace freezing and isolation.
use super::*;

#[test]
fn projectless_file_workflow_keeps_its_folder_after_restart_and_isolates_new_chats() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider.clone());
    configure_project_read_workflow(&mut desktop, None, true);
    desktop
        .workflow_set_default(WorkflowTargetInput {
            command_id: "private.default".into(),
            workflow_id: "workflow.simple-chat".into(),
        })
        .unwrap();
    assert!(desktop.snapshot(0).unwrap().chat.disabled_reason.is_none());
    desktop.command(send("private.start", 0, "hello")).unwrap();
    let frozen = desktop.history.current_frozen_context().unwrap().unwrap();
    let folder = frozen.context.chat_workspace.clone().unwrap();
    assert!(frozen.context.project.is_none());
    assert_eq!(frozen.context.tools[0].tool_id, "tool.files.read");
    assert!(
        folder
            .root
            .ends_with(frozen.context.identity.chat_id.as_str())
    );
    fs::write(folder.root.join("notes.txt"), "retained content").unwrap();
    assert_eq!(desktop.snapshot(0).unwrap().chat.scope, "No project");
    assert!(desktop.settings_v2_snapshot().settings.projects.is_empty());
    drop(desktop);

    let mut desktop = runtime(&root, provider.clone());
    let restored = desktop.history.current_frozen_context().unwrap().unwrap();
    assert_eq!(restored, frozen);
    desktop
        .command(send(
            "private.follow-up",
            desktop.history.head().unwrap(),
            "read notes.txt",
        ))
        .unwrap();
    let requests = provider.execution_requests.lock().unwrap();
    assert_eq!(requests[0].workspace.as_ref(), Some(&folder));
    assert_eq!(requests[1].workspace.as_ref(), Some(&folder));
    assert!(requests[1].approvals.project_key.is_none());
    assert_eq!(
        fs::read_to_string(folder.root.join("notes.txt")).unwrap(),
        "retained content"
    );
    drop(requests);

    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "private.fork".into(),
            expected_version: desktop.history.head().unwrap(),
            action: "fork".into(),
            target_id: Some(frozen.context.identity.chat_id.to_string()),
            payload: json!({}),
        })
        .unwrap();
    let child = desktop.history.current_frozen_context().unwrap().unwrap();
    let child_folder = child.context.chat_workspace.unwrap();
    assert_ne!(child_folder, folder);
    assert!(!child_folder.root.join("notes.txt").exists());

    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "private.new".into(),
            expected_version: desktop.history.head().unwrap(),
            action: "new_chat".into(),
            target_id: None,
            payload: json!({}),
        })
        .unwrap();
    let mut start = send("private.second", 0, "another chat");
    start.expected_version = desktop.history.head().unwrap();
    desktop.command(start).unwrap();
    let second = desktop.history.current_frozen_context().unwrap().unwrap();
    let second_folder = second.context.chat_workspace.unwrap();
    assert_ne!(second_folder, folder);
    assert_ne!(second_folder, child_folder);
    assert!(!second_folder.root.join("notes.txt").exists());
}

#[test]
fn every_bundled_executable_workflow_starts_with_no_saved_projects() {
    let catalog: Value = serde_json::from_str(include_str!(
        "../../../../../workflows/default-workflows.json"
    ))
    .unwrap();
    for entry in catalog["workflows"].as_array().unwrap() {
        if entry["seedOnFreshProfile"] != true {
            continue;
        }
        let root = TempDir::new().unwrap();
        let mut desktop = runtime(&root, Arc::new(FixtureProvider::new()));
        configure_project_read_workflow(&mut desktop, None, true);
        let mut settings = desktop.settings_v2_snapshot();
        for tool in &mut settings.settings.tools {
            if entry["document"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| {
                    node["configuration"]["toolIds"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&json!(tool.id)))
                })
            {
                tool.enabled = true;
            }
        }
        desktop
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "private.settings".into(),
                expected_version: settings.version,
                settings: settings.settings,
            })
            .unwrap();
        desktop
            .workflow_set_default(WorkflowTargetInput {
                command_id: "private.default".into(),
                workflow_id: entry["document"]["id"].as_str().unwrap().into(),
            })
            .unwrap();
        assert!(desktop.snapshot(0).unwrap().chat.disabled_reason.is_none());
        let mut start = send("private.bundled", 0, "hello");
        start.payload["workflowId"] = entry["document"]["id"].clone();
        desktop.command(start).unwrap();
        let context = desktop
            .history
            .current_frozen_context()
            .unwrap()
            .unwrap()
            .context;
        assert!(context.project.is_none());
        assert!(context.chat_workspace.is_some());
    }
}

#[test]
fn saved_project_does_not_get_a_private_folder() {
    let root = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let mut desktop = runtime(&root, Arc::new(FixtureProvider::new()));
    configure_project_read_workflow(&mut desktop, Some(workspace.path()), true);
    desktop
        .command(project_tool_start("private.project", "hello"))
        .unwrap();
    let context = desktop
        .history
        .current_frozen_context()
        .unwrap()
        .unwrap()
        .context;
    assert!(context.project.is_some());
    assert!(context.chat_workspace.is_none());
}
