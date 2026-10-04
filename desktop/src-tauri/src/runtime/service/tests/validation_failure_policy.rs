//! Phase P1 of task 184: validation must not be able to kill a message.
//!
//! Every test here reproduces one run-path refusal that used to end a message,
//! and pins the ordered outcome the failure policy requires instead: a
//! committed notice, a dropped binding, the frozen snapshot, or — where a human
//! must act — one refusal that names the fix. The three conditions that may end
//! an Agent node (the model's answer, user cancellation, an unrecoverable
//! refusal) are untouched by this phase.
use super::*;

/// A saved document whose Agent node carries a node type this build has no
/// executor for. The editor preserves it losslessly and the catalog refuses it
/// at start, which is the trap P1.6 reconciles.
fn document_with_future_node(runtime: &mut DesktopRuntime, workflow_id: &str) -> Value {
    let mut document = runtime.workflow_snapshot_for(workflow_id.to_owned()).document;
    document["nodes"][1]["type"] = json!("future_node");
    document
}

fn comfyui_workflow_binding(runtime: &mut DesktopRuntime) -> String {
    let mut settings = runtime.settings_v2_snapshot().settings;
    settings
        .comfyui
        .workflow_tools
        .push(ComfyUiWorkflowToolV2 {
            id: "krea".into(),
            name: "Krea".into(),
            description: "Generates an image".into(),
            workflow_path: "D:/workflows/krea.json".into(),
            enabled: true,
            parameters: Vec::new(),
        });
    runtime
        .settings_v2_commit(SettingsV2CommitInput {
            command_id: "settings.p1.comfyui".into(),
            expected_version: runtime.settings_v2_snapshot().version,
            settings,
        })
        .unwrap();
    let created = runtime
        .workflow_duplicate(WorkflowDuplicateInput {
            command_id: "workflow.p1.comfyui.create".into(),
            workflow_id: "workflow.simple-chat".into(),
            name: "ComfyUI binding workflow".into(),
        })
        .unwrap();
    let mut document = runtime.workflow_snapshot_for(created.workflow_id.clone()).document;
    document["nodes"][1]["configuration"]["toolIds"] = json!(["comfyui.krea"]);
    runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.p1.comfyui.save".into(),
            expected_version: created.current_version,
            document,
            workflow_id: Some(created.workflow_id.clone()),
        })
        .unwrap();
    created.workflow_id
}

#[test]
fn a_mid_chat_edit_of_the_saved_workflow_does_not_block_the_chat() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut runtime = runtime(&root, provider.clone());
    configure(&mut runtime);
    let created = runtime
        .workflow_duplicate(WorkflowDuplicateInput {
            command_id: "workflow.mid-edit.create".into(),
            workflow_id: "workflow.simple-chat".into(),
            name: "Mid-edit workflow".into(),
        })
        .unwrap();
    runtime
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "chat.mid-edit.start".into(),
            expected_version: 0,
            action: "start".into(),
            target_id: None,
            payload: json!({
                "workflowId": created.workflow_id.clone(),
                "input": "hello",
                "attachments": [],
            }),
        })
        .unwrap();
    let frozen_snapshot = provider.execution_requests.lock().unwrap()[0]
        .workflow_snapshot
        .clone();

    // The user edits the saved workflow mid-Chat into a state the catalog
    // refuses at start.
    let document = document_with_future_node(&mut runtime, &created.workflow_id);
    let receipt = runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.mid-edit.save".into(),
            expected_version: created.current_version,
            document,
            workflow_id: Some(created.workflow_id.clone()),
        })
        .unwrap();
    assert!(receipt.accepted, "preservation on save is a design invariant");

    // The next message of the running Chat still runs, on the frozen graph, and
    // says why the saved entry was not used.
    runtime
        .command(send(
            "chat.mid-edit.follow-up",
            runtime.snapshot(0).unwrap().version,
            "again",
        ))
        .unwrap();
    let requests = provider.execution_requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the follow-up ran");
    let mut expected = frozen_snapshot;
    expected["capabilityWarnings"] = json!([format!(
        "saved workflow '{}' is not executable in this build; this pass runs the graph the Chat froze",
        created.workflow_id
    )]);
    assert_eq!(requests[1].workflow_snapshot, expected);
    assert_eq!(
        runtime.snapshot(0).unwrap().chat.workflow_id.as_deref(),
        Some(created.workflow_id.as_str())
    );
}

#[test]
fn a_broken_default_workflow_does_not_disable_send_for_the_selected_workflow() {
    let root = TempDir::new().unwrap();
    let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut runtime);
    let broken = runtime
        .workflow_duplicate(WorkflowDuplicateInput {
            command_id: "workflow.broken-default.create".into(),
            workflow_id: "workflow.simple-chat".into(),
            name: "Broken default".into(),
        })
        .unwrap();
    let document = document_with_future_node(&mut runtime, &broken.workflow_id);
    runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.broken-default.save".into(),
            expected_version: broken.current_version,
            document,
            workflow_id: Some(broken.workflow_id.clone()),
        })
        .unwrap();
    runtime
        .documents
        .set_default_workflow(&broken.workflow_id)
        .unwrap();

    // The workflow the user selected is ready, so Send is not disabled.
    let selected = runtime
        .workflow_snapshot_for("workflow.simple-chat".into())
        .execution_verdict
        .expect("the core answers per workflow");
    assert!(selected.executable, "{selected:?}");
    // The unrelated broken default only reports its own verdict.
    let default = runtime
        .workflow_snapshot_for(broken.workflow_id.clone())
        .execution_verdict
        .unwrap();
    assert!(!default.executable);
    assert!(
        default
            .rule
            .as_deref()
            .is_some_and(|rule| rule.contains("future_node")),
        "{default:?}"
    );
    assert!(default.remedy.is_some(), "a refusal names the fix: {default:?}");
    // The draft projection carries no library-default verdict that could
    // disable the composer's Send.
    assert_eq!(runtime.snapshot(0).unwrap().chat.disabled_reason, None);
}

#[test]
fn a_stored_binding_shape_mismatch_degrades_to_a_notice() {
    // The binding stays complete evidence in the stored record, but a shape
    // this build cannot dispatch is dropped with a notice instead of ending the
    // message. The later-pass path is `freeze_current_tools`, which decides
    // which frozen bindings this build still executes.
    let root = TempDir::new().unwrap();
    let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut runtime);
    let workflow_id = comfyui_workflow_binding(&mut runtime);
    let command = UiCommandInput {
        schema_version: 1,
        command_id: "chat.p1.shape".into(),
        expected_version: 0,
        action: "start".into(),
        target_id: None,
        payload: json!({
            "workflowId": workflow_id.clone(),
            "input": "make an image",
            "attachments": [],
        }),
    };
    let fingerprint = command_fingerprint(&command).unwrap();
    let prepared = runtime
        .prepare_workflow_context(&command, &command.command_id, &fingerprint, 0, None)
        .unwrap();
    assert_eq!(prepared.context.tools.len(), 1);
    assert!(
        crate::runtime::history::frozen_tool_binding_is_executable(
            &prepared.context.tools[0]
        )
    );

    // A record another build wrote: the binding is complete evidence, but its
    // frozen configuration is not the shape this build dispatches.
    let mut binding = prepared.context.tools[0].clone();
    binding.tool_snapshot.configuration.remove("parameters");
    binding.tool_hash = canonical_hash(&binding.tool_snapshot).unwrap();

    let workflow = runtime.workflow_snapshot_for(workflow_id).document;
    let settings = runtime.settings_v2_snapshot().settings;
    let (tools, warnings) = super::super::current_configuration::freeze_current_tools(
        &workflow,
        &settings,
        &[binding],
    );
    assert!(tools.is_empty(), "the unexecutable binding is dropped: {tools:?}");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("cannot execute")),
        "{warnings:?}"
    );
}

#[test]
fn a_stored_binding_hash_mismatch_is_still_rejected() {
    // Negative control for the split above: shape degrades, tampering does not.
    let root = TempDir::new().unwrap();
    let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut runtime);
    let workflow_id = comfyui_workflow_binding(&mut runtime);
    let command = UiCommandInput {
        schema_version: 1,
        command_id: "chat.p1.tamper".into(),
        expected_version: 0,
        action: "start".into(),
        target_id: None,
        payload: json!({
            "workflowId": workflow_id,
            "input": "make an image",
            "attachments": [],
        }),
    };
    let fingerprint = command_fingerprint(&command).unwrap();
    let mut prepared = runtime
        .prepare_workflow_context(&command, &command.command_id, &fingerprint, 0, None)
        .unwrap();
    prepared.context.tools[0].tool_hash = format!("sha256:{}", "b".repeat(64));
    prepared.context_hash = canonical_hash(&prepared.context).unwrap();
    runtime
        .history
        .freeze_context(prepared.context.clone())
        .unwrap();
    let error = runtime.command(command).unwrap_err();
    assert!(error.contains("integrity validation"), "{error}");
}

#[test]
fn a_workflow_without_a_model_consuming_node_is_refused_with_the_fix() {
    let root = TempDir::new().unwrap();
    let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut runtime);
    let created = runtime
        .workflow_duplicate(WorkflowDuplicateInput {
            command_id: "workflow.no-model.create".into(),
            workflow_id: "workflow.simple-chat".into(),
            name: "No model workflow".into(),
        })
        .unwrap();
    let mut document = runtime.workflow_snapshot_for(created.workflow_id.clone()).document;
    document["nodes"] = json!([
        {"id": "input.1", "type": "input", "label": "Input", "position": {"x": 36, "y": 205}},
        {"id": "completion.1", "type": "completion", "label": "Done", "position": {"x": 245, "y": 205}}
    ]);
    document["edges"] = json!([
        {"id": "input-completion", "source": "input.1", "target": "completion.1"}
    ]);
    runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.no-model.save".into(),
            expected_version: created.current_version,
            document,
            workflow_id: Some(created.workflow_id.clone()),
        })
        .unwrap();

    // This used to be a panic on the start path (`.expect("at least one model
    // tier is resolved")`); it is a typed refusal that names the fix.
    let error = runtime
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "chat.no-model.start".into(),
            expected_version: 0,
            action: "start".into(),
            target_id: None,
            payload: json!({
                "workflowId": created.workflow_id.clone(),
                "input": "hello",
                "attachments": [],
            }),
        })
        .unwrap_err();
    assert!(error.contains("no model-consuming node"), "{error}");
    assert!(
        error.contains("Agent or Model Call node"),
        "the refusal names the fix: {error}"
    );
    let verdict = runtime
        .workflow_snapshot_for(created.workflow_id)
        .execution_verdict
        .unwrap();
    assert!(!verdict.executable);
    assert!(
        verdict
            .rule
            .as_deref()
            .is_some_and(|rule| rule.contains("no model-consuming node")),
        "{verdict:?}"
    );
}

#[test]
fn saving_reconciles_with_execution_without_refusing_the_save() {
    let root = TempDir::new().unwrap();
    let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
    configure(&mut runtime);
    let created = runtime
        .workflow_duplicate(WorkflowDuplicateInput {
            command_id: "workflow.preserved.create".into(),
            workflow_id: "workflow.simple-chat".into(),
            name: "Preserved workflow".into(),
        })
        .unwrap();
    let mut document = runtime.workflow_snapshot_for(created.workflow_id.clone()).document;
    document["nodes"][1]["type"] = json!("future_node");
    document["nodes"][1]["configuration"]["futureOption"] = json!(true);
    let receipt = runtime
        .workflow_commit(WorkflowCommitInput {
            command_id: "workflow.preserved.save".into(),
            expected_version: created.current_version,
            document: document.clone(),
            workflow_id: Some(created.workflow_id.clone()),
        })
        .unwrap();
    assert!(receipt.accepted, "preservation on save is a design invariant");
    assert_eq!(receipt.current_version, created.current_version + 1);

    // The unknown node type and its unknown configuration survive losslessly.
    let stored = runtime.workflow_snapshot_for(created.workflow_id.clone()).document;
    assert_eq!(stored["nodes"][1]["type"], json!("future_node"));
    assert_eq!(stored["nodes"][1]["configuration"]["futureOption"], json!(true));

    // The core verdict is surfaced at save time and on open, and names the fix.
    let reason = receipt.reason.expect("the save states the core verdict");
    assert!(reason.contains("future_node"), "{reason}");
    assert!(reason.contains("editor"), "the verdict names the fix: {reason}");
    let verdict = runtime
        .workflow_snapshot_for(created.workflow_id)
        .execution_verdict
        .unwrap();
    assert!(!verdict.executable);
    assert!(
        verdict
            .rule
            .as_deref()
            .is_some_and(|rule| rule.contains("future_node")),
        "{verdict:?}"
    );
    assert!(verdict.remedy.is_some());
}
