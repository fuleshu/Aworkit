//! Native Settings probe for configured external-agent lifecycle adapters.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
    time::Instant,
};

use aworkit_capability_host::{
    CodexAppServerEnvironmentV1, CodexAppServerProbeConfigV1, probe_codex_app_server_v1,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{
    credentials::CredentialVault,
    settings_v2::{
        CredentialMetadataConfigurationV2, ExternalAgentCapabilitiesV2,
        ExternalAgentConfigurationV2, IntegrationTransportV2, NamedCredentialBindingV2,
        validate_secret_free_stdio_argument,
    },
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
