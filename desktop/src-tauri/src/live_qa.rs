//! Black-box live-model QA mode hosted by the actual Aworkit desktop binary.
//!
//! The mode composes `DesktopRuntime` exactly as the Tauri setup hook does and
//! drives only its public Settings, workflow, Chat, and approval command APIs.
//! It deliberately provides no mock provider or mock tool implementation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime::{
    DesktopRuntime, ModelConfigurationV2, ModelTargetV2, ModelTierResolutionV2,
    ProjectConfigurationV2, ProviderConfigurationV2, RuntimeSnapshot, SettingsV2CommitInput,
    UiCommandInput, WorkflowCommitInput, WorkspaceConfigurationV2, WorkspaceKindV2,
};

const LIVE_QA_FLAG: &str = "--live-model-qa";
const WORKFLOW_ID: &str = "workflow.simple-chat";
const PROJECT_ID: &str = "project.live-qa";
const PROVIDER_ID: &str = "provider.live-qa";
const MODEL_ID: &str = "model.live-qa";

/// Runs the live QA mode when the desktop process receives its explicit flag.
/// Normal graphical launches return `None` and continue through Tauri startup.
pub fn run_from_arguments(arguments: impl Iterator<Item = String>) -> Option<Result<(), String>> {
    let values = arguments.collect::<Vec<_>>();
    if values.first().map(String::as_str) != Some(LIVE_QA_FLAG) {
        return None;
    }
    Some(run(&values[1..]))
}

fn run(arguments: &[String]) -> Result<(), String> {
    let [data_root, project_root, base_url, remote_model] = arguments else {
        return Err(format!(
            "usage: aworkit-desktop {LIVE_QA_FLAG} <data-root> <project-root> <base-url> <model>"
        ));
    };
    let data_root = absolute_directory(data_root, "data root")?;
    let project_root = absolute_directory(project_root, "project root")?;
    let marker = live_marker()?;
    prepare_project(&project_root, &marker)?;

    let mut runtime = DesktopRuntime::open(data_root.join("runtime"))?;
    configure_runtime(&mut runtime, &project_root, base_url, remote_model)?;

    let cases = live_cases(&marker);
    validate_tool_case_coverage(&runtime, &cases)?;
    let mut results = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        let result = run_case(&mut runtime, &project_root, index, case)?;
        println!(
            "LIVE_QA_PASS case={} model={} tool={} approvals={} assistant={}",
            result.case,
            remote_model,
            result.tool.as_deref().unwrap_or("none"),
            result.approvals,
            compact(&result.assistant)
        );
        results.push(result);
    }

    println!(
        "{}",
        serde_json::to_string(&LiveQaSummary {
            schema_version: 1,
            status: "passed",
            application: "aworkit-desktop",
            base_url,
            model: remote_model,
            results: &results,
        })
        .map_err(|error| format!("cannot encode live QA summary: {error}"))?
    );
    Ok(())
}

/// Keeps the live suite coupled to the application's installed built-in tool
/// catalog. A newly installed tool cannot silently escape real-model QA merely
/// because this file's case matrix was not updated at the same time.
fn validate_tool_case_coverage(runtime: &DesktopRuntime, cases: &[LiveCase]) -> Result<(), String> {
    let installed = runtime
        .settings_v2_snapshot()
        .settings
        .tools
        .into_iter()
        .map(|tool| tool.id)
        .collect::<BTreeSet<_>>();
    let covered = cases
        .iter()
        .filter_map(|case| case.tool.map(str::to_owned))
        .collect::<BTreeSet<_>>();
    if installed == covered {
        return Ok(());
    }
    let missing = installed.difference(&covered).cloned().collect::<Vec<_>>();
    let unknown = covered.difference(&installed).cloned().collect::<Vec<_>>();
    Err(format!(
        "live QA tool coverage does not match the installed application catalog; missing cases: {missing:?}; unknown cases: {unknown:?}"
    ))
}

fn absolute_directory(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("live QA {label} must be an absolute path"));
    }
    fs::create_dir_all(&path).map_err(|error| format!("cannot create live QA {label}: {error}"))?;
    fs::canonicalize(path).map_err(|error| format!("cannot resolve live QA {label}: {error}"))
}

fn live_marker() -> Result<String, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(format!("AWORKIT_LIVE_{nanos:X}"))
}

fn prepare_project(root: &Path, marker: &str) -> Result<(), String> {
    for (name, content) in [
        ("qa-read.txt", format!("read marker: {marker}")),
        ("qa-search.txt", "NEEDLE alpha NEEDLE omega".to_owned()),
        ("qa-list-visible.txt", format!("list marker: {marker}")),
        ("qa-grep-visible.txt", format!("grep marker: GREP_{marker}")),
        ("qa-edit.txt", "EDIT_BEFORE".to_owned()),
        ("qa-shell-secret.txt", format!("SHELL_{marker}")),
        ("qa-python-secret.txt", format!("PYTHON_{marker}")),
        ("qa-subagent-secret.txt", format!("SUBAGENT_{marker}")),
    ] {
        fs::write(root.join(name), content)
            .map_err(|error| format!("cannot write live QA fixture {name}: {error}"))?;
    }
    Ok(())
}

fn configure_runtime(
    runtime: &mut DesktopRuntime,
    project_root: &Path,
    base_url: &str,
    remote_model: &str,
) -> Result<(), String> {
    let snapshot = runtime.settings_v2_snapshot();
    let mut settings = snapshot.settings;
    settings.providers = vec![ProviderConfigurationV2 {
        id: PROVIDER_ID.into(),
        name: "Live QA local provider".into(),
        kind: "openai_compatible".into(),
        base_url: base_url.into(),
        enabled: true,
        credential_ref: None,
        models: vec![ModelConfigurationV2 {
            id: MODEL_ID.into(),
            name: remote_model.into(),
            remote_id: remote_model.into(),
            enabled: true,
            context_window: None,
            max_output_tokens: None,
            capabilities: vec!["text".into(), "tools".into()],
            parameters: BTreeMap::new(),
        }],
        configuration: BTreeMap::new(),
    }];
    let target = ModelTargetV2 {
        provider_id: PROVIDER_ID.into(),
        model_id: MODEL_ID.into(),
    };
    for tier in &mut settings.model_tiers {
        tier.resolution = ModelTierResolutionV2::Exact {
            target: target.clone(),
        };
    }
    for tool in &mut settings.tools {
        tool.enabled = true;
    }
    settings.projects = vec![ProjectConfigurationV2 {
        id: PROJECT_ID.into(),
        name: "Live QA project".into(),
        workspace: WorkspaceConfigurationV2 {
            kind: WorkspaceKindV2::LocalDirectory,
            location: project_root.to_string_lossy().into_owned(),
        },
        default_workflow_id: Some(WORKFLOW_ID.into()),
        portable_history_enabled: false,
    }];
    runtime.settings_v2_commit(SettingsV2CommitInput {
        command_id: "qa.live.settings".into(),
        expected_version: snapshot.version,
        settings,
    })?;
    Ok(())
}

#[derive(Clone)]
struct LiveCase {
    name: &'static str,
    tool: Option<&'static str>,
    provider_name: Option<&'static str>,
    arguments: Option<Value>,
    expected_answer_fragment: Option<String>,
    side_effect: SideEffectCheck,
}

#[derive(Clone)]
enum SideEffectCheck {
    None,
    FileEquals { path: &'static str, content: String },
}

fn live_cases(marker: &str) -> Vec<LiveCase> {
    vec![
        LiveCase {
            name: "base-agent-loop",
            tool: None,
            provider_name: None,
            arguments: None,
            expected_answer_fragment: Some("AWORKIT_BASE_LOOP_OK".into()),
            side_effect: SideEffectCheck::None,
        },
        tool_case(
            "file-read",
            "tool.files.read",
            "aworkit_read_project_file",
            json!({"path":"qa-read.txt"}),
            Some(marker.into()),
        ),
        tool_case(
            "file-search",
            "tool.files.search",
            "aworkit_search_project_file",
            json!({"path":"qa-search.txt","query":"NEEDLE"}),
            Some("2".into()),
        ),
        tool_case(
            "file-list",
            "tool.files.list",
            "aworkit_list_project_files",
            json!({"pattern":"qa-list-*.txt"}),
            Some("qa-list-visible.txt".into()),
        ),
        tool_case(
            "file-grep",
            "tool.files.grep",
            "aworkit_grep_project_files",
            json!({"pattern":"GREP_AWORKIT_LIVE_[A-F0-9]+"}),
            Some(format!("GREP_{marker}")),
        ),
        LiveCase {
            name: "file-edit",
            tool: Some("tool.files.edit"),
            provider_name: Some("aworkit_edit_project_file"),
            arguments: Some(json!({
                "path":"qa-edit.txt",
                "old_string":"EDIT_BEFORE",
                "new_string":"EDIT_AFTER"
            })),
            expected_answer_fragment: None,
            side_effect: SideEffectCheck::FileEquals {
                path: "qa-edit.txt",
                content: "EDIT_AFTER".into(),
            },
        },
        LiveCase {
            name: "file-write",
            tool: Some("tool.files.write"),
            provider_name: Some("aworkit_write_project_file"),
            arguments: Some(json!({
                "path":"qa-written.txt",
                "content":format!("WRITTEN_{marker}")
            })),
            expected_answer_fragment: None,
            side_effect: SideEffectCheck::FileEquals {
                path: "qa-written.txt",
                content: format!("WRITTEN_{marker}"),
            },
        },
        tool_case(
            "host-shell",
            "tool.shell.host",
            "aworkit_host_shell",
            json!({"command": if cfg!(windows) { "type qa-shell-secret.txt" } else { "cat qa-shell-secret.txt" }}),
            Some(format!("SHELL_{marker}")),
        ),
        tool_case(
            "host-python",
            "tool.python.host",
            "aworkit_host_python",
            json!({"script":"print(open('qa-python-secret.txt', encoding='utf-8').read())"}),
            Some(format!("PYTHON_{marker}")),
        ),
        tool_case(
            "todo",
            "tool.todo",
            "aworkit_todo",
            json!({"todos":[{"content":format!("TODO_{marker}"),"status":"completed"}]}),
            Some(format!("TODO_{marker}")),
        ),
        tool_case(
            "web-search",
            "tool.web_search",
            "aworkit_web_search",
            json!({"query":"OpenAI official website"}),
            None,
        ),
        tool_case(
            "web-fetch",
            "tool.web_fetch",
            "aworkit_web_fetch",
            json!({"url":"https://example.com"}),
            Some("Example Domain".into()),
        ),
        tool_case(
            "subagent",
            "tool.subagent",
            "aworkit_spawn_subagent",
            json!({"task":"Read qa-subagent-secret.txt and return its exact content."}),
            Some(format!("SUBAGENT_{marker}")),
        ),
    ]
}

fn tool_case(
    name: &'static str,
    tool: &'static str,
    provider_name: &'static str,
    arguments: Value,
    expected_answer_fragment: Option<String>,
) -> LiveCase {
    LiveCase {
        name,
        tool: Some(tool),
        provider_name: Some(provider_name),
        arguments: Some(arguments),
        expected_answer_fragment,
        side_effect: SideEffectCheck::None,
    }
}

fn run_case(
    runtime: &mut DesktopRuntime,
    project_root: &Path,
    index: usize,
    case: &LiveCase,
) -> Result<LiveCaseResult, String> {
    if index > 0 {
        let snapshot = runtime.snapshot(0)?;
        runtime.command(UiCommandInput {
            schema_version: 1,
            command_id: format!("qa.live.new-chat.{index}"),
            expected_version: snapshot.version,
            action: "new_chat".into(),
            target_id: None,
            payload: json!({}),
        })?;
    }

    save_case_workflow(runtime, index, case)?;
    let before = runtime.snapshot(0)?;
    let prompt = case_prompt(case)?;
    runtime.command(UiCommandInput {
        schema_version: 1,
        command_id: format!("qa.live.start.{index}"),
        expected_version: before.version,
        action: "start".into(),
        target_id: None,
        payload: json!({
            "workflowId": WORKFLOW_ID,
            "projectId": PROJECT_ID,
            "input": prompt,
            "attachments": [],
        }),
    })?;

    let mut approvals = 0_u32;
    loop {
        let snapshot = runtime.snapshot(before.through_sequence)?;
        if snapshot.chat.phase != "awaiting_approval" {
            break;
        }
        if approvals >= 8 {
            return Err(format!("case '{}' exceeded the approval bound", case.name));
        }
        let decision_id = latest_pending_decision(&snapshot)?;
        let full = runtime.snapshot(0)?;
        runtime.command(UiCommandInput {
            schema_version: 1,
            command_id: format!("qa.live.approval.{index}.{approvals}"),
            expected_version: full.version,
            action: "approval".into(),
            target_id: None,
            payload: json!({"decisionId":decision_id,"approved":true}),
        })?;
        approvals = approvals.saturating_add(1);
    }

    let snapshot = runtime.snapshot(before.through_sequence)?;
    if snapshot.chat.phase == "failed" {
        return Err(case_failure(case, &snapshot));
    }
    let assistant = last_assistant(&snapshot)?;
    if let Some(expected) = &case.expected_answer_fragment
        && !assistant.contains(expected)
    {
        return Err(format!(
            "case '{}' assistant did not contain live result fragment '{}': {}",
            case.name, expected, assistant
        ));
    }
    if let Some(tool) = case.tool
        && !has_completed_tool_event(&snapshot, tool)
    {
        return Err(format!(
            "case '{}' produced an assistant response without a completed '{}' event",
            case.name, tool
        ));
    }
    verify_side_effect(project_root, &case.side_effect)?;
    Ok(LiveCaseResult {
        case: case.name,
        tool: case.tool.map(str::to_owned),
        approvals,
        assistant,
    })
}

fn save_case_workflow(
    runtime: &mut DesktopRuntime,
    index: usize,
    case: &LiveCase,
) -> Result<(), String> {
    let snapshot = runtime.workflow_snapshot_for(WORKFLOW_ID.into());
    let tools = case.tool.map_or_else(Vec::new, |tool| vec![tool]);
    let instructions = case_instructions(case)?;
    let document = json!({
        "schemaVersion":1,
        "id":WORKFLOW_ID,
        "name":format!("Live QA: {}", case.name),
        "nodes":[
            {"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}},
            {"id":"agent.1","label":"Agent","type":"agent","position":{"x":245,"y":205},"configuration":{
                "modelTierId":"tier:balanced",
                "toolIds":tools,
                "instructions":instructions
            }},
            {"id":"output.1","label":"Output","type":"output","position":{"x":470,"y":205}},
            {"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":695,"y":205}}
        ],
        "edges":[
            {"id":"input-agent","source":"input.1","target":"agent.1"},
            {"id":"agent-output","source":"agent.1","target":"output.1"},
            {"id":"output-wait","source":"output.1","target":"wait.1"}
        ],
        "comments":"Live-model QA workflow executed by the actual Aworkit desktop binary."
    });
    runtime.workflow_commit(WorkflowCommitInput {
        command_id: format!("qa.live.workflow.{index}"),
        expected_version: snapshot.version,
        document,
        workflow_id: Some(WORKFLOW_ID.into()),
    })?;
    Ok(())
}

fn case_instructions(case: &LiveCase) -> Result<String, String> {
    let Some(tool) = case.tool else {
        return Ok("Reply with exactly AWORKIT_BASE_LOOP_OK.".into());
    };
    let provider_name = case
        .provider_name
        .ok_or_else(|| format!("case '{}' has no provider tool name", case.name))?;
    let arguments = case
        .arguments
        .as_ref()
        .ok_or_else(|| format!("case '{}' has no tool arguments", case.name))?;
    Ok(format!(
        "This is a live application QA run. You MUST call the only available tool, {provider_name} ({tool}), exactly once with exactly these JSON arguments: {}. Do not answer before receiving the tool result. After the result, report the actual result concisely. Never invent success.",
        serde_json::to_string(arguments)
            .map_err(|error| format!("cannot encode case arguments: {error}"))?
    ))
}

fn case_prompt(case: &LiveCase) -> Result<String, String> {
    if case.tool.is_none() {
        return Ok("Prove the base agent loop is live now.".into());
    }
    Ok(format!(
        "Execute live QA case '{}'. Follow the system instruction and use the tool now.",
        case.name
    ))
}

fn latest_pending_decision(snapshot: &RuntimeSnapshot) -> Result<String, String> {
    snapshot
        .events
        .iter()
        .rev()
        .find(|event| event.kind == "approval.requested")
        .and_then(|event| event.payload.get("decisionId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Chat is awaiting approval without a decision identity".into())
}

fn has_completed_tool_event(snapshot: &RuntimeSnapshot, tool: &str) -> bool {
    snapshot.events.iter().any(|event| {
        matches!(
            event.kind.as_str(),
            "span.completed" | "tool.completed" | "subagent.completed"
        ) && event.payload.get("capabilityId").and_then(Value::as_str) == Some(tool)
    })
}

fn last_assistant(snapshot: &RuntimeSnapshot) -> Result<String, String> {
    snapshot
        .events
        .iter()
        .rev()
        .find(|event| event.kind == "message.assistant")
        .and_then(|event| event.payload.get("body"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "live QA Chat has no final assistant response".into())
}

fn verify_side_effect(project_root: &Path, check: &SideEffectCheck) -> Result<(), String> {
    match check {
        SideEffectCheck::None => Ok(()),
        SideEffectCheck::FileEquals { path, content } => {
            let observed = fs::read_to_string(project_root.join(path))
                .map_err(|error| format!("cannot inspect live QA side effect {path}: {error}"))?;
            if observed == *content {
                Ok(())
            } else {
                Err(format!(
                    "live QA side effect {path} has content '{observed}', expected '{content}'"
                ))
            }
        }
    }
}

fn case_failure(case: &LiveCase, snapshot: &RuntimeSnapshot) -> String {
    let detail = snapshot
        .events
        .iter()
        .rev()
        .find(|event| matches!(event.kind.as_str(), "execution.failed" | "span.failed"))
        .and_then(|event| event.payload.get("body"))
        .and_then(Value::as_str)
        .unwrap_or("no execution error was projected");
    format!(
        "case '{}' failed in the actual application: {detail}",
        case.name
    )
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveCaseResult {
    case: &'static str,
    tool: Option<String>,
    approvals: u32,
    assistant: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveQaSummary<'a> {
    schema_version: u16,
    status: &'static str,
    application: &'static str,
    base_url: &'a str,
    model: &'a str,
    results: &'a [LiveCaseResult],
}
