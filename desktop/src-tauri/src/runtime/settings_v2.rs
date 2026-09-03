//! Canonical, secret-free desktop settings schema and validation.
//!
//! This module deliberately models references to credential-store records, not
//! credential values. The same strict shape is used for the canonical JSON
//! document and the full-document desktop boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::{
    PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1, PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1,
    PROJECT_FILE_READ_MAXIMUM_BYTES_V1, PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
    PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1, WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1,
    WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1, WEB_SEARCH_MAXIMUM_RESULTS_V1,
};

/// Current canonical desktop-settings schema.
pub const SETTINGS_SCHEMA_VERSION_V2: u16 = 2;

const MAX_COLLECTION_ITEMS: usize = 1_024;
const MAX_MODELS_PER_PROVIDER: usize = 1_024;
const MAX_BINDINGS: usize = 256;
const MAX_ARGUMENTS: usize = 512;
const MAX_STRING_BYTES: usize = 16 * 1024;
const MAX_FREEFORM_BYTES: usize = 256 * 1024;
const MAX_FREEFORM_DEPTH: usize = 16;
pub(crate) const DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1: u64 = 300;
pub(crate) const MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1: u64 = 3_600;
pub(crate) const DEFAULT_MAXIMUM_TOOL_OUTPUT_BYTES_V1: usize = 64 * 1024;
pub(crate) const MINIMUM_MAXIMUM_TOOL_OUTPUT_BYTES_V1: u64 = 1024;
pub(crate) const MAXIMUM_MAXIMUM_TOOL_OUTPUT_BYTES_V1: u64 = 512 * 1024;
const STANDARD_TIERS: [(&str, &str); 4] = [
    ("tier:fast", "Fast"),
    ("tier:simple", "Simple"),
    ("tier:balanced", "Balanced"),
    ("tier:quality", "Quality"),
];
const BUILTIN_TOOL_IDS: [&str; 12] = [
    "tool.files.read",
    "tool.files.search",
    "tool.files.list",
    "tool.files.grep",
    "tool.files.edit",
    "tool.files.write",
    "tool.shell.host",
    "tool.python.host",
    "tool.todo",
    "tool.web_search",
    "tool.web_fetch",
    "tool.subagent",
];

/// Complete canonical configuration persisted as one versioned JSON document.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsConfigurationV2 {
    pub schema_version: u16,
    pub providers: Vec<ProviderConfigurationV2>,
    pub model_tiers: Vec<ModelTierConfigurationV2>,
    pub credentials: Vec<CredentialMetadataConfigurationV2>,
    pub tools: Vec<BuiltInToolConfigurationV2>,
    pub extensions: Vec<ExtensionConfigurationV2>,
    pub mcp_servers: Vec<McpServerConfigurationV2>,
    pub external_agents: Vec<ExternalAgentConfigurationV2>,
    pub data: DataConfigurationV2,
    pub projects: Vec<ProjectConfigurationV2>,
    pub appearance: AppearanceConfigurationV2,
}

impl Default for SettingsConfigurationV2 {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION_V2,
            providers: Vec::new(),
            model_tiers: standard_model_tiers(),
            credentials: Vec::new(),
            tools: default_builtin_tools(),
            extensions: Vec::new(),
            mcp_servers: Vec::new(),
            external_agents: Vec::new(),
            data: DataConfigurationV2::default(),
            projects: Vec::new(),
            appearance: AppearanceConfigurationV2::default(),
        }
    }
}

impl SettingsConfigurationV2 {
    /// Validates the full document, including all cross-section references.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION_V2 {
            return Err(format!(
                "settings schemaVersion must be {SETTINGS_SCHEMA_VERSION_V2}, got {}",
                self.schema_version
            ));
        }
        validate_collection_size("providers", self.providers.len())?;
        validate_collection_size("model tiers", self.model_tiers.len())?;
        validate_collection_size("credentials", self.credentials.len())?;
        validate_collection_size("tools", self.tools.len())?;
        validate_collection_size("extensions", self.extensions.len())?;
        validate_collection_size("MCP servers", self.mcp_servers.len())?;
        validate_collection_size("external agents", self.external_agents.len())?;
        validate_collection_size("projects", self.projects.len())?;

        let provider_ids = unique_ids(
            "provider",
            self.providers.iter().map(|provider| provider.id.as_str()),
        )?;
        let credential_refs = unique_ids(
            "credential reference",
            self.credentials
                .iter()
                .map(|credential| credential.credential_ref.as_str()),
        )?;
        let tool_ids = unique_ids("tool", self.tools.iter().map(|tool| tool.id.as_str()))?;
        let extension_ids = unique_ids(
            "extension",
            self.extensions
                .iter()
                .map(|extension| extension.id.as_str()),
        )?;
        let mcp_ids = unique_ids(
            "MCP server",
            self.mcp_servers.iter().map(|server| server.id.as_str()),
        )?;
        let external_agent_ids = unique_ids(
            "external agent",
            self.external_agents.iter().map(|agent| agent.id.as_str()),
        )?;
        let project_ids = unique_ids(
            "project",
            self.projects.iter().map(|project| project.id.as_str()),
        )?;
        let expected_tool_ids = BUILTIN_TOOL_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<BTreeSet<_>>();
        if tool_ids != expected_tool_ids {
            return Err(format!(
                "built-in tools must contain exactly the implemented IDs: {}",
                BUILTIN_TOOL_IDS.join(", ")
            ));
        }
        debug_assert_eq!(extension_ids.len(), self.extensions.len());
        debug_assert_eq!(external_agent_ids.len(), self.external_agents.len());
        debug_assert_eq!(project_ids.len(), self.projects.len());

        let mut model_refs = BTreeSet::new();
        for provider in &self.providers {
            provider.validate(&credential_refs)?;
            for model in &provider.models {
                if !model_refs.insert((provider.id.clone(), model.id.clone())) {
                    return Err(format!(
                        "provider '{}' contains duplicate model id '{}'",
                        provider.id, model.id
                    ));
                }
            }
        }
        for credential in &self.credentials {
            credential.validate(&provider_ids)?;
        }
        let credential_fields = self
            .credentials
            .iter()
            .map(|credential| {
                (
                    credential.credential_ref.clone(),
                    credential.field_names.iter().cloned().collect(),
                )
            })
            .collect::<BTreeMap<String, BTreeSet<String>>>();
        for provider in &self.providers {
            if let Some(reference) = provider.credential_ref.as_deref() {
                let credential = self
                    .credential(reference)
                    .expect("provider credential reference was validated");
                if let Some(bound_provider_id) = credential.bound_provider_id.as_deref()
                    && (bound_provider_id != provider.id
                        || credential.bound_endpoint.as_deref() != Some(provider.base_url.as_str()))
                {
                    return Err(format!(
                        "provider '{}' cannot use credential '{}' because it is bound to another provider or endpoint",
                        provider.id, reference
                    ));
                }
            }
        }
        validate_tiers(&self.model_tiers, &model_refs)?;
        for tool in &self.tools {
            tool.validate(&credential_fields)?;
        }
        for extension in &self.extensions {
            extension.validate()?;
        }
        for server in &self.mcp_servers {
            server.validate(&credential_fields)?;
        }
        for agent in &self.external_agents {
            agent.validate(&credential_fields, &mcp_ids)?;
        }
        self.data.validate()?;
        for project in &self.projects {
            project.validate()?;
        }
        self.appearance.validate()?;
        Ok(())
    }

    /// Rejects values exposed by earlier Settings builds when the installed
    /// production adapters have no consumer for them. Loading remains
    /// lossless so an incompatible record can be repaired explicitly; a new
    /// generic Settings commit may not reaffirm ignored secret or lifecycle
    /// semantics.
    pub(crate) fn validate_installed_runtime_consumers(&self) -> Result<(), String> {
        for provider in &self.providers {
            provider.runtime_limits()?;
            let Some(reference) = provider.credential_ref.as_deref() else {
                continue;
            };
            let credential = self.credential(reference).ok_or_else(|| {
                format!("provider '{}' references unknown credential", provider.id)
            })?;
            if !credential
                .field_names
                .iter()
                .any(|field| field == "api_key")
            {
                return Err(format!(
                    "provider '{}' credential '{}' has no api_key field required by the installed adapters",
                    provider.id, reference
                ));
            }
        }
        for tool in &self.tools {
            if tool.id != "tool.web_search" && !tool.credential_bindings.is_empty() {
                return Err(format!(
                    "built-in tool '{}' has credential bindings, but the installed adapter cannot consume them",
                    tool.id
                ));
            }
            if tool.id == "tool.web_search" {
                for binding in &tool.credential_bindings {
                    let credential = self.credential(&binding.credential_ref).ok_or_else(|| {
                        format!(
                            "web-search tool references unknown credential '{}'",
                            binding.credential_ref
                        )
                    })?;
                    if credential.bound_provider_id.is_some() {
                        return Err(format!(
                            "web-search tool credential '{}' must be an unbound integration credential",
                            binding.credential_ref
                        ));
                    }
                }
            }
        }
        let provider_scoped = self
            .credentials
            .iter()
            .filter(|credential| credential.bound_provider_id.is_some())
            .map(|credential| credential.credential_ref.as_str())
            .collect::<BTreeSet<_>>();
        for server in &self.mcp_servers {
            validate_mcp_transport_targets(&server.id, &server.transport)?;
            if let Some(binding) = transport_bindings(&server.transport)
                .iter()
                .find(|binding| provider_scoped.contains(binding.credential_ref.as_str()))
            {
                return Err(format!(
                    "MCP server '{}' cannot inject provider-scoped credential '{}'",
                    server.id, binding.credential_ref
                ));
            }
        }
        for agent in &self.external_agents {
            validate_external_agent_transport_targets(agent)?;
            if let Some(binding) = transport_bindings(&agent.connection)
                .iter()
                .chain(agent.credential_bindings.iter())
                .find(|binding| provider_scoped.contains(binding.credential_ref.as_str()))
            {
                return Err(format!(
                    "external agent '{}' cannot inject provider-scoped credential '{}'",
                    agent.id, binding.credential_ref
                ));
            }
            if agent.capabilities != ExternalAgentCapabilitiesV2::default() {
                return Err(format!(
                    "external agent '{}' has persisted capabilities, but negotiated capabilities are ephemeral probe output and cannot be persisted by generic or dedicated Settings commands",
                    agent.id
                ));
            }
            if agent.adapter == "codex_app_server" {
                if !matches!(agent.connection, IntegrationTransportV2::Stdio { .. }) {
                    return Err(
                        "Codex App Server requires the installed local STDIO transport".into(),
                    );
                }
                if !agent.mcp_server_ids.is_empty() {
                    return Err(format!(
                        "external agent '{}' has MCP forwarding metadata, but the installed adapter cannot consume it",
                        agent.id
                    ));
                }
                if !agent.configuration.is_empty() {
                    return Err(format!(
                        "external agent '{}' has adapter configuration, but the installed adapter cannot consume it",
                        agent.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Appends built-in tool entries that newer builds introduced after the
    /// stored document was written, preserving every existing entry exactly
    /// (including its enabled state). New entries start disabled with the
    /// current default contract and the list is reordered to the canonical
    /// implemented-id order. Returns whether the document changed.
    pub(crate) fn reconcile_builtin_tools(&mut self) -> bool {
        let existing = self
            .tools
            .iter()
            .map(|tool| tool.id.clone())
            .collect::<BTreeSet<String>>();
        let mut changed = false;
        for id in BUILTIN_TOOL_IDS {
            if !existing.contains(id) {
                let default = default_builtin_tools()
                    .into_iter()
                    .find(|tool| tool.id == id)
                    .expect("built-in tool default exists");
                self.tools.push(default);
                changed = true;
            }
        }
        if changed {
            let order = BUILTIN_TOOL_IDS
                .iter()
                .enumerate()
                .map(|(index, id)| (*id, index))
                .collect::<BTreeMap<_, _>>();
            self.tools
                .sort_by_key(|tool| order.get(tool.id.as_str()).copied().unwrap_or(usize::MAX));
        }
        changed
    }

    /// Clears controls that older rescue builds persisted without composing
    /// the corresponding runtime behavior. The caller persists this one-way
    /// compatibility repair before exposing the canonical Settings snapshot.
    pub(crate) fn disable_inactive_runtime_controls(&mut self) -> bool {
        let mut changed = false;
        for server in &mut self.mcp_servers {
            if server.auto_connect {
                server.auto_connect = false;
                changed = true;
            }
        }
        if self.data.portable_history_enabled
            || self.data.detailed_capture_enabled
            || self.data.detailed_capture_retention_days.is_some()
            || self.data.local_history_retention_days.is_some()
        {
            self.data.portable_history_enabled = false;
            self.data.detailed_capture_enabled = false;
            self.data.detailed_capture_retention_days = None;
            self.data.local_history_retention_days = None;
            changed = true;
        }
        for project in &mut self.projects {
            if project.portable_history_enabled {
                project.portable_history_enabled = false;
                changed = true;
            }
        }
        changed
    }

    /// Narrows project-tool limits written by earlier desktop builds to the
    /// first persistence-safe native contract. This is used only while opening
    /// an already-versioned document; new save requests are rejected instead
    /// of silently rewritten.
    pub(crate) fn normalize_legacy_project_tool_limits(&mut self) -> bool {
        let mut changed = false;
        for tool in &mut self.tools {
            match tool.id.as_str() {
                "tool.files.read" => {
                    if tool
                        .configuration
                        .get("maximumBytes")
                        .and_then(Value::as_u64)
                        .is_some_and(|value| value > PROJECT_FILE_READ_MAXIMUM_BYTES_V1)
                    {
                        tool.configuration.insert(
                            "maximumBytes".to_owned(),
                            Value::from(PROJECT_FILE_READ_MAXIMUM_BYTES_V1),
                        );
                        changed = true;
                    }
                }
                "tool.files.search" => {
                    if tool
                        .configuration
                        .get("maximumResults")
                        .and_then(Value::as_u64)
                        .is_some_and(|value| value > PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1)
                    {
                        tool.configuration.insert(
                            "maximumResults".to_owned(),
                            Value::from(PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1),
                        );
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        changed
    }

    /// Removes the obsolete subagent turn limit from stored Settings. Child
    /// loops now use the same natural-completion and repeat-reminder strategy
    /// as their parent Agent loop.
    pub(crate) fn normalize_legacy_agent_turn_limits(&mut self) -> bool {
        self.tools
            .iter_mut()
            .filter(|tool| tool.id == "tool.subagent")
            .any(|tool| tool.configuration.remove("maximumTurns").is_some())
    }

    /// Expands the one-field web-search record written by earlier builds into
    /// the complete provider-neutral configuration without changing its
    /// enabled state or credential bindings.
    pub(crate) fn normalize_legacy_web_search_configuration(&mut self) -> bool {
        let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
        else {
            return false;
        };
        let defaults = web_search_default_configuration();
        let mut changed = false;
        for (key, value) in defaults {
            if !tool.configuration.contains_key(&key) {
                tool.configuration.insert(key, value);
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn credential(&self, reference: &str) -> Option<&CredentialMetadataConfigurationV2> {
        self.credentials
            .iter()
            .find(|credential| credential.credential_ref == reference)
    }
}

/// One configured model provider.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfigurationV2 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    pub models: Vec<ModelConfigurationV2>,
    pub configuration: BTreeMap<String, Value>,
}

impl ProviderConfigurationV2 {
    fn validate(&self, credential_refs: &BTreeSet<String>) -> Result<(), String> {
        validate_stable_id("provider id", &self.id)?;
        validate_label("provider name", &self.name)?;
        validate_symbol("provider kind", &self.kind)?;
        validate_http_url("provider base URL", &self.base_url)?;
        if self.models.len() > MAX_MODELS_PER_PROVIDER {
            return Err(format!(
                "provider '{}' exceeds the {MAX_MODELS_PER_PROVIDER} model limit",
                self.id
            ));
        }
        let mut model_ids = BTreeSet::new();
        for model in &self.models {
            model.validate()?;
            if !model_ids.insert(model.id.clone()) {
                return Err(format!(
                    "provider '{}' contains duplicate model id '{}'",
                    self.id, model.id
                ));
            }
        }
        if self.enabled && !self.models.iter().any(|model| model.enabled) {
            return Err(format!(
                "enabled provider '{}' must have at least one enabled model",
                self.id
            ));
        }
        if let Some(reference) = self.credential_ref.as_deref()
            && !credential_refs.contains(reference)
        {
            return Err(format!(
                "provider '{}' references unknown credential '{}'",
                self.id, reference
            ));
        }
        validate_freeform(
            "provider configuration",
            &Value::Object(to_json_map(&self.configuration)),
        )
    }

    /// Resolves the closed provider/runtime controls while keeping older
    /// records with an empty configuration on the current safe defaults.
    pub(crate) fn runtime_limits(&self) -> Result<ProviderRuntimeLimitsV1, String> {
        let supported = BTreeSet::from(["requestTimeoutSeconds", "maximumToolOutputBytes"]);
        if let Some(unknown) = self
            .configuration
            .keys()
            .find(|key| !supported.contains(key.as_str()))
        {
            return Err(format!(
                "provider '{}' configuration field '{}' is unsupported by the installed native runtime",
                self.id, unknown
            ));
        }
        let request_timeout_seconds = optional_provider_u64(
            self,
            "requestTimeoutSeconds",
            1,
            MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1,
        )?
        .unwrap_or(DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1);
        let maximum_tool_output_bytes = optional_provider_u64(
            self,
            "maximumToolOutputBytes",
            MINIMUM_MAXIMUM_TOOL_OUTPUT_BYTES_V1,
            MAXIMUM_MAXIMUM_TOOL_OUTPUT_BYTES_V1,
        )?
        .map_or(DEFAULT_MAXIMUM_TOOL_OUTPUT_BYTES_V1, |value| {
            usize::try_from(value).unwrap_or(usize::MAX)
        });
        Ok(ProviderRuntimeLimitsV1 {
            request_timeout_seconds,
            maximum_tool_output_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderRuntimeLimitsV1 {
    pub request_timeout_seconds: u64,
    pub maximum_tool_output_bytes: usize,
}

fn optional_provider_u64(
    provider: &ProviderConfigurationV2,
    field: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let Some(value) = provider.configuration.get(field) else {
        return Ok(None);
    };
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .map(Some)
        .ok_or_else(|| {
            format!(
                "provider '{}' configuration.{field} must be an integer from {minimum} through {maximum}",
                provider.id
            )
        })
}

/// One concrete provider model selectable by a model tier.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelConfigurationV2 {
    pub id: String,
    pub name: String,
    pub remote_id: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    pub capabilities: Vec<String>,
    pub parameters: BTreeMap<String, Value>,
}

impl ModelConfigurationV2 {
    fn validate(&self) -> Result<(), String> {
        validate_stable_id("model id", &self.id)?;
        validate_label("model name", &self.name)?;
        validate_nonempty("remote model id", &self.remote_id)?;
        if self.context_window == Some(0) || self.max_output_tokens == Some(0) {
            return Err(format!(
                "model '{}' token limits must be positive when present",
                self.id
            ));
        }
        unique_symbols("model capability", &self.capabilities)?;
        validate_freeform(
            "model parameters",
            &Value::Object(to_json_map(&self.parameters)),
        )
    }
}

/// Distinguishes reserved portable tiers from user-defined tiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTierKindV2 {
    Standard,
    Custom,
}

/// One portable model tier and its resolution strategy.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelTierConfigurationV2 {
    pub id: String,
    pub name: String,
    pub kind: ModelTierKindV2,
    pub resolution: ModelTierResolutionV2,
}

/// Concrete model identity used in tier resolution.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelTargetV2 {
    pub provider_id: String,
    pub model_id: String,
}

/// Supported subordinate tier-selection preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPolicyPreferenceV2 {
    Quality,
    Latency,
    Cost,
}

/// How a portable tier maps to configured concrete models.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "strategy", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelTierResolutionV2 {
    Unconfigured,
    Exact {
        target: ModelTargetV2,
    },
    Fallback {
        targets: Vec<ModelTargetV2>,
    },
    Policy {
        candidates: Vec<ModelTargetV2>,
        preference: ModelPolicyPreferenceV2,
    },
}

/// Metadata for one opaque operating-system credential-store record.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialMetadataConfigurationV2 {
    pub credential_ref: String,
    pub label: String,
    pub kind: String,
    pub field_names: Vec<String>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_endpoint: Option<String>,
}

impl CredentialMetadataConfigurationV2 {
    fn validate(&self, provider_ids: &BTreeSet<String>) -> Result<(), String> {
        validate_credential_ref(&self.credential_ref)?;
        validate_label("credential label", &self.label)?;
        validate_symbol("credential kind", &self.kind)?;
        if self.revision == 0 {
            return Err(format!(
                "credential '{}' revision must be positive",
                self.credential_ref
            ));
        }
        if self.field_names.is_empty() {
            return Err(format!(
                "credential '{}' must expose at least one field name",
                self.credential_ref
            ));
        }
        unique_symbols("credential field name", &self.field_names)?;
        match (
            self.bound_provider_id.as_deref(),
            self.bound_endpoint.as_deref(),
        ) {
            (Some(provider_id), Some(endpoint)) => {
                if !provider_ids.contains(provider_id) {
                    return Err(format!(
                        "credential '{}' is bound to unknown provider '{}'",
                        self.credential_ref, provider_id
                    ));
                }
                validate_http_url("credential-bound endpoint", endpoint)?;
            }
            (None, None) => {}
            _ => {
                return Err(format!(
                    "credential '{}' provider and endpoint bindings must either both be present or both be absent",
                    self.credential_ref
                ));
            }
        }
        Ok(())
    }
}

/// Maps a named integration field to one field in an opaque credential record.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedCredentialBindingV2 {
    pub name: String,
    pub credential_ref: String,
    pub field: String,
}

impl NamedCredentialBindingV2 {
    fn validate(
        &self,
        owner: &str,
        credentials: &BTreeMap<String, BTreeSet<String>>,
    ) -> Result<(), String> {
        validate_symbol("credential binding name", &self.name)?;
        validate_symbol("credential binding field", &self.field)?;
        let Some(fields) = credentials.get(&self.credential_ref) else {
            return Err(format!(
                "{owner} references unknown credential '{}'",
                self.credential_ref
            ));
        };
        if !fields.contains(&self.field) {
            return Err(format!(
                "{owner} references unknown field '{}' on credential '{}'",
                self.field, self.credential_ref
            ));
        }
        Ok(())
    }
}

/// Configuration for one Aworkit-owned built-in tool.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuiltInToolConfigurationV2 {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub requires_project: bool,
    pub credential_bindings: Vec<NamedCredentialBindingV2>,
    pub configuration: BTreeMap<String, Value>,
}

impl BuiltInToolConfigurationV2 {
    fn validate(&self, credentials: &BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
        validate_stable_id("tool id", &self.id)?;
        validate_label("tool name", &self.name)?;
        validate_bindings(
            &format!("tool '{}'", self.id),
            &self.credential_bindings,
            credentials,
        )?;
        validate_freeform(
            "tool configuration",
            &Value::Object(to_json_map(&self.configuration)),
        )?;
        self.validate_implemented_contract()
    }

    fn validate_implemented_contract(&self) -> Result<(), String> {
        match self.id.as_str() {
            "tool.files.read" => {
                require_exact_config_keys(self, &["authorityMode", "effect", "maximumBytes"])?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "read")?;
                require_config_u64(self, "maximumBytes", 1, PROJECT_FILE_READ_MAXIMUM_BYTES_V1)
            }
            "tool.files.search" => {
                require_exact_config_keys(self, &["authorityMode", "effect", "maximumResults"])?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "search")?;
                require_config_u64(
                    self,
                    "maximumResults",
                    1,
                    PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
                )
            }
            "tool.files.edit" => {
                require_exact_config_keys(
                    self,
                    &[
                        "authorityMode",
                        "effect",
                        "requiresApproval",
                        "maximumBytes",
                    ],
                )?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "write")?;
                require_config_bool(self, "requiresApproval", true)?;
                require_config_u64(self, "maximumBytes", 1, 1_048_576)
            }
            "tool.shell.host" => {
                require_exact_config_keys(
                    self,
                    &[
                        "authorityMode",
                        "requiresApproval",
                        "timeoutSeconds",
                        "maximumOutputBytes",
                    ],
                )?;
                require_tool_project_scope(self, false)?;
                require_config_string(self, "authorityMode", "host_shell")?;
                require_config_bool(self, "requiresApproval", true)?;
                require_config_u64(self, "timeoutSeconds", 1, 300)?;
                require_config_u64(self, "maximumOutputBytes", 1, 262_144)
            }
            "tool.python.host" => {
                require_exact_config_keys(
                    self,
                    &[
                        "authorityMode",
                        "requiresApproval",
                        "isolatedInterpreter",
                        "timeoutSeconds",
                        "maximumOutputBytes",
                    ],
                )?;
                require_tool_project_scope(self, false)?;
                require_config_string(self, "authorityMode", "host_python")?;
                require_config_bool(self, "requiresApproval", true)?;
                require_config_bool(self, "isolatedInterpreter", true)?;
                require_config_u64(self, "timeoutSeconds", 1, 300)?;
                require_config_u64(self, "maximumOutputBytes", 1, 262_144)
            }
            "tool.files.list" => {
                require_exact_config_keys(self, &["authorityMode", "effect", "maximumEntries"])?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "list")?;
                require_config_u64(
                    self,
                    "maximumEntries",
                    1,
                    PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1,
                )
            }
            "tool.files.grep" => {
                require_exact_config_keys(self, &["authorityMode", "effect", "maximumMatches"])?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "grep")?;
                require_config_u64(
                    self,
                    "maximumMatches",
                    1,
                    PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1,
                )
            }
            "tool.files.write" => {
                require_exact_config_keys(
                    self,
                    &[
                        "authorityMode",
                        "effect",
                        "requiresApproval",
                        "maximumBytes",
                    ],
                )?;
                require_tool_project_scope(self, true)?;
                require_config_string(self, "authorityMode", "project_files")?;
                require_config_string(self, "effect", "write")?;
                require_config_bool(self, "requiresApproval", true)?;
                require_config_u64(self, "maximumBytes", 1, PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1)
            }
            "tool.todo" => {
                require_exact_config_keys(self, &["authorityMode"])?;
                require_tool_project_scope(self, false)?;
                require_config_string(self, "authorityMode", "run_todo")
            }
            "tool.web_search" => {
                require_exact_config_keys(
                    self,
                    &[
                        "backend",
                        "credentialBackend",
                        "providerTier",
                        "maximumResults",
                        "requestTimeoutSeconds",
                        "maximumRetries",
                        "keylessFallback",
                        "keylessRescue",
                        "cacheEnabled",
                        "cacheTtlMinutes",
                        "searxngBaseUrl",
                        "providerBaseUrl",
                        "parallelSearchMode",
                        "xaiModel",
                        "xaiAllowedDomains",
                        "xaiExcludedDomains",
                        "deepseekBaseUrl",
                        "deepseekModel",
                        "deepseekMaximumOutputTokens",
                    ],
                )?;
                require_tool_project_scope(self, false)?;
                let backend = require_config_one_of(
                    self,
                    "backend",
                    &[
                        "automatic",
                        "keyless",
                        "duckduckgo",
                        "searxng",
                        "exa",
                        "parallel",
                        "firecrawl",
                        "tavily",
                        "brave",
                        "keenable",
                        "xai",
                        "deepseek",
                    ],
                )?;
                require_config_one_of(
                    self,
                    "credentialBackend",
                    &[
                        "exa",
                        "parallel",
                        "firecrawl",
                        "tavily",
                        "brave",
                        "keenable",
                        "xai",
                        "deepseek",
                    ],
                )?;
                let provider_tier =
                    require_config_one_of(self, "providerTier", &["automatic", "free", "paid"])?;
                require_config_u64(self, "maximumResults", 1, WEB_SEARCH_MAXIMUM_RESULTS_V1)?;
                require_config_u64(self, "requestTimeoutSeconds", 5, 120)?;
                require_config_u64(self, "maximumRetries", 0, 3)?;
                require_config_boolean(self, "keylessFallback")?;
                require_config_boolean(self, "keylessRescue")?;
                require_config_boolean(self, "cacheEnabled")?;
                require_config_u64(self, "cacheTtlMinutes", 1, 1_440)?;
                require_optional_search_url(self, "searxngBaseUrl", true)?;
                require_optional_search_url(self, "providerBaseUrl", true)?;
                require_config_one_of(
                    self,
                    "parallelSearchMode",
                    &["fast", "one-shot", "agentic"],
                )?;
                require_config_text(self, "xaiModel", 1, 256)?;
                require_search_domain_filters(self)?;
                require_optional_search_url(self, "deepseekBaseUrl", false)?;
                require_config_text(self, "deepseekModel", 1, 256)?;
                require_config_u64(self, "deepseekMaximumOutputTokens", 256, 16_384)?;
                let keyless_fallback = self.configuration["keylessFallback"]
                    .as_bool()
                    .expect("validated keylessFallback");
                let keyless_rescue = self.configuration["keylessRescue"]
                    .as_bool()
                    .expect("validated keylessRescue");
                if keyless_rescue && !keyless_fallback {
                    return Err(
                        "built-in tool 'tool.web_search' keylessRescue requires keylessFallback"
                            .into(),
                    );
                }
                if backend == "searxng"
                    && self.configuration["searxngBaseUrl"]
                        .as_str()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(
                        "built-in tool 'tool.web_search' requires searxngBaseUrl when backend is searxng"
                            .into(),
                    );
                }
                if backend == "deepseek"
                    && self.configuration["deepseekBaseUrl"]
                        .as_str()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(
                        "built-in tool 'tool.web_search' requires deepseekBaseUrl when backend is deepseek"
                            .into(),
                    );
                }
                if backend == "automatic" && provider_tier != "automatic" {
                    return Err(
                        "built-in tool 'tool.web_search' providerTier must be automatic when backend is automatic"
                            .into(),
                    );
                }
                if matches!(backend, "keyless" | "duckduckgo" | "searxng")
                    && provider_tier != "automatic"
                {
                    return Err(format!(
                        "built-in tool 'tool.web_search' providerTier must be automatic when backend is '{backend}'"
                    ));
                }
                if matches!(backend, "brave" | "xai" | "deepseek") && provider_tier == "free" {
                    return Err(format!(
                        "built-in tool 'tool.web_search' backend '{backend}' has no anonymous free tier"
                    ));
                }
                if self.credential_bindings.len() > 1
                    || self
                        .credential_bindings
                        .iter()
                        .any(|binding| binding.name != "api_key")
                {
                    return Err(
                        "built-in tool 'tool.web_search' accepts at most one credential binding named 'api_key'"
                            .into(),
                    );
                }
                let dual_tier_backend = matches!(
                    backend,
                    "exa" | "parallel" | "firecrawl" | "tavily" | "keenable"
                );
                let requires_key = matches!(backend, "brave" | "xai" | "deepseek")
                    || (dual_tier_backend && provider_tier == "paid");
                let forbids_key = matches!(backend, "keyless" | "duckduckgo" | "searxng")
                    || (dual_tier_backend && provider_tier == "free");
                if requires_key && self.credential_bindings.len() != 1 {
                    return Err(format!(
                        "built-in tool 'tool.web_search' requires one api_key credential binding for backend '{backend}'"
                    ));
                }
                if forbids_key && !self.credential_bindings.is_empty() {
                    return Err(format!(
                        "built-in tool 'tool.web_search' backend '{backend}' does not consume an api_key credential at the selected tier"
                    ));
                }
                Ok(())
            }
            "tool.web_fetch" => {
                require_exact_config_keys(self, &["maximumDownloadBytes", "maximumExtractBytes"])?;
                require_tool_project_scope(self, false)?;
                require_config_u64(
                    self,
                    "maximumDownloadBytes",
                    1,
                    WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1,
                )?;
                require_config_u64(
                    self,
                    "maximumExtractBytes",
                    1,
                    WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1,
                )
            }
            "tool.subagent" => {
                require_exact_config_keys(self, &["authorityMode", "requiresApproval"])?;
                require_tool_project_scope(self, false)?;
                require_config_string(self, "authorityMode", "run_subagent")?;
                require_config_bool(self, "requiresApproval", true)
            }
            _ => Err(format!("built-in tool '{}' is not implemented", self.id)),
        }
    }
}

/// Lifecycle state recorded for a discovered or installed extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionStatusV2 {
    Discovered,
    Installed,
    Incompatible,
}

/// One trusted-extension record; discovery alone never enables it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionConfigurationV2 {
    pub id: String,
    pub name: String,
    pub version: String,
    pub status: ExtensionStatusV2,
    pub enabled: bool,
    pub trust_accepted: bool,
    pub manifest_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_point: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    pub configuration: BTreeMap<String, Value>,
}

impl ExtensionConfigurationV2 {
    fn validate(&self) -> Result<(), String> {
        validate_stable_id("extension id", &self.id)?;
        validate_label("extension name", &self.name)?;
        validate_nonempty("extension version", &self.version)?;
        validate_nonempty("extension manifest path", &self.manifest_path)?;
        if self.enabled && (self.status != ExtensionStatusV2::Installed || !self.trust_accepted) {
            return Err(format!(
                "extension '{}' has invalid enabled legacy metadata: verified installation and explicit trust metadata are required, and this build does not provide extension enablement or execution",
                self.id
            ));
        }
        if self.trust_accepted && self.status != ExtensionStatusV2::Installed {
            return Err(format!(
                "extension '{}' can accept trust only after verified installation",
                self.id
            ));
        }
        if let Some(entry_point) = self.entry_point.as_deref() {
            validate_nonempty("extension entry point", entry_point)?;
        }
        if let Some(hash) = self.content_hash.as_deref() {
            validate_sha256("extension content hash", hash)?;
        }
        validate_optional_text("extension compatibility", self.compatibility.as_deref())?;
        validate_optional_text("extension provenance", self.provenance.as_deref())?;
        validate_freeform(
            "extension configuration",
            &Value::Object(to_json_map(&self.configuration)),
        )
    }
}

/// Protects core-owned extension lifecycle facts from the generic full-document
/// Settings command. Discovery may add inert records, but only dedicated native
/// commands may register or remove an installed identity.
pub(crate) fn validate_extension_lifecycle_update(
    previous: &SettingsConfigurationV2,
    next: &SettingsConfigurationV2,
) -> Result<(), String> {
    let previous_by_id = previous
        .extensions
        .iter()
        .map(|extension| (extension.id.as_str(), extension))
        .collect::<BTreeMap<_, _>>();
    let next_by_id = next
        .extensions
        .iter()
        .map(|extension| (extension.id.as_str(), extension))
        .collect::<BTreeMap<_, _>>();

    for extension in &next.extensions {
        let Some(prior) = previous_by_id.get(extension.id.as_str()) else {
            if extension.status == ExtensionStatusV2::Installed {
                return Err(format!(
                    "extension '{}' cannot fabricate installation through generic Settings; discover and register it with the dedicated command",
                    extension.id
                ));
            }
            continue;
        };
        if prior.status != extension.status {
            return Err(format!(
                "extension '{}' lifecycle status can change only through a dedicated extension command",
                extension.id
            ));
        }
        if !prior.enabled && extension.enabled {
            return Err(format!(
                "extension '{}' cannot be enabled through generic Settings; extension enablement and execution are unavailable in this build",
                extension.id
            ));
        }
        if prior.version != extension.version
            || prior.manifest_path != extension.manifest_path
            || prior.entry_point != extension.entry_point
            || prior.content_hash != extension.content_hash
            || prior.compatibility != extension.compatibility
            || prior.provenance != extension.provenance
            || protected_extension_facts(prior) != protected_extension_facts(extension)
        {
            return Err(format!(
                "extension '{}' immutable discovery/installation facts can change only after a fresh native inspection",
                extension.id
            ));
        }
    }

    for extension in &previous.extensions {
        if extension.status == ExtensionStatusV2::Installed
            && !next_by_id.contains_key(extension.id.as_str())
        {
            return Err(format!(
                "installed extension '{}' cannot be removed through generic Settings",
                extension.id
            ));
        }
    }
    Ok(())
}

/// Prevents the generic full-document Settings command from claiming that an
/// executor is active when this build has no execution path for it. Existing
/// `true` values are treated as legacy metadata: they remain round-trippable
/// and may be cleared, but generic Settings cannot create a new enablement.
pub(crate) fn validate_unavailable_executor_enablement_update(
    previous: &SettingsConfigurationV2,
    next: &SettingsConfigurationV2,
) -> Result<(), String> {
    let previous_mcp_enabled = previous
        .mcp_servers
        .iter()
        .map(|server| (server.id.as_str(), server.enabled))
        .collect::<BTreeMap<_, _>>();
    for server in &next.mcp_servers {
        if server.enabled
            && !previous_mcp_enabled
                .get(server.id.as_str())
                .copied()
                .unwrap_or(false)
        {
            return Err(format!(
                "MCP server '{}' cannot be enabled through generic Settings; MCP execution is unavailable in this build",
                server.id
            ));
        }
    }

    let previous_agent_enabled = previous
        .external_agents
        .iter()
        .map(|agent| (agent.id.as_str(), agent.enabled))
        .collect::<BTreeMap<_, _>>();
    for agent in &next.external_agents {
        if agent.enabled
            && !previous_agent_enabled
                .get(agent.id.as_str())
                .copied()
                .unwrap_or(false)
        {
            return Err(format!(
                "external agent '{}' cannot be enabled through generic Settings; external-agent execution is unavailable in this build",
                agent.id
            ));
        }
    }

    // All eleven built-in tools have installed v1 executors (W4); enabling a
    // tool through generic Settings is supported. MCP servers and external
    // agents remain gated until their chat execution paths ship.
    let _previous_tool_enabled = previous
        .tools
        .iter()
        .map(|tool| (tool.id.as_str(), tool.enabled))
        .collect::<BTreeMap<_, _>>();

    Ok(())
}

fn protected_extension_facts(extension: &ExtensionConfigurationV2) -> BTreeMap<&str, &Value> {
    const PROTECTED_KEYS: [&str; 12] = [
        "aworkitVersionRequirement",
        "contentHashScope",
        "contributionCount",
        "dependencyCount",
        "entryPointContentHash",
        "entryPointIdentity",
        "inspectionMode",
        "installationState",
        "installedForAworkitVersion",
        "integrityState",
        "manifestContentHash",
        "pluginProtocolVersion",
    ];
    PROTECTED_KEYS
        .into_iter()
        .filter_map(|key| extension.configuration.get(key).map(|value| (key, value)))
        .collect()
}

/// One configured MCP server.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerConfigurationV2 {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub auto_connect: bool,
    pub transport: IntegrationTransportV2,
}

impl McpServerConfigurationV2 {
    fn validate(&self, credentials: &BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
        validate_stable_id("MCP server id", &self.id)?;
        validate_label("MCP server name", &self.name)?;
        if self.auto_connect {
            return Err(format!(
                "MCP server '{}' cannot connect at launch because this build implements only explicit one-shot Discover and Test sessions",
                self.id
            ));
        }
        self.transport
            .validate(&format!("MCP server '{}'", self.id), credentials)
    }
}

/// Secret-safe HTTP or standard-I/O connection configuration.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrationTransportV2 {
    Http {
        url: String,
        headers: Vec<NamedCredentialBindingV2>,
    },
    Stdio {
        command: String,
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        env: Vec<NamedCredentialBindingV2>,
    },
}

impl IntegrationTransportV2 {
    fn validate(
        &self,
        owner: &str,
        credentials: &BTreeMap<String, BTreeSet<String>>,
    ) -> Result<(), String> {
        match self {
            Self::Http { url, headers } => {
                validate_http_url(&format!("{owner} URL"), url)?;
                validate_bindings(owner, headers, credentials)
            }
            Self::Stdio {
                command,
                args,
                cwd,
                env,
            } => {
                validate_nonempty(&format!("{owner} command"), command)?;
                if args.len() > MAX_ARGUMENTS {
                    return Err(format!(
                        "{owner} exceeds the {MAX_ARGUMENTS} argument limit"
                    ));
                }
                for argument in args {
                    validate_secret_free_stdio_argument(&format!("{owner} argument"), argument)?;
                }
                validate_optional_text(&format!("{owner} working directory"), cwd.as_deref())?;
                validate_bindings(owner, env, credentials)
            }
        }
    }
}

fn transport_bindings(transport: &IntegrationTransportV2) -> &[NamedCredentialBindingV2] {
    match transport {
        IntegrationTransportV2::Http { headers, .. } => headers,
        IntegrationTransportV2::Stdio { env, .. } => env,
    }
}

fn validate_mcp_transport_targets(
    server_id: &str,
    transport: &IntegrationTransportV2,
) -> Result<(), String> {
    let owner = format!("MCP server '{server_id}'");
    match transport {
        IntegrationTransportV2::Http { headers, .. } => {
            validate_http_credential_targets(&owner, headers, true)
        }
        IntegrationTransportV2::Stdio { command, env, .. } => {
            if !launchable_mcp_command(Path::new(unquote_runtime_path(command))) {
                return Err(format!(
                    "{owner} STDIO executable must be absolute or one bare command name from PATH for the installed MCP adapter"
                ));
            }
            validate_environment_credential_targets(&owner, env.iter())
        }
    }
}

/// A quoted executable path is a common, harmless Windows shell copy/paste.
fn unquote_runtime_path(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && matches!(trimmed.as_bytes()[0], b'\'' | b'"')
        && trimmed.as_bytes()[0] == *trimmed.as_bytes().last().expect("length checked")
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn launchable_mcp_command(path: &Path) -> bool {
    path.is_absolute()
        || path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)))
}

fn validate_external_agent_transport_targets(
    agent: &ExternalAgentConfigurationV2,
) -> Result<(), String> {
    let owner = format!("external agent '{}'", agent.id);
    match &agent.connection {
        IntegrationTransportV2::Http { headers, .. } => {
            validate_http_credential_targets(&owner, headers, false)?;
            validate_environment_credential_targets(&owner, agent.credential_bindings.iter())
        }
        IntegrationTransportV2::Stdio {
            command,
            args,
            cwd,
            env,
        } => {
            let binding_count = env
                .len()
                .checked_add(agent.credential_bindings.len())
                .ok_or_else(|| format!("{owner} environment binding count overflowed"))?;
            if binding_count > MAX_BINDINGS {
                return Err(format!(
                    "{owner} environment exceeds the {MAX_BINDINGS}-binding limit across connection.env and credentialBindings"
                ));
            }
            validate_environment_credential_targets(
                &owner,
                env.iter().chain(agent.credential_bindings.iter()),
            )?;
            if agent.adapter == "codex_app_server" {
                validate_codex_app_server_stdio_contract(&owner, command, args, cwd.as_deref())?;
            }
            Ok(())
        }
    }
}

fn validate_codex_app_server_stdio_contract(
    owner: &str,
    command: &str,
    args: &[String],
    cwd: Option<&str>,
) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("app-server") {
        return Err(format!(
            "{owner} arguments must begin with the explicit 'app-server' subcommand"
        ));
    }
    if uses_non_stdio_listener(args) {
        return Err(format!(
            "{owner} supports only --listen stdio or --listen stdio://"
        ));
    }
    let path = Path::new(command);
    let mut components = path.components();
    let bare_path_command =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !path.is_absolute() && !bare_path_command {
        return Err(format!(
            "{owner} executable must be absolute or one bare command name from PATH"
        ));
    }
    if let Some(cwd) = cwd
        && !Path::new(cwd).is_absolute()
    {
        return Err(format!(
            "{owner} working directory must be absolute when configured"
        ));
    }
    Ok(())
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

fn validate_http_credential_targets(
    owner: &str,
    bindings: &[NamedCredentialBindingV2],
    reject_mcp_reserved_headers: bool,
) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for binding in bindings {
        if !valid_http_credential_target(&binding.name) {
            return Err(format!(
                "{owner} HTTP credential target '{}' must be at most 128 ASCII letters, digits, hyphens, or underscores",
                binding.name
            ));
        }
        if reject_mcp_reserved_headers && reserved_mcp_header_name(&binding.name) {
            return Err(format!(
                "{owner} HTTP credential target '{}' is reserved by the native MCP transport",
                binding.name
            ));
        }
        if !names.insert(binding.name.to_ascii_lowercase()) {
            return Err(format!(
                "{owner} HTTP credential target '{}' is configured more than once (header names are case-insensitive)",
                binding.name
            ));
        }
    }
    Ok(())
}

fn validate_environment_credential_targets<'a>(
    owner: &str,
    bindings: impl IntoIterator<Item = &'a NamedCredentialBindingV2>,
) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for binding in bindings {
        if !valid_environment_credential_target(&binding.name) {
            return Err(format!(
                "{owner} environment credential target '{}' must be at most 128 ASCII letters, digits, or underscores",
                binding.name
            ));
        }
        if !names.insert(binding.name.to_ascii_lowercase()) {
            return Err(format!(
                "{owner} environment credential target '{}' is configured more than once (names are compared case-insensitively for cross-platform portability)",
                binding.name
            ));
        }
    }
    Ok(())
}

fn valid_http_credential_target(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_environment_credential_target(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn reserved_mcp_header_name(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "accept"
            | "connection"
            | "content-length"
            | "content-type"
            | "expect"
            | "host"
            | "proxy-authorization"
            | "transfer-encoding"
            | "upgrade"
            | "mcp-session-id"
            | "mcp-protocol-version"
            | "last-event-id"
            | "mcp-method"
            | "mcp-name"
    ) || name.starts_with("mcp-param-")
}

/// Lifecycle features honestly advertised by an external-agent adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentCapabilitiesV2 {
    pub progress: bool,
    pub continuation: bool,
    pub cancellation: bool,
    pub approvals: bool,
}

/// One configured external-agent target such as Codex App Server or ACP.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentConfigurationV2 {
    pub id: String,
    pub name: String,
    pub adapter: String,
    pub enabled: bool,
    pub connection: IntegrationTransportV2,
    pub credential_bindings: Vec<NamedCredentialBindingV2>,
    pub mcp_server_ids: Vec<String>,
    pub capabilities: ExternalAgentCapabilitiesV2,
    pub configuration: BTreeMap<String, Value>,
}

impl ExternalAgentConfigurationV2 {
    fn validate(
        &self,
        credentials: &BTreeMap<String, BTreeSet<String>>,
        mcp_servers: &BTreeSet<String>,
    ) -> Result<(), String> {
        validate_stable_id("external-agent id", &self.id)?;
        validate_label("external-agent name", &self.name)?;
        validate_symbol("external-agent adapter", &self.adapter)?;
        self.connection
            .validate(&format!("external agent '{}'", self.id), credentials)?;
        validate_bindings(
            &format!("external agent '{}'", self.id),
            &self.credential_bindings,
            credentials,
        )?;
        let ids = unique_ids(
            "external-agent MCP server reference",
            self.mcp_server_ids.iter().map(String::as_str),
        )?;
        for id in ids {
            if !mcp_servers.contains(&id) {
                return Err(format!(
                    "external agent '{}' references unknown MCP server '{}'",
                    self.id, id
                ));
            }
        }
        validate_freeform(
            "external-agent configuration",
            &Value::Object(to_json_map(&self.configuration)),
        )
    }
}

/// Local and Git-portable history behavior.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataConfigurationV2 {
    pub portable_history_enabled: bool,
    pub detailed_capture_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detailed_capture_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_history_retention_days: Option<u32>,
    pub portable_directory: String,
}

impl Default for DataConfigurationV2 {
    fn default() -> Self {
        Self {
            portable_history_enabled: false,
            detailed_capture_enabled: false,
            detailed_capture_retention_days: None,
            local_history_retention_days: None,
            portable_directory: ".aworkit/sessions".into(),
        }
    }
}

impl DataConfigurationV2 {
    fn validate(&self) -> Result<(), String> {
        if self.portable_history_enabled {
            return Err(
                "portable Chat history is not composed in this build; local SQLite remains the only active history backend"
                    .into(),
            );
        }
        if self.detailed_capture_enabled
            || self.detailed_capture_retention_days.is_some()
            || self.local_history_retention_days.is_some()
        {
            return Err(
                "detailed capture and retention policies are not composed in this build and must remain disabled"
                    .into(),
            );
        }
        validate_nonempty("portable session directory", &self.portable_directory)?;
        if self.portable_directory.starts_with('/')
            || self.portable_directory.starts_with('\\')
            || self.portable_directory.split('/').any(|part| part == "..")
        {
            return Err("portable session directory must be a project-relative path".into());
        }
        Ok(())
    }
}

/// Supported project workspace categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKindV2 {
    LocalDirectory,
    GitWorktree,
    ContainerMount,
    Remote,
}

/// Resolved project workspace location.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceConfigurationV2 {
    pub kind: WorkspaceKindV2,
    pub location: String,
}

/// One selectable Aworkit project and workspace.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectConfigurationV2 {
    pub id: String,
    pub name: String,
    pub workspace: WorkspaceConfigurationV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workflow_id: Option<String>,
    pub portable_history_enabled: bool,
}

impl ProjectConfigurationV2 {
    fn validate(&self) -> Result<(), String> {
        validate_stable_id("project id", &self.id)?;
        validate_label("project name", &self.name)?;
        validate_nonempty("project workspace location", &self.workspace.location)?;
        validate_optional_stable_id("default workflow id", self.default_workflow_id.as_deref())?;
        if self.portable_history_enabled {
            return Err(format!(
                "project '{}' cannot enable portable history because this build uses only local SQLite Chat history",
                self.id
            ));
        }
        Ok(())
    }
}

/// Desktop color mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceModeV2 {
    #[default]
    System,
    Light,
    Dark,
}

/// Desktop appearance settings.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppearanceConfigurationV2 {
    pub mode: AppearanceModeV2,
    pub font_scale: f64,
}

impl Default for AppearanceConfigurationV2 {
    fn default() -> Self {
        Self {
            mode: AppearanceModeV2::System,
            font_scale: 1.0,
        }
    }
}

impl AppearanceConfigurationV2 {
    fn validate(&self) -> Result<(), String> {
        if !self.font_scale.is_finite() || !(0.75..=2.0).contains(&self.font_scale) {
            return Err("appearance fontScale must be between 0.75 and 2.0".into());
        }
        Ok(())
    }
}

pub(crate) fn standard_model_tiers() -> Vec<ModelTierConfigurationV2> {
    STANDARD_TIERS
        .iter()
        .map(|(id, name)| ModelTierConfigurationV2 {
            id: (*id).into(),
            name: (*name).into(),
            kind: ModelTierKindV2::Standard,
            resolution: ModelTierResolutionV2::Unconfigured,
        })
        .collect()
}

fn default_builtin_tools() -> Vec<BuiltInToolConfigurationV2> {
    vec![
        builtin_tool(
            "tool.files.read",
            "Project file read",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("read")),
                (
                    "maximumBytes".into(),
                    Value::from(PROJECT_FILE_READ_MAXIMUM_BYTES_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.files.search",
            "Project file search",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("search")),
                (
                    "maximumResults".into(),
                    Value::from(PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.files.edit",
            "Project file edit",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("write")),
                ("requiresApproval".into(), Value::Bool(true)),
                ("maximumBytes".into(), Value::from(1_048_576_u64)),
            ]),
        ),
        builtin_tool(
            "tool.files.list",
            "Project file list (glob)",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("list")),
                (
                    "maximumEntries".into(),
                    Value::from(crate::runtime::PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.files.grep",
            "Project file regex search",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("grep")),
                (
                    "maximumMatches".into(),
                    Value::from(crate::runtime::PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.files.write",
            "Project file write",
            true,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("project_files")),
                ("effect".into(), Value::from("write")),
                ("requiresApproval".into(), Value::Bool(true)),
                (
                    "maximumBytes".into(),
                    Value::from(crate::runtime::PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.shell.host",
            "Host shell",
            false,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("host_shell")),
                ("requiresApproval".into(), Value::Bool(true)),
                ("timeoutSeconds".into(), Value::from(30_u64)),
                ("maximumOutputBytes".into(), Value::from(262_144_u64)),
            ]),
        ),
        builtin_tool(
            "tool.python.host",
            "Host Python",
            false,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("host_python")),
                ("requiresApproval".into(), Value::Bool(true)),
                ("isolatedInterpreter".into(), Value::Bool(true)),
                ("timeoutSeconds".into(), Value::from(30_u64)),
                ("maximumOutputBytes".into(), Value::from(262_144_u64)),
            ]),
        ),
        builtin_tool(
            "tool.todo",
            "Run task list",
            false,
            BTreeMap::from([("authorityMode".into(), Value::from("run_todo"))]),
        ),
        builtin_tool(
            "tool.web_search",
            "Web search",
            false,
            web_search_default_configuration(),
        ),
        builtin_tool(
            "tool.web_fetch",
            "Web page fetch",
            false,
            BTreeMap::from([
                (
                    "maximumDownloadBytes".into(),
                    Value::from(crate::runtime::WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1),
                ),
                (
                    "maximumExtractBytes".into(),
                    Value::from(crate::runtime::WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1),
                ),
            ]),
        ),
        builtin_tool(
            "tool.subagent",
            "Subagent delegation",
            false,
            BTreeMap::from([
                ("authorityMode".into(), Value::from("run_subagent")),
                ("requiresApproval".into(), Value::Bool(true)),
            ]),
        ),
    ]
}

pub(crate) fn web_search_default_configuration() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("backend".into(), Value::from("automatic")),
        ("credentialBackend".into(), Value::from("deepseek")),
        ("providerTier".into(), Value::from("automatic")),
        ("maximumResults".into(), Value::from(10_u64)),
        ("requestTimeoutSeconds".into(), Value::from(30_u64)),
        ("maximumRetries".into(), Value::from(1_u64)),
        ("keylessFallback".into(), Value::Bool(true)),
        ("keylessRescue".into(), Value::Bool(true)),
        ("cacheEnabled".into(), Value::Bool(true)),
        ("cacheTtlMinutes".into(), Value::from(20_u64)),
        ("searxngBaseUrl".into(), Value::from("")),
        ("providerBaseUrl".into(), Value::from("")),
        ("parallelSearchMode".into(), Value::from("agentic")),
        ("xaiModel".into(), Value::from("grok-build-0.1")),
        ("xaiAllowedDomains".into(), Value::Array(Vec::new())),
        ("xaiExcludedDomains".into(), Value::Array(Vec::new())),
        (
            "deepseekBaseUrl".into(),
            Value::from("https://api.deepseek.com"),
        ),
        ("deepseekModel".into(), Value::from("deepseek-v4-flash")),
        ("deepseekMaximumOutputTokens".into(), Value::from(4_096_u64)),
    ])
}

fn builtin_tool(
    id: &str,
    name: &str,
    requires_project: bool,
    configuration: BTreeMap<String, Value>,
) -> BuiltInToolConfigurationV2 {
    BuiltInToolConfigurationV2 {
        id: id.into(),
        name: name.into(),
        enabled: false,
        requires_project,
        credential_bindings: Vec::new(),
        configuration,
    }
}

fn validate_tiers(
    tiers: &[ModelTierConfigurationV2],
    models: &BTreeSet<(String, String)>,
) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for tier in tiers {
        validate_tier_id(&tier.id)?;
        validate_label("model tier name", &tier.name)?;
        if !ids.insert(tier.id.clone()) {
            return Err(format!("duplicate model tier id '{}'", tier.id));
        }
        let is_standard = STANDARD_TIERS.iter().any(|(id, _)| *id == tier.id);
        if is_standard != (tier.kind == ModelTierKindV2::Standard) {
            return Err(format!(
                "model tier '{}' has the wrong standard/custom kind",
                tier.id
            ));
        }
        match &tier.resolution {
            ModelTierResolutionV2::Unconfigured => {}
            ModelTierResolutionV2::Exact { target } => {
                validate_model_target(&tier.id, target, models)?;
            }
            ModelTierResolutionV2::Fallback { targets } => {
                validate_tier_targets(&tier.id, targets, models)?;
                if targets.len() < 2 {
                    return Err(format!(
                        "model tier '{}' fallback strategy requires at least two targets",
                        tier.id
                    ));
                }
            }
            ModelTierResolutionV2::Policy { candidates, .. } => {
                validate_tier_targets(&tier.id, candidates, models)?;
                if candidates.is_empty() {
                    return Err(format!(
                        "model tier '{}' policy requires at least one candidate",
                        tier.id
                    ));
                }
            }
        }
    }
    for (standard, _) in STANDARD_TIERS {
        if !ids.contains(standard) {
            return Err(format!(
                "standard model tier '{standard}' must always be present"
            ));
        }
    }
    Ok(())
}

fn validate_tier_targets(
    tier_id: &str,
    targets: &[ModelTargetV2],
    models: &BTreeSet<(String, String)>,
) -> Result<(), String> {
    if targets.len() > MAX_COLLECTION_ITEMS {
        return Err(format!("model tier '{tier_id}' has too many targets"));
    }
    let mut seen = BTreeSet::new();
    for target in targets {
        validate_model_target(tier_id, target, models)?;
        if !seen.insert(target.clone()) {
            return Err(format!(
                "model tier '{tier_id}' contains duplicate target '{}:{}'",
                target.provider_id, target.model_id
            ));
        }
    }
    Ok(())
}

fn validate_model_target(
    tier_id: &str,
    target: &ModelTargetV2,
    models: &BTreeSet<(String, String)>,
) -> Result<(), String> {
    validate_stable_id("model target provider id", &target.provider_id)?;
    validate_stable_id("model target model id", &target.model_id)?;
    if !models.contains(&(target.provider_id.clone(), target.model_id.clone())) {
        return Err(format!(
            "model tier '{tier_id}' references unknown model '{}:{}'",
            target.provider_id, target.model_id
        ));
    }
    Ok(())
}

fn validate_bindings(
    owner: &str,
    bindings: &[NamedCredentialBindingV2],
    credentials: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(), String> {
    if bindings.len() > MAX_BINDINGS {
        return Err(format!(
            "{owner} exceeds the {MAX_BINDINGS} credential-binding limit"
        ));
    }
    let mut names = BTreeSet::new();
    for binding in bindings {
        binding.validate(owner, credentials)?;
        if !names.insert(binding.name.clone()) {
            return Err(format!(
                "{owner} contains duplicate credential binding name '{}'",
                binding.name
            ));
        }
    }
    Ok(())
}

fn require_tool_project_scope(
    tool: &BuiltInToolConfigurationV2,
    expected: bool,
) -> Result<(), String> {
    if tool.requires_project == expected {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' requiresProject must be {expected}",
            tool.id
        ))
    }
}

fn require_exact_config_keys(
    tool: &BuiltInToolConfigurationV2,
    expected: &[&str],
) -> Result<(), String> {
    let actual = tool
        .configuration
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration must contain exactly the installed adapter fields",
            tool.id
        ))
    }
}

fn require_config_string(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    expected: &str,
) -> Result<(), String> {
    if tool.configuration.get(field).and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must be '{expected}'",
            tool.id
        ))
    }
}

fn require_config_one_of<'a>(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    expected: &'a [&str],
) -> Result<&'a str, String> {
    let Some(value) = tool.configuration.get(field).and_then(Value::as_str) else {
        return Err(format!(
            "built-in tool '{}' configuration.{field} must be one of {}",
            tool.id,
            expected.join(", ")
        ));
    };
    expected
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .ok_or_else(|| {
            format!(
                "built-in tool '{}' configuration.{field} must be one of {}",
                tool.id,
                expected.join(", ")
            )
        })
}

fn require_config_boolean(tool: &BuiltInToolConfigurationV2, field: &str) -> Result<(), String> {
    if tool.configuration.get(field).is_some_and(Value::is_boolean) {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must be a boolean",
            tool.id
        ))
    }
}

fn require_config_text(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), String> {
    if tool
        .configuration
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| {
            value.trim().len() >= minimum
                && value.len() <= maximum
                && !value.contains(['\0', '\r', '\n'])
        })
    {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must contain {minimum} through {maximum} safe text bytes",
            tool.id
        ))
    }
}

fn require_optional_search_url(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    allow_loopback_http: bool,
) -> Result<(), String> {
    let value = tool
        .configuration
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "built-in tool '{}' configuration.{field} must be a URL string",
                tool.id
            )
        })?;
    if value.trim().is_empty() {
        return Ok(());
    }
    validate_http_url(field, value)?;
    let url = Url::parse(value).map_err(|_| format!("{field} is invalid"))?;
    let loopback = url
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
    if url.scheme() == "https" || (allow_loopback_http && url.scheme() == "http" && loopback) {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must use HTTPS{}",
            tool.id,
            if allow_loopback_http {
                " or loopback HTTP"
            } else {
                ""
            }
        ))
    }
}

fn require_search_domain_filters(tool: &BuiltInToolConfigurationV2) -> Result<(), String> {
    fn read<'a>(tool: &'a BuiltInToolConfigurationV2, field: &str) -> Result<Vec<&'a str>, String> {
        let values = tool
            .configuration
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!(
                    "built-in tool '{}' configuration.{field} must be an array",
                    tool.id
                )
            })?;
        if values.len() > 5 {
            return Err(format!(
                "built-in tool '{}' configuration.{field} accepts at most five domains",
                tool.id
            ));
        }
        values
            .iter()
            .map(|value| {
                let domain = value.as_str().ok_or_else(|| {
                    format!(
                        "built-in tool '{}' configuration.{field} entries must be strings",
                        tool.id
                    )
                })?;
                let safe = !domain.is_empty()
                    && domain.len() <= 253
                    && domain.split('.').all(|label| {
                        !label.is_empty()
                            && label.len() <= 63
                            && !label.starts_with('-')
                            && !label.ends_with('-')
                            && label
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    });
                safe.then_some(domain).ok_or_else(|| {
                    format!(
                        "built-in tool '{}' configuration.{field} contains an invalid domain",
                        tool.id
                    )
                })
            })
            .collect()
    }

    let allowed = read(tool, "xaiAllowedDomains")?;
    let excluded = read(tool, "xaiExcludedDomains")?;
    if !allowed.is_empty() && !excluded.is_empty() {
        return Err(
            "built-in tool 'tool.web_search' xaiAllowedDomains and xaiExcludedDomains are mutually exclusive"
                .into(),
        );
    }
    Ok(())
}

fn require_config_bool(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    expected: bool,
) -> Result<(), String> {
    if tool.configuration.get(field).and_then(Value::as_bool) == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must be {expected}",
            tool.id
        ))
    }
}

fn require_config_u64(
    tool: &BuiltInToolConfigurationV2,
    field: &str,
    minimum: u64,
    maximum: u64,
) -> Result<(), String> {
    if tool
        .configuration
        .get(field)
        .and_then(Value::as_u64)
        .is_some_and(|value| (minimum..=maximum).contains(&value))
    {
        Ok(())
    } else {
        Err(format!(
            "built-in tool '{}' configuration.{field} must be an integer from {minimum} through {maximum}",
            tool.id
        ))
    }
}

fn validate_collection_size(label: &str, size: usize) -> Result<(), String> {
    if size <= MAX_COLLECTION_ITEMS {
        Ok(())
    } else {
        Err(format!(
            "settings {label} exceeds the {MAX_COLLECTION_ITEMS} item limit"
        ))
    }
}

fn unique_ids<'a>(
    label: &str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<BTreeSet<String>, String> {
    let mut unique = BTreeSet::new();
    for id in ids {
        if label == "credential reference" {
            validate_credential_ref(id)?;
        } else {
            validate_stable_id(&format!("{label} id"), id)?;
        }
        if !unique.insert(id.to_owned()) {
            return Err(format!("duplicate {label} '{id}'"));
        }
    }
    Ok(unique)
}

fn unique_symbols(label: &str, values: &[String]) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_symbol(label, value)?;
        if !unique.insert(value) {
            return Err(format!("duplicate {label} '{value}'"));
        }
    }
    Ok(())
}

fn validate_stable_id(label: &str, value: &str) -> Result<(), String> {
    StableId::parse(value.to_owned())
        .map(|_| ())
        .map_err(|_| format!("{label} '{value}' is not a valid stable identifier"))
}

fn validate_optional_stable_id(label: &str, value: Option<&str>) -> Result<(), String> {
    value.map_or(Ok(()), |value| validate_stable_id(label, value))
}

fn validate_credential_ref(value: &str) -> Result<(), String> {
    validate_stable_id("credential reference", value)?;
    if value.starts_with("credential.") {
        Ok(())
    } else {
        Err(format!(
            "credential reference '{value}' must use the credential. namespace"
        ))
    }
}

fn validate_tier_id(value: &str) -> Result<(), String> {
    let Some(suffix) = value.strip_prefix("tier:") else {
        return Err(format!("model tier id '{value}' must start with tier:"));
    };
    if suffix.is_empty()
        || suffix.len() > 96
        || !suffix.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return Err(format!("model tier id '{value}' is invalid"));
    }
    Ok(())
}

fn validate_symbol(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
    {
        Err(format!("{label} '{value}' is invalid"))
    } else {
        Ok(())
    }
}

fn validate_label(label: &str, value: &str) -> Result<(), String> {
    validate_nonempty(label, value)
}

fn validate_nonempty(label: &str, value: &str) -> Result<(), String> {
    validate_bounded_text(label, value)?;
    if value.trim().is_empty() {
        Err(format!("{label} cannot be empty"))
    } else {
        Ok(())
    }
}

fn validate_optional_text(label: &str, value: Option<&str>) -> Result<(), String> {
    value.map_or(Ok(()), |value| validate_nonempty(label, value))
}

fn validate_bounded_text(label: &str, value: &str) -> Result<(), String> {
    if value.len() > MAX_STRING_BYTES {
        return Err(format!("{label} exceeds the {MAX_STRING_BYTES}-byte limit"));
    }
    if value.contains('\0') {
        return Err(format!("{label} contains NUL"));
    }
    Ok(())
}

pub(crate) fn validate_http_url(label: &str, value: &str) -> Result<(), String> {
    validate_nonempty(label, value)?;
    let url = Url::parse(value).map_err(|_| {
        format!("{label} must be an absolute HTTP(S) URL without credentials, query, or fragment")
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.cannot_be_a_base()
    {
        return Err(format!(
            "{label} must be an absolute HTTP(S) URL without credentials, query, or fragment"
        ));
    }
    Ok(())
}

/// Rejects command-line secret material. STDIO integrations receive secrets
/// only through named credential-backed environment bindings.
pub(crate) fn validate_secret_free_stdio_argument(label: &str, value: &str) -> Result<(), String> {
    validate_bounded_text(label, value)?;
    let normalized = normalize_sensitive_name(value);
    let looks_like_secret_value = value.starts_with("sk-")
        || value.starts_with("ghp_")
        || value.starts_with("github_pat_")
        || value.to_ascii_lowercase().starts_with("bearer ");
    if secret_like_normalized(&normalized) || looks_like_secret_value {
        return Err(format!(
            "{label} appears to contain authentication or credential material; use a named credential-backed environment binding instead"
        ));
    }
    for candidate in [Some(value), value.split_once('=').map(|(_, suffix)| suffix)]
        .into_iter()
        .flatten()
    {
        if matches!(Url::parse(candidate), Ok(url) if matches!(url.scheme(), "http" | "https")) {
            validate_http_url(label, candidate)?;
        }
    }
    Ok(())
}

fn validate_sha256(label: &str, value: &str) -> Result<(), String> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(format!("{label} must start with sha256:"));
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} must contain a 64-character hex digest"));
    }
    Ok(())
}

fn validate_freeform(label: &str, value: &Value) -> Result<(), String> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| format!("cannot encode {label}: {error}"))?;
    if encoded.len() > MAX_FREEFORM_BYTES {
        return Err(format!(
            "{label} exceeds the {MAX_FREEFORM_BYTES}-byte limit"
        ));
    }
    validate_freeform_value(label, value, 0)
}

fn validate_freeform_value(label: &str, value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_FREEFORM_DEPTH {
        return Err(format!("{label} exceeds the maximum nesting depth"));
    }
    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                validate_bounded_text(&format!("{label} key"), key)?;
                if secret_like_key(key) {
                    return Err(format!(
                        "{label} contains secret-like field '{key}'; store the value in the credential store and reference it through credentialBindings"
                    ));
                }
                validate_freeform_value(label, nested, depth + 1)?;
            }
        }
        Value::Array(values) => {
            if values.len() > MAX_COLLECTION_ITEMS {
                return Err(format!("{label} contains an oversized array"));
            }
            for nested in values {
                validate_freeform_value(label, nested, depth + 1)?;
            }
        }
        Value::String(value) => validate_bounded_text(label, value)?,
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
    Ok(())
}

fn secret_like_key(key: &str) -> bool {
    secret_like_normalized(&normalize_sensitive_name(key))
}

fn normalize_sensitive_name(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn secret_like_normalized(normalized: &str) -> bool {
    let explicit_secret_markers = [
        "apikey",
        "accesstoken",
        "authtoken",
        "authorization",
        "authheader",
        "bearertoken",
        "clientsecret",
        "password",
        "passwd",
        "privatekey",
        "secret",
    ];
    explicit_secret_markers
        .iter()
        .any(|marker| normalized.contains(marker))
        || normalized == "token"
        || normalized.ends_with("tokenvalue")
        || normalized == "credential"
        || normalized.ends_with("credentials")
        || normalized.contains("credentialref")
}

fn to_json_map(values: &BTreeMap<String, Value>) -> serde_json::Map<String, Value> {
    values
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> SettingsConfigurationV2 {
        let mut settings = SettingsConfigurationV2::default();
        settings
            .credentials
            .push(CredentialMetadataConfigurationV2 {
                credential_ref: "credential.fixture".into(),
                label: "Fixture API key".into(),
                kind: "api_key".into(),
                field_names: vec!["api_key".into()],
                revision: 1,
                bound_provider_id: Some("provider.fixture".into()),
                bound_endpoint: Some("https://provider.example/v1".into()),
            });
        settings.providers.push(ProviderConfigurationV2 {
            id: "provider.fixture".into(),
            name: "Fixture".into(),
            kind: "openai_compatible".into(),
            base_url: "https://provider.example/v1".into(),
            enabled: true,
            credential_ref: Some("credential.fixture".into()),
            models: vec![ModelConfigurationV2 {
                id: "model.fixture".into(),
                name: "Fixture model".into(),
                remote_id: "fixture-model".into(),
                enabled: true,
                context_window: Some(32_768),
                max_output_tokens: Some(4_096),
                capabilities: vec!["text".into(), "tools".into()],
                parameters: BTreeMap::from([("reasoningEffort".into(), Value::from("medium"))]),
            }],
            configuration: BTreeMap::from([
                (
                    "requestTimeoutSeconds".into(),
                    Value::from(DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1),
                ),
                (
                    "maximumToolOutputBytes".into(),
                    Value::from(DEFAULT_MAXIMUM_TOOL_OUTPUT_BYTES_V1),
                ),
            ]),
        });
        settings.model_tiers[2].resolution = ModelTierResolutionV2::Exact {
            target: ModelTargetV2 {
                provider_id: "provider.fixture".into(),
                model_id: "model.fixture".into(),
            },
        };
        settings
    }

    fn add_integration_credential(settings: &mut SettingsConfigurationV2) {
        settings
            .credentials
            .push(CredentialMetadataConfigurationV2 {
                credential_ref: "credential.integration".into(),
                label: "Integration token".into(),
                kind: "token".into(),
                field_names: vec!["token".into()],
                revision: 1,
                bound_provider_id: None,
                bound_endpoint: None,
            });
    }

    fn integration_binding(name: &str) -> NamedCredentialBindingV2 {
        NamedCredentialBindingV2 {
            name: name.into(),
            credential_ref: "credential.integration".into(),
            field: "token".into(),
        }
    }

    fn codex_agent<I, S>(
        command: impl Into<String>,
        args: I,
        cwd: Option<String>,
    ) -> ExternalAgentConfigurationV2
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ExternalAgentConfigurationV2 {
            id: "agent.codex".into(),
            name: "Codex".into(),
            adapter: "codex_app_server".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: command.into(),
                args: args.into_iter().map(Into::into).collect(),
                cwd,
                env: Vec::new(),
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
        }
    }

    #[test]
    fn default_document_has_all_standard_tiers_and_is_valid() {
        let settings = SettingsConfigurationV2::default();
        settings.validate().unwrap();
        assert_eq!(
            settings
                .model_tiers
                .iter()
                .map(|tier| tier.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tier:fast", "tier:simple", "tier:balanced", "tier:quality"]
        );
        assert_eq!(
            settings
                .tools
                .iter()
                .map(|tool| tool.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "tool.files.read",
                "tool.files.search",
                "tool.files.edit",
                "tool.files.list",
                "tool.files.grep",
                "tool.files.write",
                "tool.shell.host",
                "tool.python.host",
                "tool.todo",
                "tool.web_search",
                "tool.web_fetch",
                "tool.subagent"
            ]
        );
        assert!(settings.tools.iter().all(|tool| !tool.enabled));
        assert!(
            settings
                .tools
                .iter()
                .all(|tool| tool.id != "tool.python.sandboxed")
        );
    }

    #[test]
    fn web_search_provider_tiers_enforce_exact_credential_contracts() {
        let mut settings = SettingsConfigurationV2::default();
        let web_search = settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
            .expect("web-search tool");
        web_search
            .configuration
            .insert("backend".into(), Value::from("exa"));
        web_search
            .configuration
            .insert("providerTier".into(), Value::from("paid"));
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("requires one api_key")
        );

        add_integration_credential(&mut settings);
        settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
            .expect("web-search tool")
            .credential_bindings = vec![integration_binding("api_key")];
        settings.validate().expect("paid Exa configuration");
        settings
            .validate_installed_runtime_consumers()
            .expect("installed paid Exa consumer");

        let web_search = settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
            .expect("web-search tool");
        web_search
            .configuration
            .insert("providerTier".into(), Value::from("free"));
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("does not consume an api_key")
        );
        settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
            .expect("web-search tool")
            .credential_bindings
            .clear();
        settings.validate().expect("anonymous Exa configuration");

        let web_search = settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.web_search")
            .expect("web-search tool");
        web_search
            .configuration
            .insert("backend".into(), Value::from("deepseek"));
        web_search
            .configuration
            .insert("providerTier".into(), Value::from("paid"));
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("requires one api_key")
        );
    }

    #[test]
    fn provider_runtime_limits_default_and_reject_unconsumed_fields() {
        let mut provider = configured().providers.remove(0);
        provider.configuration.clear();
        assert_eq!(
            provider.runtime_limits().unwrap(),
            ProviderRuntimeLimitsV1 {
                request_timeout_seconds: 300,
                maximum_tool_output_bytes: 65_536,
            }
        );

        provider
            .configuration
            .insert("requestTimeoutSeconds".into(), Value::from(600));
        provider
            .configuration
            .insert("maximumToolOutputBytes".into(), Value::from(4096));
        assert_eq!(
            provider.runtime_limits().unwrap().request_timeout_seconds,
            600
        );
        assert_eq!(
            provider.runtime_limits().unwrap().maximum_tool_output_bytes,
            4096
        );

        provider
            .configuration
            .insert("requestTimeoutSeconds".into(), Value::from(3_601));
        assert!(
            provider
                .runtime_limits()
                .unwrap_err()
                .contains("1 through 3600")
        );
        provider
            .configuration
            .insert("requestTimeoutSeconds".into(), Value::from(600));

        provider
            .configuration
            .insert("ignoredByRuntime".into(), Value::Bool(true));
        assert!(
            provider
                .runtime_limits()
                .unwrap_err()
                .contains("unsupported")
        );
    }

    #[test]
    fn complete_multi_section_document_is_valid() {
        let mut settings = configured();
        let binding = NamedCredentialBindingV2 {
            name: "AUTHORIZATION".into(),
            credential_ref: "credential.fixture".into(),
            field: "api_key".into(),
        };
        settings.tools[3].enabled = true;
        settings.tools[3].credential_bindings = vec![binding.clone()];
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.fixture".into(),
            name: "Fixture MCP".into(),
            enabled: true,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: vec![binding],
            },
        });
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.codex".into(),
            name: "Codex".into(),
            adapter: "codex_app_server".into(),
            enabled: true,
            connection: IntegrationTransportV2::Stdio {
                command: "codex".into(),
                args: vec!["app-server".into()],
                cwd: None,
                env: Vec::new(),
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: vec!["mcp.fixture".into()],
            capabilities: ExternalAgentCapabilitiesV2 {
                progress: true,
                continuation: true,
                cancellation: true,
                approvals: true,
            },
            configuration: BTreeMap::new(),
        });
        settings.extensions.push(ExtensionConfigurationV2 {
            id: "extension.fixture".into(),
            name: "Fixture extension".into(),
            version: "1.0.0".into(),
            status: ExtensionStatusV2::Installed,
            enabled: true,
            trust_accepted: true,
            manifest_path: "/opt/fixture/manifest.json".into(),
            entry_point: Some("/opt/fixture/run".into()),
            content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            compatibility: Some(">=0.1".into()),
            provenance: Some("local installation".into()),
            configuration: BTreeMap::new(),
        });
        settings.projects.push(ProjectConfigurationV2 {
            id: "project.fixture".into(),
            name: "Fixture".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::GitWorktree,
                location: "/workspace/fixture".into(),
            },
            default_workflow_id: Some("workflow.simple-chat".into()),
            portable_history_enabled: false,
        });
        settings.appearance.font_scale = 1.25;
        settings.validate().unwrap();
    }

    #[test]
    fn built_in_tool_configuration_rejects_unknown_adapter_fields_at_save_time() {
        let mut settings = SettingsConfigurationV2::default();
        assert_eq!(
            settings.tools[0].configuration["maximumBytes"],
            Value::from(PROJECT_FILE_READ_MAXIMUM_BYTES_V1)
        );
        assert_eq!(
            settings.tools[1].configuration["maximumResults"],
            Value::from(PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1)
        );
        settings.tools[0]
            .configuration
            .insert("ignoredByRuntime".into(), Value::Bool(true));

        let error = settings.validate().unwrap_err();
        assert!(
            error.contains("exactly the installed adapter fields"),
            "{error}"
        );

        let mut settings = SettingsConfigurationV2::default();
        settings.tools[0].configuration.insert(
            "maximumBytes".into(),
            Value::from(PROJECT_FILE_READ_MAXIMUM_BYTES_V1 + 1),
        );
        assert!(settings.validate().unwrap_err().contains("maximumBytes"));
    }

    #[test]
    fn validation_rejects_dangling_cross_section_references() {
        let mut settings = configured();
        settings.providers[0].credential_ref = Some("credential.missing".into());
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("unknown credential")
        );

        let mut settings = configured();
        settings.model_tiers[0].resolution = ModelTierResolutionV2::Exact {
            target: ModelTargetV2 {
                provider_id: "provider.fixture".into(),
                model_id: "model.missing".into(),
            },
        };
        assert!(settings.validate().unwrap_err().contains("unknown model"));
    }

    #[test]
    fn validation_rejects_secret_values_hidden_in_freeform_configuration() {
        for (key, nested) in [
            ("apiKeyValue", Value::from("must-not-persist")),
            ("authHeader", Value::from("Bearer must-not-persist")),
            (
                "public",
                serde_json::json!({"nested":{"credentials":"must-not-persist"}}),
            ),
            ("credentialRef", Value::from("credential.fixture")),
        ] {
            let mut settings = configured();
            settings.providers[0].models[0]
                .parameters
                .insert(key.into(), nested);
            let error = settings.validate().unwrap_err();
            assert!(error.contains("secret-like field"), "{error}");
        }
    }

    #[test]
    fn validation_rejects_secret_bearing_urls_and_stdio_arguments() {
        for url in [
            "https://user@example.test/v1",
            "https://user:password@example.test/v1",
            "https://example.test/v1?api_key=value",
            "https://example.test/v1#credential",
        ] {
            let mut settings = configured();
            settings.providers[0].base_url = url.into();
            let error = settings.validate().unwrap_err();
            assert!(error.contains("without credentials, query, or fragment"));
        }

        let mut settings = configured();
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.unsafe".into(),
            name: "Unsafe".into(),
            adapter: "codex_app_server".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: "codex".into(),
                args: vec!["app-server".into(), "--api-key=plaintext".into()],
                cwd: None,
                env: Vec::new(),
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
        });
        let error = settings.validate().unwrap_err();
        assert!(error.contains("credential-backed environment binding"));
    }

    #[test]
    fn validation_enforces_extension_trust_and_transport_shapes() {
        let mut settings = configured();
        settings.extensions.push(ExtensionConfigurationV2 {
            id: "extension.untrusted".into(),
            name: "Untrusted".into(),
            version: "1".into(),
            status: ExtensionStatusV2::Discovered,
            enabled: true,
            trust_accepted: false,
            manifest_path: "/tmp/manifest.json".into(),
            entry_point: None,
            content_hash: None,
            compatibility: None,
            provenance: None,
            configuration: BTreeMap::new(),
        });
        assert!(settings.validate().unwrap_err().contains("explicit trust"));

        settings.extensions.clear();
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.invalid".into(),
            name: "Invalid".into(),
            enabled: true,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "file:///tmp/socket".into(),
                headers: Vec::new(),
            },
        });
        assert!(settings.validate().unwrap_err().contains("HTTP(S)"));
    }

    #[test]
    fn inactive_runtime_controls_fail_closed_and_legacy_values_normalize_once() {
        let mut settings = configured();
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.inactive".into(),
            name: "Inactive".into(),
            enabled: false,
            auto_connect: true,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        settings.data.portable_history_enabled = true;
        settings.data.detailed_capture_enabled = true;
        settings.data.detailed_capture_retention_days = Some(7);
        settings.data.local_history_retention_days = Some(30);
        settings.projects.push(ProjectConfigurationV2 {
            id: "project.inactive".into(),
            name: "Inactive".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::LocalDirectory,
                location: "/workspace/inactive".into(),
            },
            default_workflow_id: None,
            portable_history_enabled: true,
        });
        assert!(settings.validate().is_err());
        assert!(settings.disable_inactive_runtime_controls());
        settings.validate().unwrap();
        assert!(!settings.disable_inactive_runtime_controls());
        assert!(!settings.mcp_servers[0].auto_connect);
        assert!(!settings.data.portable_history_enabled);
        assert_eq!(settings.data.detailed_capture_retention_days, None);
        assert_eq!(settings.data.local_history_retention_days, None);
        assert!(!settings.projects[0].portable_history_enabled);
    }

    #[test]
    fn legacy_subagent_turn_limit_is_removed_once() {
        let mut settings = SettingsConfigurationV2::default();
        let subagent = settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.subagent")
            .expect("subagent tool");
        subagent
            .configuration
            .insert("maximumTurns".into(), Value::from(4));

        assert!(settings.normalize_legacy_agent_turn_limits());
        assert!(!settings.normalize_legacy_agent_turn_limits());
        settings.validate().expect("normalized settings");
    }

    #[test]
    fn generic_commit_contract_rejects_values_installed_adapters_ignore() {
        let mut missing_api_key = configured();
        missing_api_key.credentials[0].field_names = vec!["token".into()];
        missing_api_key
            .validate()
            .expect("lossless document remains readable");
        assert!(
            missing_api_key
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("no api_key field")
        );

        let binding = NamedCredentialBindingV2 {
            name: "SECRET".into(),
            credential_ref: "credential.fixture".into(),
            field: "api_key".into(),
        };
        let mut bound_tool = configured();
        bound_tool.tools[0].credential_bindings = vec![binding.clone()];
        bound_tool
            .validate()
            .expect("lossless document remains readable");
        assert!(
            bound_tool
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("cannot consume")
        );

        let mut scoped_mcp = configured();
        scoped_mcp.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.scoped".into(),
            name: "Scoped".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: vec![binding],
            },
        });
        scoped_mcp
            .validate()
            .expect("generic binding is structurally valid");
        assert!(
            scoped_mcp
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("provider-scoped")
        );

        let mut ignored_external = configured();
        ignored_external
            .external_agents
            .push(ExternalAgentConfigurationV2 {
                id: "agent.codex".into(),
                name: "Codex".into(),
                adapter: "codex_app_server".into(),
                enabled: false,
                connection: IntegrationTransportV2::Stdio {
                    command: "codex".into(),
                    args: vec!["app-server".into()],
                    cwd: None,
                    env: Vec::new(),
                },
                credential_bindings: Vec::new(),
                mcp_server_ids: vec!["mcp.future".into()],
                capabilities: ExternalAgentCapabilitiesV2::default(),
                configuration: BTreeMap::new(),
            });
        ignored_external.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.future".into(),
            name: "Future".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        ignored_external
            .validate()
            .expect("forwarding metadata remains losslessly readable");
        assert!(
            ignored_external
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("cannot consume")
        );
    }

    #[test]
    fn installed_mcp_transport_targets_match_the_native_consumer() {
        let mut settings = configured();
        add_integration_credential(&mut settings);
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.targets".into(),
            name: "Targets".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: vec![integration_binding("X.Invalid")],
            },
        });
        settings
            .validate()
            .expect("legacy target remains losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("ASCII letters, digits, hyphens")
        );

        let IntegrationTransportV2::Http { headers, .. } = &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        headers[0].name = "McP-PrOtOcOl-VeRsIoN".into();
        settings
            .validate()
            .expect("legacy reserved target remains losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("reserved by the native MCP transport")
        );

        let IntegrationTransportV2::Http { headers, .. } = &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        headers[0].name = "MCP-PARAM-Cursor".into();
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("reserved by the native MCP transport")
        );

        let IntegrationTransportV2::Http { headers, .. } = &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        *headers = vec![
            integration_binding("Authorization"),
            integration_binding("authorization"),
        ];
        settings
            .validate()
            .expect("case variants remain readable before an explicit repair commit");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("header names are case-insensitive")
        );

        let too_long = integration_binding(&"X".repeat(129));
        assert!(
            validate_http_credential_targets("MCP fixture", &[too_long], true)
                .unwrap_err()
                .contains("at most 128")
        );
    }

    #[test]
    fn installed_mcp_stdio_contract_accepts_quoted_paths_and_ignores_cwd() {
        let mut settings = configured();
        add_integration_credential(&mut settings);
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.stdio".into(),
            name: "STDIO".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Stdio {
                command: "bin/mcp-server".into(),
                args: Vec::new(),
                cwd: None,
                env: Vec::new(),
            },
        });
        settings
            .validate()
            .expect("legacy relative executable remains losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("absolute or one bare command name from PATH")
        );

        let IntegrationTransportV2::Stdio { command, cwd, .. } =
            &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        let executable = std::env::current_exe()
            .expect("current executable")
            .to_string_lossy()
            .into_owned();
        *command = format!("\"{executable}\"");
        *cwd = Some("relative/workspace".into());
        settings
            .validate_installed_runtime_consumers()
            .expect("quoted executable and ignored MCP cwd are accepted");

        let IntegrationTransportV2::Stdio { cwd: _, env, .. } =
            &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        *env = vec![integration_binding("API-KEY")];
        settings
            .validate()
            .expect("legacy environment target remains losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("ASCII letters, digits, or underscores")
        );

        let IntegrationTransportV2::Stdio { env, .. } = &mut settings.mcp_servers[0].transport
        else {
            unreachable!()
        };
        *env = vec![integration_binding("Token"), integration_binding("token")];
        settings
            .validate()
            .expect("case variants remain readable before an explicit repair commit");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("cross-platform portability")
        );

        let too_long = integration_binding(&"X".repeat(129));
        assert!(
            validate_environment_credential_targets("MCP fixture", [&too_long])
                .unwrap_err()
                .contains("at most 128")
        );
    }

    #[test]
    fn installed_external_agent_contract_unifies_environment_and_rejects_forged_capabilities() {
        let mut settings = configured();
        add_integration_credential(&mut settings);
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.fixture".into(),
            name: "Fixture".into(),
            adapter: "acp".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: "fixture-agent".into(),
                args: Vec::new(),
                cwd: None,
                env: vec![integration_binding("TOKEN")],
            },
            credential_bindings: vec![integration_binding("token")],
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
        });
        settings
            .validate()
            .expect("cross-list case variants remain losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("cross-platform portability")
        );

        settings.external_agents[0].credential_bindings = vec![integration_binding("API-KEY")];
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("ASCII letters, digits, or underscores")
        );

        settings.external_agents[0].credential_bindings = vec![integration_binding("SECOND_TOKEN")];
        settings.external_agents[0].capabilities.progress = true;
        settings
            .validate()
            .expect("legacy negotiated capabilities remain losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("ephemeral probe output")
        );
    }

    #[test]
    fn installed_external_agent_environment_enforces_the_combined_consumer_limit() {
        let mut settings = configured();
        add_integration_credential(&mut settings);
        let connection_bindings = (0..128)
            .map(|index| integration_binding(&format!("CONNECTION_{index}")))
            .collect();
        let adapter_bindings = (0..129)
            .map(|index| integration_binding(&format!("ADAPTER_{index}")))
            .collect();
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.bindings".into(),
            name: "Bindings".into(),
            adapter: "acp".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: "fixture-agent".into(),
                args: Vec::new(),
                cwd: None,
                env: connection_bindings,
            },
            credential_bindings: adapter_bindings,
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
        });

        settings
            .validate()
            .expect("each legacy binding list remains independently readable");
        let error = settings.validate_installed_runtime_consumers().unwrap_err();
        assert!(error.contains("256-binding limit"), "{error}");
        assert!(error.contains("connection.env"), "{error}");

        settings.external_agents[0].credential_bindings.pop();
        settings
            .validate_installed_runtime_consumers()
            .expect("the consumer's exact 256-binding boundary remains accepted");
    }

    #[test]
    fn installed_codex_settings_match_the_probe_command_grammar() {
        let mut settings = configured();
        settings
            .external_agents
            .push(codex_agent("codex", ["serve"], None));
        settings
            .validate()
            .expect("legacy Codex command remains losslessly readable");
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("must begin with the explicit 'app-server'")
        );

        let IntegrationTransportV2::Stdio { args, .. } =
            &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *args = vec![
            "app-server".into(),
            "--listen".into(),
            "http://127.0.0.1:4500".into(),
        ];
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("only --listen stdio")
        );

        let IntegrationTransportV2::Stdio { args, .. } =
            &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *args = vec!["app-server".into(), "--listen=http://127.0.0.1:4500".into()];
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("only --listen stdio")
        );

        let IntegrationTransportV2::Stdio { command, args, .. } =
            &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *args = vec!["app-server".into()];
        *command = "bin/codex".into();
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("one bare command name from PATH")
        );

        let IntegrationTransportV2::Stdio { command, cwd, .. } =
            &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *command = "codex".into();
        *cwd = Some("relative/workspace".into());
        assert!(
            settings
                .validate_installed_runtime_consumers()
                .unwrap_err()
                .contains("working directory must be absolute")
        );

        let IntegrationTransportV2::Stdio { args, cwd, .. } =
            &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *args = vec!["app-server".into(), "--listen=stdio://".into()];
        *cwd = None;
        settings
            .validate_installed_runtime_consumers()
            .expect("bare PATH command and explicit STDIO listener");

        let IntegrationTransportV2::Stdio {
            command, args, cwd, ..
        } = &mut settings.external_agents[0].connection
        else {
            unreachable!()
        };
        *command = std::env::current_exe()
            .expect("current executable")
            .to_string_lossy()
            .into_owned();
        *args = vec!["app-server".into(), "--listen".into(), "stdio".into()];
        *cwd = Some(
            std::env::current_dir()
                .expect("current directory")
                .to_string_lossy()
                .into_owned(),
        );
        settings
            .validate_installed_runtime_consumers()
            .expect("absolute command and cwd with split STDIO listener");
    }

    #[test]
    fn generic_settings_cannot_fabricate_or_mutate_installed_extension_facts() {
        let previous = SettingsConfigurationV2::default();
        let mut invented = previous.clone();
        invented.extensions.push(ExtensionConfigurationV2 {
            id: "extension.invented".into(),
            name: "Invented".into(),
            version: "1.0.0".into(),
            status: ExtensionStatusV2::Installed,
            enabled: false,
            trust_accepted: false,
            manifest_path: "/opt/invented/extension.json".into(),
            entry_point: Some("/opt/invented/run".into()),
            content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            compatibility: Some("compatible".into()),
            provenance: Some("fabricated".into()),
            configuration: BTreeMap::new(),
        });
        assert!(
            validate_extension_lifecycle_update(&previous, &invented)
                .unwrap_err()
                .contains("cannot fabricate installation")
        );

        let mut registered = previous.clone();
        registered.extensions.push(ExtensionConfigurationV2 {
            id: "extension.registered".into(),
            name: "Registered".into(),
            version: "1.0.0".into(),
            status: ExtensionStatusV2::Installed,
            enabled: false,
            trust_accepted: false,
            manifest_path: "/opt/registered/extension.json".into(),
            entry_point: Some("/opt/registered/run".into()),
            content_hash: Some(format!("sha256:{}", "b".repeat(64))),
            compatibility: Some("compatible".into()),
            provenance: Some("verified".into()),
            configuration: BTreeMap::from([(
                "entryPointIdentity".into(),
                Value::String(format!("sha256:{}", "c".repeat(64))),
            )]),
        });
        let mut forbidden_enablement = registered.clone();
        forbidden_enablement.extensions[0].trust_accepted = true;
        forbidden_enablement.extensions[0].enabled = true;
        assert!(
            validate_extension_lifecycle_update(&registered, &forbidden_enablement)
                .unwrap_err()
                .contains("cannot be enabled through generic Settings")
        );

        let mut trusted_metadata = registered.clone();
        trusted_metadata.extensions[0].name = "Friendly label".into();
        trusted_metadata.extensions[0].trust_accepted = true;
        validate_extension_lifecycle_update(&registered, &trusted_metadata)
            .expect("trust metadata, name, and user configuration remain editable");

        let mut legacy_enabled = trusted_metadata.clone();
        legacy_enabled.extensions[0].enabled = true;
        validate_extension_lifecycle_update(&legacy_enabled, &legacy_enabled)
            .expect("existing enabled legacy metadata remains lossless");
        let mut disabled_legacy = legacy_enabled.clone();
        disabled_legacy.extensions[0].enabled = false;
        validate_extension_lifecycle_update(&legacy_enabled, &disabled_legacy)
            .expect("enabled legacy metadata can be disabled");

        let mut drifted = registered.clone();
        drifted.extensions[0].content_hash = Some(format!("sha256:{}", "d".repeat(64)));
        assert!(
            validate_extension_lifecycle_update(&registered, &drifted)
                .unwrap_err()
                .contains("immutable")
        );

        let mut removed = registered.clone();
        removed.extensions.clear();
        assert!(
            validate_extension_lifecycle_update(&registered, &removed)
                .unwrap_err()
                .contains("cannot be removed")
        );
    }

    #[test]
    fn unavailable_executors_cannot_gain_enabled_state_through_generic_settings() {
        let mut previous = SettingsConfigurationV2::default();
        previous.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.fixture".into(),
            name: "Fixture MCP".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        previous
            .external_agents
            .push(codex_agent("codex", ["app-server"], None));

        let mut enabled_mcp = previous.clone();
        enabled_mcp.mcp_servers[0].enabled = true;
        assert!(
            validate_unavailable_executor_enablement_update(&previous, &enabled_mcp)
                .unwrap_err()
                .contains("MCP server 'mcp.fixture' cannot be enabled")
        );

        let mut enabled_agent = previous.clone();
        enabled_agent.external_agents[0].enabled = true;
        assert!(
            validate_unavailable_executor_enablement_update(&previous, &enabled_agent)
                .unwrap_err()
                .contains("external agent 'agent.codex' cannot be enabled")
        );

        // Every built-in tool now has an installed v1 executor, so generic
        // Settings may enable any of them.
        for tool_id in BUILTIN_TOOL_IDS {
            let mut enabled_tool = previous.clone();
            enabled_tool
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
            validate_unavailable_executor_enablement_update(&previous, &enabled_tool)
                .unwrap_or_else(|error| panic!("enabling {tool_id} failed: {error}"));
        }

        let mut supported_tools = previous.clone();
        for tool_id in ["tool.files.read", "tool.files.search"] {
            supported_tools
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
        }
        validate_unavailable_executor_enablement_update(&previous, &supported_tools)
            .expect("implemented read-only tools may be enabled");

        let mut legacy_enabled = previous.clone();
        legacy_enabled.mcp_servers[0].enabled = true;
        legacy_enabled.external_agents[0].enabled = true;
        for tool_id in ["tool.files.edit", "tool.shell.host", "tool.python.host"] {
            legacy_enabled
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
        }
        validate_unavailable_executor_enablement_update(&legacy_enabled, &legacy_enabled)
            .expect("preexisting enabled metadata remains lossless");

        let mut disabled_legacy = legacy_enabled.clone();
        disabled_legacy.mcp_servers[0].enabled = false;
        disabled_legacy.external_agents[0].enabled = false;
        for tool_id in ["tool.files.edit", "tool.shell.host", "tool.python.host"] {
            disabled_legacy
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = false;
        }
        validate_unavailable_executor_enablement_update(&legacy_enabled, &disabled_legacy)
            .expect("preexisting enabled metadata may be cleared");
    }

    #[test]
    fn built_in_tool_contracts_cannot_claim_weaker_authority_defaults() {
        let mut settings = SettingsConfigurationV2::default();
        settings.tools[2]
            .configuration
            .insert("requiresApproval".into(), Value::Bool(false));
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("requiresApproval must be true")
        );

        let mut settings = SettingsConfigurationV2::default();
        settings.tools[7]
            .configuration
            .insert("authorityMode".into(), Value::from("sandboxed_python"));
        assert!(
            settings
                .validate()
                .unwrap_err()
                .contains("must be 'host_python'")
        );
    }

    #[test]
    fn serde_rejects_unknown_fields_and_never_has_secret_value_slots() {
        let value = serde_json::to_value(configured()).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("must-not-persist"));

        let mut object = value.as_object().unwrap().clone();
        object.insert("unexpected".into(), Value::Bool(true));
        assert!(serde_json::from_value::<SettingsConfigurationV2>(Value::Object(object)).is_err());
    }
}
