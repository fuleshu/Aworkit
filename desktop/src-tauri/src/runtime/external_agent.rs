//! Native Settings probe for configured external-agent lifecycle adapters.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
    time::Instant,
};

use std::sync::Arc;

use aworkit_capability_host::{
    ClaudeOneShotBackendV1, ClaudeOneShotConfigV1, ClaudeOneShotLimitsV1, ClaudePermissionModeV1,
    CodexAppServerEnvironmentV1, CodexAppServerProbeConfigV1, CodexOneShotBackendV1,
    CodexOneShotConfigV1, CodexOneShotLimitsV1, CodexPermissionModeV1, ExternalAgentBackendV1,
    probe_codex_app_server_v1,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use serde_json::Value;

use super::{
    credentials::CredentialVault,
    settings_v2::{
        CredentialMetadataConfigurationV2, ExternalAgentCapabilitiesV2,
        ExternalAgentConfigurationV2, IntegrationTransportV2, NamedCredentialBindingV2,
        SettingsConfigurationV2, validate_secret_free_stdio_argument,
    },
    tool_loop::ResolvedExternalAgentTargetV1,
};

const MAXIMUM_BINDINGS: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentProbeRequestV2 {
    pub agent: ExternalAgentConfigurationV2,
    pub draft_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentProbeResultV2 {
    pub agent_id: String,
    pub protocol: String,
    pub server_identity: Option<String>,
    pub platform_family: Option<String>,
    pub platform_os: Option<String>,
    pub account_type: Option<String>,
    pub requires_openai_auth: bool,
    pub model_ids: Vec<String>,
    pub capabilities: ExternalAgentCapabilitiesV2,
    pub latency_millis: u64,
    pub draft_fingerprint: String,
    pub message: String,
}

/// Adapter a delegation tool targets, or `None` for a tool that delegates
/// in-process. The two external delegation tools are bound to one product each.
pub(crate) fn delegation_tool_adapter(tool_id: &str) -> Option<&'static str> {
    match tool_id {
        "tool.subagent_codex" => Some("codex_app_server"),
        "tool.subagent_claude_code" => Some("claude_code"),
        _ => None,
    }
}

/// Builds the one-shot backend for one frozen delegation target.
///
/// Only products with an installed adapter are accepted, and the frozen
/// permission mode must be one the product can express; anything else fails
/// loudly instead of being translated into a weaker policy.
pub(crate) fn build_delegation_backend(
    backend: &str,
    executable: &str,
    arguments: &[String],
    permission_mode: &str,
) -> Result<Arc<dyn ExternalAgentBackendV1>, String> {
    let executable = PathBuf::from(executable);
    match backend {
        "codex" => {
            let permission_mode = match permission_mode {
                "never" => CodexPermissionModeV1::Never,
                "approveForMe" => CodexPermissionModeV1::ApproveForMe,
                "dangerouslyBypassApprovalsAndSandbox" => {
                    CodexPermissionModeV1::DangerouslyBypassApprovalsAndSandbox
                }
                other => {
                    return Err(format!(
                        "Codex App Server does not accept permission mode '{other}'"
                    ));
                }
            };
            let config = CodexOneShotConfigV1 {
                name: "codex".to_owned(),
                executable,
                arguments: arguments.to_vec(),
                working_directory: None,
                // Native Codex configuration and login stay authoritative.
                inherit_environment: true,
                environment: Vec::new(),
                permission_mode,
                limits: CodexOneShotLimitsV1::default(),
            };
            Ok(Arc::new(
                CodexOneShotBackendV1::new(config).map_err(|error| error.to_string())?,
            ))
        }
        "claude-code" => {
            let permission_mode = match permission_mode {
                "dontAsk" => ClaudePermissionModeV1::DontAsk,
                "acceptEdits" => ClaudePermissionModeV1::AcceptEdits,
                "auto" => ClaudePermissionModeV1::Auto,
                "plan" => ClaudePermissionModeV1::Plan,
                "bypassPermissions" => ClaudePermissionModeV1::BypassPermissions,
                other => {
                    return Err(format!(
                        "Claude Code does not accept permission mode '{other}'"
                    ));
                }
            };
            let config = ClaudeOneShotConfigV1 {
                name: "claude-code".to_owned(),
                executable,
                arguments: arguments.to_vec(),
                working_directory: None,
                // Native Claude settings and login stay authoritative.
                inherit_environment: true,
                environment: Vec::new(),
                permission_mode,
                limits: ClaudeOneShotLimitsV1::default(),
            };
            Ok(Arc::new(
                ClaudeOneShotBackendV1::new(config).map_err(|error| error.to_string())?,
            ))
        }
        other => Err(format!(
            "external delegation backend '{other}' has no installed adapter"
        )),
    }
}

/// Resolves the configured external-agent target one delegation tool runs, so
/// the Chat freezes exact values instead of re-reading Settings mid-Run.
///
/// Fails closed for every reason a delegation could not honestly run: no
/// enabled target, an unknown or disabled target, a transport the adapter does
/// not support, credential-backed environment values this build cannot
/// materialize for a child process, or an executable that cannot be found.
pub(crate) fn resolve_delegation_target(
    tool_id: &str,
    configuration: &BTreeMap<String, Value>,
    settings: &SettingsConfigurationV2,
) -> Result<ResolvedExternalAgentTargetV1, String> {
    let adapter = delegation_tool_adapter(tool_id)
        .ok_or_else(|| format!("tool '{tool_id}' does not delegate to an external agent"))?;
    let requested_id = configuration
        .get("targetId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let candidates = settings
        .external_agents
        .iter()
        .filter(|agent| agent.adapter == adapter)
        .collect::<Vec<_>>();
    let target = if requested_id.is_empty() {
        candidates
            .iter()
            .copied()
            .find(|agent| agent.enabled)
            .ok_or_else(|| {
                format!("no enabled {adapter} external agent is configured; add one in Settings")
            })?
    } else {
        let target = candidates
            .iter()
            .copied()
            .find(|agent| agent.id == requested_id)
            .ok_or_else(|| {
                format!(
                    "external delegation target '{requested_id}' is not a configured {adapter} target"
                )
            })?;
        if !target.enabled {
            return Err(format!(
                "external delegation target '{requested_id}' is disabled in saved Settings"
            ));
        }
        target
    };
    let IntegrationTransportV2::Stdio {
        command,
        args,
        cwd,
        env,
    } = &target.connection
    else {
        return Err(format!(
            "external delegation target '{}' requires the local STDIO transport",
            target.id
        ));
    };
    if !env.is_empty() || !target.credential_bindings.is_empty() {
        return Err(format!(
            "external delegation target '{}' injects credential-backed environment values, which this build cannot materialize for a delegation; sign in with the product's own login or remove the bindings",
            target.id
        ));
    }
    if adapter == "codex_app_server" && args.first().map(String::as_str) != Some("app-server") {
        return Err(
            "Codex App Server arguments must begin with the explicit 'app-server' subcommand"
                .into(),
        );
    }
    let executable = resolve_executable(command)?;
    let working_directory = cwd
        .as_deref()
        .map(resolve_directory)
        .transpose()?
        .map(|path| path.display().to_string());
    Ok(ResolvedExternalAgentTargetV1 {
        backend: match adapter {
            "codex_app_server" => "codex",
            _ => "claude-code",
        }
        .to_owned(),
        executable: executable.display().to_string(),
        arguments: args.clone(),
        working_directory,
        permission_mode: target.permission_mode.map_or_else(
            || match adapter {
                "codex_app_server" => "never".to_owned(),
                _ => "dontAsk".to_owned(),
            },
            |mode| mode.as_str().to_owned(),
        ),
        model: target.model.clone(),
        reasoning_effort: target.reasoning_effort.clone(),
    })
}

pub(crate) fn probe_external_agent(
    vault: &mut CredentialVault,
    credentials: &[CredentialMetadataConfigurationV2],
    request: ExternalAgentProbeRequestV2,
) -> Result<ExternalAgentProbeResultV2, String> {
    if request.draft_fingerprint.trim().is_empty() {
        return Err("external-agent probe requires a non-empty draft fingerprint".into());
    }
    if request.draft_fingerprint.len() > 256 * 1024 || request.draft_fingerprint.contains('\0') {
        return Err("external-agent draft fingerprint exceeds the native boundary".into());
    }
    StableId::parse(request.agent.id.clone())
        .map_err(|_| "external-agent id is invalid".to_owned())?;
    if request.agent.adapter != "codex_app_server" {
        return Err(format!(
            "external-agent adapter '{}' has no installed native handshake",
            request.agent.adapter
        ));
    }
    let (command, arguments, working_directory, connection_bindings) =
        match &request.agent.connection {
            IntegrationTransportV2::Stdio {
                command,
                args,
                cwd,
                env,
            } => (command, args, cwd, env),
            IntegrationTransportV2::Http { .. } => {
                return Err(
                    "Codex App Server currently requires its stable local STDIO transport".into(),
                );
            }
        };
    for argument in arguments {
        validate_secret_free_stdio_argument("external-agent STDIO argument", argument)?;
    }
    if arguments.first().map(String::as_str) != Some("app-server") {
        return Err(
            "Codex App Server arguments must begin with the explicit 'app-server' subcommand"
                .into(),
        );
    }
    if uses_non_stdio_listener(arguments) {
        return Err("the Aworkit Codex adapter supports only stable local STDIO transport".into());
    }
    let executable = resolve_executable(command)?;
    let working_directory = working_directory
        .as_deref()
        .map(resolve_directory)
        .transpose()?;
    let bindings = connection_bindings
        .iter()
        .chain(request.agent.credential_bindings.iter())
        .collect::<Vec<_>>();
    let environment = materialize_environment(vault, credentials, &bindings)?;
    let started = Instant::now();
    let result = probe_codex_app_server_v1(CodexAppServerProbeConfigV1 {
        executable,
        arguments: arguments.clone(),
        working_directory,
        // The configured Codex process owns its existing login and config.
        // Explicit credential bindings overlay inherited values for this one
        // transient process only.
        inherit_environment: true,
        environment,
        limits: Default::default(),
    })
    .map_err(|error| format!("Codex App Server handshake failed: {error}"))?;
    let capabilities = ExternalAgentCapabilitiesV2 {
        progress: result.capabilities.progress,
        continuation: result.capabilities.continuation,
        cancellation: result.capabilities.cancellation,
        approvals: result.capabilities.approvals,
    };
    let latency_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let auth = match result.account.account_type.as_deref() {
        Some(account_type) => format!("account type {account_type}"),
        None if result.account.requires_openai_auth => "authentication required".to_owned(),
        None => "no OpenAI authentication required by the active provider".to_owned(),
    };
    Ok(ExternalAgentProbeResultV2 {
        agent_id: request.agent.id,
        protocol: result.protocol,
        server_identity: result.server_identity,
        platform_family: result.platform_family,
        platform_os: result.platform_os,
        account_type: result.account.account_type,
        requires_openai_auth: result.account.requires_openai_auth,
        model_ids: result.model_ids.clone(),
        capabilities,
        latency_millis,
        draft_fingerprint: request.draft_fingerprint,
        message: format!(
            "Codex App Server handshake completed; {auth}; {} model(s) available.",
            result.model_ids.len()
        ),
    })
}

fn materialize_environment(
    vault: &mut CredentialVault,
    credentials: &[CredentialMetadataConfigurationV2],
    bindings: &[&NamedCredentialBindingV2],
) -> Result<Vec<CodexAppServerEnvironmentV1>, String> {
    if bindings.len() > MAXIMUM_BINDINGS {
        return Err(format!(
            "external-agent environment exceeds the {MAXIMUM_BINDINGS}-binding limit"
        ));
    }
    let metadata = credentials
        .iter()
        .map(|credential| (credential.credential_ref.as_str(), credential))
        .collect::<BTreeMap<_, _>>();
    if metadata.len() != credentials.len() {
        return Err("saved credential metadata contains duplicate references".into());
    }
    let mut names = BTreeSet::new();
    let mut requests = BTreeMap::<String, BTreeSet<String>>::new();
    for binding in bindings {
        if !valid_environment_name(&binding.name) {
            return Err(format!(
                "external-agent environment target '{}' is invalid",
                binding.name
            ));
        }
        let folded = if cfg!(windows) {
            binding.name.to_ascii_uppercase()
        } else {
            binding.name.clone()
        };
        if !names.insert(folded) {
            return Err(format!(
                "external-agent environment target '{}' is configured more than once",
                binding.name
            ));
        }
        let credential = metadata
            .get(binding.credential_ref.as_str())
            .ok_or_else(|| {
                format!(
                    "external agent references unknown credential '{}'",
                    binding.credential_ref
                )
            })?;
        if credential.bound_provider_id.is_some() || credential.bound_endpoint.is_some() {
            return Err(format!(
                "provider-scoped credential '{}' cannot be injected into an external agent",
                binding.credential_ref
            ));
        }
        if !credential
            .field_names
            .iter()
            .any(|field| field == &binding.field)
        {
            return Err(format!(
                "external agent references unknown field '{}' on credential '{}'",
                binding.field, binding.credential_ref
            ));
        }
        requests
            .entry(binding.credential_ref.clone())
            .or_default()
            .insert(binding.field.clone());
    }
    let mut materialized = BTreeMap::new();
    for (credential_ref, fields) in requests {
        materialized.insert(
            credential_ref.clone(),
            vault.resolve_fields(&credential_ref, fields)?,
        );
    }
    bindings
        .iter()
        .map(|binding| {
            let value = materialized
                .get(&binding.credential_ref)
                .and_then(|fields| fields.get(&binding.field))
                .ok_or_else(|| {
                    "credential store omitted an approved external-agent field".to_owned()
                })?;
            let text = String::from_utf8(value.as_slice().to_vec()).map_err(|_| {
                format!(
                    "credential field '{}' for environment target '{}' is not UTF-8",
                    binding.field, binding.name
                )
            })?;
            Ok(CodexAppServerEnvironmentV1::new(
                binding.name.clone(),
                Zeroizing::new(text),
            ))
        })
        .collect()
}

fn resolve_executable(command: &str) -> Result<PathBuf, String> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err("external-agent executable cannot be empty".into());
    }
    let path = Path::new(command);
    if path.is_absolute() {
        return canonical_file(path);
    }
    if path.components().count() != 1 {
        return Err(
            "external-agent executable must be absolute or one bare command name from PATH".into(),
        );
    }
    let search = env::var_os("PATH").ok_or_else(|| {
        "PATH is unavailable; configure an absolute external-agent executable".to_owned()
    })?;
    for directory in env::split_paths(&search) {
        if !directory.is_absolute() {
            continue;
        }
        for candidate in executable_candidates(&directory, command) {
            if candidate.is_file() {
                return canonical_file(&candidate);
            }
        }
    }
    Err(format!(
        "external-agent executable '{command}' was not found; configure its absolute path"
    ))
}

fn executable_candidates(directory: &Path, command: &str) -> Vec<PathBuf> {
    let base = directory.join(command);
    if !cfg!(windows) || Path::new(command).extension().is_some() {
        return vec![base];
    }
    let extensions = env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".EXE".into(), ".CMD".into(), ".BAT".into()]);
    let mut candidates = vec![base.clone()];
    candidates.extend(
        extensions
            .into_iter()
            .map(|extension| directory.join(format!("{command}{extension}"))),
    );
    candidates
}

fn canonical_file(path: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| "external-agent executable could not be resolved".to_owned())?;
    if !canonical.is_file() {
        return Err("external-agent executable is not a regular file".into());
    }
    Ok(canonical)
}

fn resolve_directory(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err("external-agent working directory must be absolute".into());
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| "external-agent working directory could not be resolved".to_owned())?;
    if !canonical.is_dir() {
        return Err("external-agent working directory is not a directory".into());
    }
    Ok(canonical)
}

fn valid_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('=')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn uses_non_stdio_listener(arguments: &[String]) -> bool {
    arguments
        .windows(2)
        .any(|pair| pair[0] == "--listen" && pair[1] != "stdio://" && pair[1] != "stdio")
        || arguments.iter().any(|argument| {
            argument
                .strip_prefix("--listen=")
                .is_some_and(|transport| transport != "stdio://" && transport != "stdio")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One configured target whose executable is this test binary, which is a
    /// real absolute file the resolver can attest.
    fn target(adapter: &str, enabled: bool) -> ExternalAgentConfigurationV2 {
        ExternalAgentConfigurationV2 {
            id: "agent.fixture".into(),
            name: "Fixture".into(),
            adapter: adapter.into(),
            enabled,
            connection: IntegrationTransportV2::Stdio {
                command: std::env::current_exe()
                    .expect("test executable")
                    .display()
                    .to_string(),
                args: if adapter == "codex_app_server" {
                    vec!["app-server".into()]
                } else {
                    Vec::new()
                },
                cwd: None,
                env: Vec::new(),
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
            permission_mode: None,
            model: None,
            reasoning_effort: None,
        }
    }

    fn configuration(target_id: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([("targetId".to_owned(), Value::String(target_id.to_owned()))])
    }

    #[test]
    fn a_delegation_resolves_one_enabled_target_of_its_own_product() {
        let mut settings = SettingsConfigurationV2::default();
        settings
            .external_agents
            .push(target("codex_app_server", true));
        let resolved = resolve_delegation_target(
            "tool.subagent_codex",
            &configuration("agent.fixture"),
            &settings,
        )
        .expect("an enabled Codex target resolves");
        assert_eq!(resolved.backend, "codex");
        assert_eq!(resolved.permission_mode, "never");
        assert_eq!(resolved.arguments, ["app-server"]);
        assert!(resolved.model.is_none() && resolved.reasoning_effort.is_none());

        // An empty id selects the first enabled target of that product.
        let resolved =
            resolve_delegation_target("tool.subagent_codex", &BTreeMap::new(), &settings)
                .expect("the first enabled target resolves");
        assert_eq!(resolved.backend, "codex");

        // The other product's tool never accepts this target.
        let error =
            resolve_delegation_target("tool.subagent_claude_code", &BTreeMap::new(), &settings)
                .expect_err("no Claude target is configured");
        assert!(error.contains("no enabled claude_code"), "{error}");
        // A tool that does not delegate is not resolvable at all.
        assert!(resolve_delegation_target("tool.subagent", &BTreeMap::new(), &settings).is_err());
    }

    #[test]
    fn delegation_targets_fail_closed_without_a_usable_product_target() {
        let mut settings = SettingsConfigurationV2::default();
        settings
            .external_agents
            .push(target("codex_app_server", false));
        let disabled = resolve_delegation_target(
            "tool.subagent_codex",
            &configuration("agent.fixture"),
            &settings,
        )
        .expect_err("a disabled target is refused");
        assert!(
            disabled.contains("disabled in saved Settings"),
            "{disabled}"
        );
        let none = resolve_delegation_target("tool.subagent_codex", &BTreeMap::new(), &settings)
            .expect_err("a disabled target is not a default");
        assert!(none.contains("no enabled codex_app_server"), "{none}");

        settings.external_agents[0].enabled = true;
        let unknown = resolve_delegation_target(
            "tool.subagent_codex",
            &configuration("agent.other"),
            &settings,
        )
        .expect_err("an unknown target id is refused");
        assert!(
            unknown.contains("not a configured codex_app_server target"),
            "{unknown}"
        );

        // Credential-backed environment values cannot be materialized for a
        // delegation yet, so the target is refused instead of run unauthenticated.
        settings.external_agents[0].credential_bindings = vec![NamedCredentialBindingV2 {
            name: "OPENAI_API_KEY".into(),
            credential_ref: "credential.fixture".to_owned(),
            field: "token".into(),
        }];
        let credentialed = resolve_delegation_target(
            "tool.subagent_codex",
            &configuration("agent.fixture"),
            &settings,
        )
        .expect_err("credential injection is refused");
        assert!(
            credentialed.contains("cannot materialize"),
            "{credentialed}"
        );

        // A transport the product does not implement is refused.
        settings.external_agents[0].credential_bindings.clear();
        settings.external_agents[0].connection = IntegrationTransportV2::Http {
            url: "https://agent.example/rpc".into(),
            headers: Vec::new(),
        };
        let http = resolve_delegation_target(
            "tool.subagent_codex",
            &configuration("agent.fixture"),
            &settings,
        )
        .expect_err("HTTP is refused");
        assert!(http.contains("local STDIO transport"), "{http}");
    }

    #[test]
    fn executable_resolution_accepts_absolute_and_bare_path_entries() {
        let executable = std::env::current_exe().expect("test executable");
        assert_eq!(
            resolve_executable(executable.to_str().expect("UTF-8 executable"))
                .expect("absolute executable"),
            std::fs::canonicalize(executable).expect("canonical executable")
        );
        assert!(resolve_executable("nested/tool").is_err());
    }

    #[test]
    fn environment_names_are_strict() {
        assert!(valid_environment_name("OPENAI_API_KEY"));
        assert!(!valid_environment_name("OPENAI-API-KEY"));
        assert!(!valid_environment_name("=OPENAI_API_KEY"));
        assert!(!valid_environment_name(""));
    }

    #[test]
    fn codex_probe_refuses_non_stdio_listeners_in_both_cli_forms() {
        assert!(uses_non_stdio_listener(&[
            "app-server".into(),
            "--listen".into(),
            "ws://127.0.0.1:4500".into(),
        ]));
        assert!(uses_non_stdio_listener(&[
            "app-server".into(),
            "--listen=unix://".into(),
        ]));
        assert!(!uses_non_stdio_listener(&[
            "app-server".into(),
            "--listen=stdio://".into(),
        ]));
    }
}
