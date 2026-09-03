//! Native, side-effect-bounded diagnostics for Settings drafts.
//!
//! Diagnostics operate on the exact unsaved records supplied by the renderer.
//! They do not persist, enable, or grant authority to a capability.

use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

use aworkit_capability_host::{
    BuiltInProcessTools, CancellationToken, FileAuthority, HostToolLimitsV1, NativeProcessPort,
    PlatformProcessPort, ProjectFiles, PythonInvocationV1, ShellInvocationV1, ToolAuthorityModeV1,
    WebSearchConfigurationV1, WebTools,
};
use serde::{Deserialize, Serialize};

use super::{
    project_scope::resolve_git_branch,
    settings_v2::{BuiltInToolConfigurationV2, ProjectConfigurationV2, WorkspaceKindV2},
};

const MAXIMUM_TOOL_TIMEOUT_SECONDS: u64 = 300;
const MAXIMUM_TOOL_OUTPUT_BYTES: u64 = 262_144;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectProbeRequestV2 {
    pub project: ProjectConfigurationV2,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectProbeResultV2 {
    pub ok: bool,
    pub project_id: String,
    pub workspace_kind: WorkspaceKindV2,
    pub resolved_location: Option<String>,
    pub message: String,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolProbeRequestV2 {
    pub tool: BuiltInToolConfigurationV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectConfigurationV2>,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolProbeResultV2 {
    pub ok: bool,
    pub tool_id: String,
    pub adapter: String,
    pub message: String,
    pub draft_fingerprint: String,
}

pub(crate) fn probe_project(request: ProjectProbeRequestV2) -> ProjectProbeResultV2 {
    let project_id = request.project.id.clone();
    let workspace_kind = request.project.workspace.kind;
    let result = inspect_project(&request.project);
    match result {
        Ok(path) => ProjectProbeResultV2 {
            ok: true,
            project_id,
            workspace_kind,
            resolved_location: Some(path.to_string_lossy().into_owned()),
            message: "Workspace exists and its native root was resolved without granting workflow authority."
                .into(),
            draft_fingerprint: request.draft_fingerprint,
        },
        Err(message) => ProjectProbeResultV2 {
            ok: false,
            project_id,
            workspace_kind,
            resolved_location: None,
            message,
            draft_fingerprint: request.draft_fingerprint,
        },
    }
}

#[cfg(test)]
pub(crate) fn probe_tool(request: ToolProbeRequestV2) -> ToolProbeResultV2 {
    probe_tool_with_api_key(request, None)
}

pub(crate) fn probe_tool_with_api_key(
    request: ToolProbeRequestV2,
    api_key: Option<&str>,
) -> ToolProbeResultV2 {
    let tool_id = request.tool.id.clone();
    let bindings_supported = request.tool.credential_bindings.is_empty()
        || (tool_id == "tool.web_search"
            && request.tool.credential_bindings.len() == 1
            && request.tool.credential_bindings[0].name == "api_key");
    let outcome = if bindings_supported {
        match tool_id.as_str() {
            "tool.files.read" | "tool.files.search" | "tool.files.list" | "tool.files.grep"
            | "tool.files.edit" | "tool.files.write" => {
                probe_project_file_tool(&request.tool, request.project.as_ref())
            }
            "tool.shell.host" => probe_host_shell(&request.tool),
            "tool.python.host" => probe_host_python(&request.tool),
            "tool.todo" => probe_todo_tool(&request.tool),
            "tool.subagent" => probe_subagent_tool(&request.tool),
            "tool.web_search" => probe_web_search_tool(&request.tool, api_key),
            "tool.web_fetch" | "tool.web_extract" => probe_web_fetch_tool(&request.tool),
            _ => Err(format!(
                "No native built-in adapter is installed for '{}'.",
                request.tool.id
            )),
        }
    } else {
        Err("This built-in adapter does not consume the configured credential bindings.".into())
    };
    match outcome {
        Ok((adapter, message)) => ToolProbeResultV2 {
            ok: true,
            tool_id,
            adapter,
            message,
            draft_fingerprint: request.draft_fingerprint,
        },
        Err(message) => ToolProbeResultV2 {
            ok: false,
            tool_id,
            adapter: "unavailable".into(),
            message,
            draft_fingerprint: request.draft_fingerprint,
        },
    }
}

fn inspect_project(project: &ProjectConfigurationV2) -> Result<PathBuf, String> {
    if project.workspace.location.trim().is_empty() {
        return Err("Workspace location is empty.".into());
    }
    if project.workspace.kind == WorkspaceKindV2::Remote {
        return Err(
            "Remote workspaces require a configured remote-workspace adapter; none is installed."
                .into(),
        );
    }
    let path = fs::canonicalize(&project.workspace.location)
        .map_err(|_| "Workspace path does not exist or cannot be resolved.".to_owned())?;
    if !path.is_dir() {
        return Err("Workspace path is not a directory.".into());
    }
    if project.workspace.kind == WorkspaceKindV2::GitWorktree {
        resolve_git_branch(&path).map_err(|message| {
            format!("Selected Git worktree has invalid HEAD identity: {message}")
        })?;
    }
    Ok(path)
}

fn probe_project_file_tool(
    tool: &BuiltInToolConfigurationV2,
    project: Option<&ProjectConfigurationV2>,
) -> Result<(String, String), String> {
    let project = project.ok_or_else(|| {
        "Select an exact project draft before testing a project-files capability.".to_owned()
    })?;
    let root = inspect_project(project)?;
    ProjectFiles::new(FileAuthority {
        root,
        allow_write: matches!(tool.id.as_str(), "tool.files.edit" | "tool.files.write"),
    })
    .map_err(|error| format!("Project-files capability could not open the root: {error}"))?;
    Ok((
        "cap-std-project-files".into(),
        format!(
            "{} can open the selected project as a root-confined directory capability.",
            tool.name
        ),
    ))
}

/// The run-local task list needs no external adapter; it stores ordered
/// snapshots inside the Run record.
fn probe_todo_tool(tool: &BuiltInToolConfigurationV2) -> Result<(String, String), String> {
    Ok((
        "run-local-todo".into(),
        format!(
            "{} uses the built-in run-local task list; no external adapter is required.",
            tool.name
        ),
    ))
}

/// The subagent tool runs bounded child loops over the frozen model gateway
/// already resolved by the Run; no external adapter exists to probe.
fn probe_subagent_tool(tool: &BuiltInToolConfigurationV2) -> Result<(String, String), String> {
    Ok((
        "run-local-subagent".into(),
        format!(
            "{} executes bounded child loops on the frozen model gateway; no external adapter is required.",
            tool.name
        ),
    ))
}

/// The web adapters perform a live bounded HTTPS round trip so the probe
/// reports real connectivity instead of a static capability claim.
fn probe_web_search_tool(
    tool: &BuiltInToolConfigurationV2,
    api_key: Option<&str>,
) -> Result<(String, String), String> {
    let configuration = serde_json::from_value::<WebSearchConfigurationV1>(
        serde_json::to_value(&tool.configuration)
            .map_err(|error| format!("Cannot encode web-search draft: {error}"))?,
    )
    .map_err(|error| format!("Web-search draft does not match adapter v2: {error}"))?;
    let outcome = WebTools::production()
        .search_configured_v1(
            "Aworkit web search connectivity test",
            &configuration,
            api_key,
            &CancellationToken::default(),
        )
        .map_err(|error| format!("Bounded web search failed: {error}"))?;
    Ok((
        format!("web-search-{}", outcome.backend),
        format!(
            "{} completed a bounded live search via {} and returned {} result(s).",
            tool.name,
            outcome.backend,
            outcome.results.len()
        ),
    ))
}

fn probe_web_fetch_tool(tool: &BuiltInToolConfigurationV2) -> Result<(String, String), String> {
    let fetched = WebTools::production()
        .fetch_v1(
            "https://example.com/",
            4096,
            1024,
            &CancellationToken::default(),
        )
        .map_err(|error| format!("Bounded web round trip failed: {error}"))?;
    Ok((
        "https-web-tools".into(),
        format!(
            "{} completed a bounded HTTPS round trip ({} byte(s) downloaded).",
            tool.name, fetched.bytes_downloaded
        ),
    ))
}

fn probe_host_shell(tool: &BuiltInToolConfigurationV2) -> Result<(String, String), String> {
    let platform = NativeProcessPort;
    let health = platform
        .health()
        .map_err(|error| format!("Native process adapter health check failed: {error}"))?;
    if !health.available || !health.process_tree_cleanup {
        return Err("Native process adapter cannot guarantee process-tree cleanup.".into());
    }
    let shell = native_shell().ok_or_else(|| "No supported host shell was found.".to_owned())?;
    let tools = BuiltInProcessTools::new(platform);
    let result = tools
        .execute_shell(
            &ShellInvocationV1 {
                mode: ToolAuthorityModeV1::HostShell,
                shell_program: shell,
                command_text: shell_noop().into(),
                working_directory: None,
                environment: BTreeMap::new(),
                limits: tool_limits(tool)?,
            },
            &CancellationToken::default(),
        )
        .map_err(|error| format!("Bounded host-shell probe failed: {error}"))?;
    if result.status != Some(0) {
        return Err("Bounded host-shell probe returned a non-zero status.".into());
    }
    Ok((
        health.adapter,
        "Host shell started through the bounded native process-group adapter and exited successfully."
            .into(),
    ))
}

fn probe_host_python(tool: &BuiltInToolConfigurationV2) -> Result<(String, String), String> {
    let platform = NativeProcessPort;
    let health = platform
        .health()
        .map_err(|error| format!("Native process adapter health check failed: {error}"))?;
    if !health.available || !health.process_tree_cleanup {
        return Err("Native process adapter cannot guarantee process-tree cleanup.".into());
    }
    let interpreter = find_executable(python_names())
        .ok_or_else(|| "No Python interpreter was found on PATH.".to_owned())?;
    let tools = BuiltInProcessTools::new(platform);
    let result = tools
        .execute_python(
            &PythonInvocationV1 {
                mode: ToolAuthorityModeV1::HostPython,
                interpreter,
                script: "import sys; sys.stdout.write('aworkit-python-ok')".into(),
                arguments: Vec::new(),
                working_directory: None,
                environment: BTreeMap::new(),
                limits: tool_limits(tool)?,
            },
            &CancellationToken::default(),
        )
        .map_err(|error| format!("Bounded host-Python probe failed: {error}"))?;
    if result.status != Some(0) || result.stdout != b"aworkit-python-ok" {
        return Err("Bounded host-Python probe did not return the expected result.".into());
    }
    Ok((
        health.adapter,
        "Python started in isolated interpreter mode through the bounded native process-group adapter."
            .into(),
    ))
}

fn tool_limits(tool: &BuiltInToolConfigurationV2) -> Result<HostToolLimitsV1, String> {
    let timeout_seconds = configured_u64(tool, "timeoutSeconds")?;
    let maximum_output_bytes = configured_u64(tool, "maximumOutputBytes")?;
    if timeout_seconds == 0
        || timeout_seconds > MAXIMUM_TOOL_TIMEOUT_SECONDS
        || maximum_output_bytes == 0
        || maximum_output_bytes > MAXIMUM_TOOL_OUTPUT_BYTES
    {
        return Err("Tool timeout or output limit is outside the native adapter bounds.".into());
    }
    Ok(HostToolLimitsV1 {
        timeout: Duration::from_secs(timeout_seconds),
        maximum_output_bytes: usize::try_from(maximum_output_bytes)
            .map_err(|_| "Tool output limit does not fit this platform.".to_owned())?,
        cancellation_grace: Duration::from_millis(100),
    })
}

fn configured_u64(tool: &BuiltInToolConfigurationV2, field: &str) -> Result<u64, String> {
    tool.configuration
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("{} has no valid {field} configuration.", tool.name))
}

#[cfg(unix)]
fn native_shell() -> Option<PathBuf> {
    let shell = PathBuf::from("/bin/sh");
    shell.is_file().then_some(shell)
}

#[cfg(windows)]
fn native_shell() -> Option<PathBuf> {
    find_executable(&["cmd.exe"])
}

#[cfg(unix)]
fn shell_noop() -> &'static str {
    ":"
}

#[cfg(windows)]
fn shell_noop() -> &'static str {
    "exit /b 0"
}

#[cfg(windows)]
fn python_names() -> &'static [&'static str] {
    &["python.exe", "python3.exe"]
}

#[cfg(not(windows))]
fn python_names() -> &'static [&'static str] {
    &["python3", "python"]
}

fn find_executable(names: &[&str]) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| fs::canonicalize(candidate).ok())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use serde_json::json;

    use super::*;
    use crate::runtime::settings_v2::{WorkspaceConfigurationV2, WorkspaceKindV2};

    fn project(path: &std::path::Path) -> ProjectConfigurationV2 {
        ProjectConfigurationV2 {
            id: "project.test".into(),
            name: "Test".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::LocalDirectory,
                location: path.to_string_lossy().into_owned(),
            },
            default_workflow_id: Some("workflow.simple-chat".into()),
            portable_history_enabled: false,
        }
    }

    fn file_tool(id: &str) -> BuiltInToolConfigurationV2 {
        BuiltInToolConfigurationV2 {
            id: id.into(),
            name: "Project file".into(),
            enabled: true,
            requires_project: true,
            credential_bindings: Vec::new(),
            configuration: BTreeMap::from([
                ("authorityMode".into(), json!("project_files")),
                ("effect".into(), json!("read")),
                (
                    "maximumBytes".into(),
                    json!(crate::runtime::PROJECT_FILE_READ_MAXIMUM_BYTES_V1),
                ),
            ]),
        }
    }

    #[test]
    fn project_and_file_tool_probe_open_the_exact_root_without_mutating_it() {
        let temporary = tempfile::tempdir().expect("temporary root");
        fs::write(temporary.path().join("keep.txt"), "unchanged").expect("fixture file");
        let project = project(temporary.path());
        let project_result = probe_project(ProjectProbeRequestV2 {
            project: project.clone(),
            draft_fingerprint: "draft.project".into(),
        });
        assert!(project_result.ok);
        assert_eq!(project_result.draft_fingerprint, "draft.project");

        let tool_result = probe_tool(ToolProbeRequestV2 {
            tool: file_tool("tool.files.read"),
            project: Some(project),
            draft_fingerprint: "draft.tool".into(),
        });
        assert!(tool_result.ok);
        assert_eq!(tool_result.adapter, "cap-std-project-files");
        assert_eq!(
            fs::read_to_string(temporary.path().join("keep.txt")).expect("fixture remains"),
            "unchanged"
        );
    }

    #[test]
    fn remote_and_missing_project_roots_fail_explicitly() {
        let mut remote = project(std::path::Path::new("remote://fixture"));
        remote.workspace.kind = WorkspaceKindV2::Remote;
        let result = probe_project(ProjectProbeRequestV2 {
            project: remote,
            draft_fingerprint: "draft.remote".into(),
        });
        assert!(!result.ok);
        assert!(result.message.contains("remote-workspace adapter"));

        let result = probe_tool(ToolProbeRequestV2 {
            tool: file_tool("tool.files.read"),
            project: None,
            draft_fingerprint: "draft.no-project".into(),
        });
        assert!(!result.ok);
        assert!(result.message.contains("Select an exact project"));
    }

    #[test]
    fn git_probe_uses_the_same_head_parser_as_execution() {
        let temporary = tempfile::tempdir().expect("temporary root");
        fs::create_dir(temporary.path().join(".git")).expect("Git directory");
        fs::write(temporary.path().join(".git/HEAD"), b"not-a-valid-head\n")
            .expect("malformed HEAD");
        let mut git = project(temporary.path());
        git.workspace.kind = WorkspaceKindV2::GitWorktree;

        let malformed = probe_project(ProjectProbeRequestV2 {
            project: git.clone(),
            draft_fingerprint: "draft.git.malformed".into(),
        });
        assert!(!malformed.ok);
        assert!(malformed.message.contains("invalid HEAD identity"));

        fs::write(
            temporary.path().join(".git/HEAD"),
            b"ref: refs/heads/feature/probed\n",
        )
        .expect("valid HEAD");
        let valid = probe_project(ProjectProbeRequestV2 {
            project: git,
            draft_fingerprint: "draft.git.valid".into(),
        });
        assert!(valid.ok, "{}", valid.message);
    }

    #[test]
    fn built_in_probe_rejects_credential_bindings_it_cannot_consume() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let mut tool = file_tool("tool.files.read");
        tool.credential_bindings = vec![crate::runtime::settings_v2::NamedCredentialBindingV2 {
            name: "API_KEY".into(),
            credential_ref: "credential.fixture".into(),
            field: "api_key".into(),
        }];
        let result = probe_tool(ToolProbeRequestV2 {
            tool,
            project: Some(project(temporary.path())),
            draft_fingerprint: "draft.bound-tool".into(),
        });
        assert!(!result.ok);
        assert!(
            result
                .message
                .contains("does not consume the configured credential bindings")
        );
    }

    #[test]
    fn extended_tool_probes_route_without_side_effects() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let project = project(temporary.path());
        // The extended project-files tools reuse the same root-confined probe.
        for tool_id in [
            "tool.files.list",
            "tool.files.grep",
            "tool.files.edit",
            "tool.files.write",
        ] {
            let result = probe_tool(ToolProbeRequestV2 {
                tool: file_tool(tool_id),
                project: Some(project.clone()),
                draft_fingerprint: format!("draft.{tool_id}"),
            });
            assert!(result.ok, "{tool_id}: {}", result.message);
            assert_eq!(result.adapter, "cap-std-project-files");
        }
        // The run-local todo adapter needs no project and no network.
        let todo_result = probe_tool(ToolProbeRequestV2 {
            tool: BuiltInToolConfigurationV2 {
                id: "tool.todo".into(),
                name: "Run task list".into(),
                enabled: true,
                requires_project: false,
                credential_bindings: Vec::new(),
                configuration: BTreeMap::from([("authorityMode".into(), json!("run_todo"))]),
            },
            project: None,
            draft_fingerprint: "draft.todo".into(),
        });
        assert!(todo_result.ok, "{}", todo_result.message);
        assert_eq!(todo_result.adapter, "run-local-todo");
        // The subagent adapter is built into the frozen model gateway.
        let subagent_result = probe_tool(ToolProbeRequestV2 {
            tool: BuiltInToolConfigurationV2 {
                id: "tool.subagent".into(),
                name: "Subagent delegation".into(),
                enabled: true,
                requires_project: false,
                credential_bindings: Vec::new(),
                configuration: BTreeMap::from([
                    ("authorityMode".into(), json!("run_subagent")),
                    ("requiresApproval".into(), json!(true)),
                ]),
            },
            project: None,
            draft_fingerprint: "draft.subagent".into(),
        });
        assert!(subagent_result.ok, "{}", subagent_result.message);
        assert_eq!(subagent_result.adapter, "run-local-subagent");
        // An unknown tool id still fails explicitly.
        let unknown = probe_tool(ToolProbeRequestV2 {
            tool: file_tool("tool.unknown"),
            project: None,
            draft_fingerprint: "draft.unknown".into(),
        });
        assert!(!unknown.ok);
        assert!(unknown.message.contains("No native built-in adapter"));
    }
}
