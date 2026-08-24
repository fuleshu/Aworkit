use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use aworkit_local_store::{
    DocumentAccessMode, DocumentKind, DocumentRepository, JsonDocument, RepositoryRoot,
    StoredDocument,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    dto::{ProviderSettingsSnapshot, SettingsSnapshot, WorkflowSnapshot},
    pipeline::MAXIMUM_WORKFLOW_SNAPSHOT_BYTES,
    provider_health::ProviderHealth,
    settings_v2::{
        AppearanceConfigurationV2, AppearanceModeV2, CredentialMetadataConfigurationV2,
        ModelConfigurationV2, ModelTargetV2, ModelTierResolutionV2, ProjectConfigurationV2,
        ProviderConfigurationV2, SETTINGS_SCHEMA_VERSION_V2, SettingsConfigurationV2,
        WorkspaceConfigurationV2, WorkspaceKindV2,
    },
};

const SETTINGS_ID: &str = "settings.desktop";
const WORKFLOW_ID: &str = "workflow.simple-chat";
const SUPPORTED_WORKFLOW_SCHEMA_VERSION: u64 = 1;
const MAXIMUM_AGENT_INSTRUCTIONS_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacySettingsDocumentV1 {
    pub schema_version: u64,
    pub appearance: String,
    pub portable_history_enabled: bool,
    pub project_roots: Vec<String>,
    pub provider: ProviderDocument,
}

pub(crate) type SettingsDocument = SettingsConfigurationV2;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderDocument {
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_endpoint: Option<String>,
    #[serde(default)]
    pub credential_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_revision: Option<u64>,
}

pub(crate) struct CanonicalDocuments {
    repository: RepositoryRoot,
    settings_version: u64,
    settings: SettingsDocument,
    workflow_version: u64,
    workflow: Value,
    workflow_editable: bool,
}

impl CanonicalDocuments {
    pub(crate) fn open(data_root: &Path) -> Result<Self, String> {
        let repository = RepositoryRoot::open(data_root.join("documents"))
            .map_err(|error| format!("cannot open desktop document store: {error}"))?;
        let (settings_version, settings) = load_or_create_settings(&repository)?;
        let (workflow_version, workflow, workflow_editable) = load_or_create_workflow(&repository)?;
        Ok(Self {
            repository,
            settings_version,
            settings,
            workflow_version,
            workflow,
            workflow_editable,
        })
    }

    pub(crate) fn settings(&self) -> &SettingsDocument {
        &self.settings
    }

    pub(crate) fn settings_snapshot(&self, health: &ProviderHealth) -> SettingsSnapshot {
        let provider = legacy_provider(&self.settings);
        SettingsSnapshot {
            version: self.settings_version,
            appearance: appearance_name(self.settings.appearance.mode).into(),
            portable_history_enabled: self.settings.data.portable_history_enabled,
            project_roots: self
                .settings
                .projects
                .iter()
                .map(|project| project.workspace.location.clone())
                .collect(),
            provider: ProviderSettingsSnapshot {
                base_url: provider.base_url,
                model: provider.model,
                credential_configured: provider.credential_ref.is_some(),
                state: health.state.clone(),
                detail: health.detail.clone(),
            },
        }
    }

    pub(crate) fn save_settings(
        &mut self,
        expected_version: u64,
        settings: SettingsDocument,
    ) -> Result<u64, String> {
        if expected_version != self.settings_version {
            return Err(format!(
                "settings version conflict: expected {expected_version}, actual {}",
                self.settings_version
            ));
        }
        settings.validate()?;
        let document = json_document(&settings)?;
        let saved = self
            .repository
            .save(
                DocumentKind::Configuration,
                SETTINGS_ID,
                Some(expected_version),
                &document,
            )
            .map_err(|error| format!("cannot commit settings: {error}"))?;
        self.settings_version = saved.version;
        self.settings = settings;
        Ok(saved.version)
    }

    pub(crate) fn workflow_snapshot(&self) -> WorkflowSnapshot {
        WorkflowSnapshot {
            version: self.workflow_version,
            document: self.workflow.clone(),
            editable: self.workflow_editable,
        }
    }

    pub(crate) fn save_workflow(
        &mut self,
        expected_version: u64,
        workflow: Value,
    ) -> Result<u64, String> {
        if expected_version != self.workflow_version {
            return Err(format!(
                "workflow version conflict: expected {expected_version}, actual {}",
                self.workflow_version
            ));
        }
        if !self.workflow_editable {
            return Err(
                "stored workflow uses an inspectable read-only schema and cannot be overwritten"
                    .into(),
            );
        }
        validate_editable_workflow_graph(&workflow)?;
        if exact_simple_chat_graph(&workflow, "agent.1", "agent") {
            validate_simple_chat_executable_configuration(&workflow)?;
        }
        let document = json_document(&workflow)?;
        let saved = self
            .repository
            .save(
                DocumentKind::Workflow,
                WORKFLOW_ID,
                Some(expected_version),
                &document,
            )
            .map_err(|error| format!("cannot commit workflow: {error}"))?;
        self.workflow_version = saved.version;
        self.workflow = workflow;
        self.workflow_editable = true;
        Ok(saved.version)
    }

    pub(crate) fn require_supported_simple_chat(&self) -> Result<(), String> {
        validate_simple_chat_graph(&self.workflow)
    }

    pub(crate) fn legacy_provider(&self) -> ProviderDocument {
        legacy_provider(&self.settings)
    }
}

fn load_or_create_settings(repository: &RepositoryRoot) -> Result<(u64, SettingsDocument), String> {
    match repository
        .load(DocumentKind::Configuration, SETTINGS_ID)
        .map_err(|error| format!("cannot load settings: {error}"))?
    {
        Some(stored) => load_or_migrate_settings(repository, stored),
        None => {
            let settings = SettingsDocument::default();
            let saved = repository
                .save(
                    DocumentKind::Configuration,
                    SETTINGS_ID,
                    None,
                    &json_document(&settings)?,
                )
                .map_err(|error| format!("cannot create settings: {error}"))?;
            Ok((saved.version, settings))
        }
    }
}

fn load_or_migrate_settings(
    repository: &RepositoryRoot,
    stored: StoredDocument,
) -> Result<(u64, SettingsDocument), String> {
    let value = stored_value_ref(&stored, "settings")?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| "stored settings document has no numeric schemaVersion".to_owned())?;
    match schema_version {
        1 => {
            let legacy: LegacySettingsDocumentV1 = decode_ref(&stored, "settings")?;
            let mut settings = migrate_v1(legacy)?;
            settings.disable_inactive_runtime_controls();
            settings.validate()?;
            let saved = repository
                .save(
                    DocumentKind::Configuration,
                    SETTINGS_ID,
                    Some(stored.version),
                    &json_document(&settings)?,
                )
                .map_err(|error| format!("cannot commit Settings v1 to v2 migration: {error}"))?;
            Ok((saved.version, settings))
        }
        version if version == u64::from(SETTINGS_SCHEMA_VERSION_V2) => {
            let mut settings: SettingsDocument = decode_ref(&stored, "settings")?;
            let repaired = settings.disable_inactive_runtime_controls()
                | settings.normalize_legacy_project_tool_limits();
            settings.validate()?;
            if repaired {
                let saved = repository
                    .save(
                        DocumentKind::Configuration,
                        SETTINGS_ID,
                        Some(stored.version),
                        &json_document(&settings)?,
                    )
                    .map_err(|error| {
                        format!("cannot disable unsupported persisted Settings controls: {error}")
                    })?;
                Ok((saved.version, settings))
            } else {
                Ok((stored.version, settings))
            }
        }
        version => Err(format!(
            "stored settings schemaVersion {version} is unsupported; this build supports v1 migration and v{SETTINGS_SCHEMA_VERSION_V2}"
        )),
    }
}

fn migrate_v1(legacy: LegacySettingsDocumentV1) -> Result<SettingsDocument, String> {
    if legacy.schema_version != 1 {
        return Err("Settings v1 migration received a non-v1 document".into());
    }
    let mut settings = SettingsDocument {
        appearance: AppearanceConfigurationV2 {
            mode: parse_legacy_appearance(&legacy.appearance)?,
            font_scale: 1.0,
        },
        data: super::settings_v2::DataConfigurationV2 {
            portable_history_enabled: false,
            ..super::settings_v2::DataConfigurationV2::default()
        },
        projects: legacy
            .project_roots
            .into_iter()
            .enumerate()
            .map(|(index, location)| ProjectConfigurationV2 {
                id: format!("project.migrated.{}", index + 1),
                name: format!("Migrated project {}", index + 1),
                workspace: WorkspaceConfigurationV2 {
                    kind: WorkspaceKindV2::LocalDirectory,
                    location,
                },
                default_workflow_id: None,
                portable_history_enabled: false,
            })
            .collect(),
        ..SettingsDocument::default()
    };
    let provider = legacy.provider;
    let has_provider = !provider.base_url.is_empty() || !provider.model.is_empty();
    if !has_provider {
        if provider.credential_ref.is_some() {
            return Err("Settings v1 contains a credential without a configured provider".into());
        }
        return Ok(settings);
    }
    if provider.base_url.is_empty() || provider.model.is_empty() {
        return Err(
            "Settings v1 provider endpoint and model must either both be set or both be empty"
                .into(),
        );
    }
    if let Some(reference) = provider.credential_ref.as_deref() {
        let revision = provider.credential_revision.ok_or_else(|| {
            "Settings v1 credential reference has no metadata revision".to_owned()
        })?;
        let endpoint = provider
            .credential_endpoint
            .clone()
            .ok_or_else(|| "Settings v1 credential reference has no endpoint binding".to_owned())?;
        if provider.credential_fields.is_empty() {
            return Err("Settings v1 credential reference has no field metadata".into());
        }
        settings
            .credentials
            .push(CredentialMetadataConfigurationV2 {
                credential_ref: reference.into(),
                label: "Migrated provider credential".into(),
                kind: "api_key".into(),
                field_names: provider.credential_fields.clone(),
                revision,
                bound_provider_id: Some("provider.primary".into()),
                bound_endpoint: Some(endpoint),
            });
    }
    settings.providers.push(ProviderConfigurationV2 {
        id: "provider.primary".into(),
        name: "Primary provider".into(),
        kind: "openai_compatible".into(),
        base_url: provider.base_url,
        enabled: true,
        credential_ref: provider.credential_ref,
        models: vec![ModelConfigurationV2 {
            id: "model.primary".into(),
            name: provider.model.clone(),
            remote_id: provider.model,
            enabled: true,
            context_window: None,
            max_output_tokens: None,
            capabilities: vec!["text".into()],
            parameters: BTreeMap::new(),
        }],
        configuration: BTreeMap::new(),
    });
    let target = ModelTargetV2 {
        provider_id: "provider.primary".into(),
        model_id: "model.primary".into(),
    };
    for tier in &mut settings.model_tiers {
        tier.resolution = ModelTierResolutionV2::Exact {
            target: target.clone(),
        };
    }
    Ok(settings)
}

fn legacy_provider(settings: &SettingsDocument) -> ProviderDocument {
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.id == "provider.primary" && provider.enabled)
        .or_else(|| settings.providers.iter().find(|provider| provider.enabled));
    let Some(provider) = provider else {
        return ProviderDocument::default();
    };
    let model = provider
        .models
        .iter()
        .find(|model| model.id == "model.primary")
        .or_else(|| provider.models.iter().find(|model| model.enabled));
    let credential = provider
        .credential_ref
        .as_deref()
        .and_then(|reference| settings.credential(reference));
    ProviderDocument {
        base_url: provider.base_url.clone(),
        model: model.map_or_else(String::new, |model| model.remote_id.clone()),
        credential_ref: provider.credential_ref.clone(),
        credential_endpoint: credential.and_then(|credential| credential.bound_endpoint.clone()),
        credential_fields: credential
            .map_or_else(Vec::new, |credential| credential.field_names.clone()),
        credential_revision: credential.map(|credential| credential.revision),
    }
}

fn appearance_name(mode: AppearanceModeV2) -> &'static str {
    match mode {
        AppearanceModeV2::System => "system",
        AppearanceModeV2::Light => "light",
        AppearanceModeV2::Dark => "dark",
    }
}

fn parse_legacy_appearance(value: &str) -> Result<AppearanceModeV2, String> {
    match value {
        "system" => Ok(AppearanceModeV2::System),
        "light" => Ok(AppearanceModeV2::Light),
        "dark" => Ok(AppearanceModeV2::Dark),
        _ => Err(format!(
            "Settings v1 appearance '{value}' is invalid; expected system, light, or dark"
        )),
    }
}

fn load_or_create_workflow(repository: &RepositoryRoot) -> Result<(u64, Value, bool), String> {
    match repository
        .load(DocumentKind::Workflow, WORKFLOW_ID)
        .map_err(|error| format!("cannot load Simple Chat workflow: {error}"))?
    {
        Some(stored) => {
            let version = stored.version;
            let editable = stored.access == DocumentAccessMode::Editable;
            let mut workflow = stored_value(stored, "workflow")?;
            if editable && migrate_rescue_model_node(&mut workflow) {
                let saved = repository
                    .save(
                        DocumentKind::Workflow,
                        WORKFLOW_ID,
                        Some(version),
                        &json_document(&workflow)?,
                    )
                    .map_err(|error| format!("cannot migrate Simple Chat Agent node: {error}"))?;
                Ok((saved.version, workflow, true))
            } else {
                Ok((version, workflow, editable))
            }
        }
        None => {
            let workflow = default_simple_chat_workflow();
            let saved = repository
                .save(
                    DocumentKind::Workflow,
                    WORKFLOW_ID,
                    None,
                    &json_document(&workflow)?,
                )
                .map_err(|error| format!("cannot create Simple Chat workflow: {error}"))?;
            Ok((saved.version, workflow, true))
        }
    }
}

fn decode_ref<T: for<'de> Deserialize<'de>>(
    stored: &StoredDocument,
    label: &str,
) -> Result<T, String> {
    serde_json::from_slice(stored.document.raw_json())
        .map_err(|error| format!("stored {label} document is invalid: {error}"))
}

fn stored_value_ref(stored: &StoredDocument, label: &str) -> Result<Value, String> {
    stored
        .document
        .value()
        .map_err(|error| format!("stored {label} document is invalid: {error}"))
}

fn stored_value(stored: StoredDocument, label: &str) -> Result<Value, String> {
    stored
        .document
        .value()
        .map_err(|error| format!("stored {label} document is invalid: {error}"))
}

fn json_document(value: &impl Serialize) -> Result<JsonDocument, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("cannot serialize canonical document: {error}"))?;
    JsonDocument::parse(bytes)
        .map_err(|error| format!("cannot validate canonical document: {error}"))
}

pub(crate) fn validate_workflow_document(document: &Value) -> Result<(), String> {
    let object = document
        .as_object()
        .ok_or_else(|| "workflow document must be an object".to_owned())?;
    if object
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .filter(|version| *version > 0)
        .is_none()
        || !object.get("nodes").is_some_and(Value::is_array)
        || !object.get("edges").is_some_and(Value::is_array)
    {
        return Err(
            "workflow document requires a positive schemaVersion plus nodes and edges arrays"
                .into(),
        );
    }
    Ok(())
}

fn validate_editable_workflow_graph(document: &Value) -> Result<(), String> {
    validate_workflow_document(document)?;
    let schema_version = document["schemaVersion"]
        .as_u64()
        .expect("validated workflow schemaVersion");
    if schema_version != SUPPORTED_WORKFLOW_SCHEMA_VERSION {
        return Err(format!(
            "workflow schemaVersion {schema_version} is inspectable but not editable; this build supports v{SUPPORTED_WORKFLOW_SCHEMA_VERSION}"
        ));
    }

    let nodes = document["nodes"]
        .as_array()
        .expect("validated workflow nodes");
    let mut node_ids = BTreeSet::new();
    for (index, node) in nodes.iter().enumerate() {
        let object = node
            .as_object()
            .ok_or_else(|| format!("workflow node {index} must be an object"))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("workflow node {index} requires a non-empty string id"))?;
        object
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("workflow node '{id}' requires a non-empty string type"))?;
        if !node_ids.insert(id.to_owned()) {
            return Err(format!("workflow contains duplicate node id '{id}'"));
        }
        if object
            .get("configuration")
            .is_some_and(|configuration| !configuration.is_object())
        {
            return Err(format!(
                "workflow node '{id}' configuration must be a JSON object"
            ));
        }
    }

    let edges = document["edges"]
        .as_array()
        .expect("validated workflow edges");
    let mut edge_ids = BTreeSet::new();
    for (index, edge) in edges.iter().enumerate() {
        let object = edge
            .as_object()
            .ok_or_else(|| format!("workflow transition {index} must be an object"))?;
        if let Some(id) = object.get("id") {
            let id = id
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    format!("workflow transition {index} id must be a non-empty string")
                })?;
            if !edge_ids.insert(id.to_owned()) {
                return Err(format!("workflow contains duplicate transition id '{id}'"));
            }
        }
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("workflow transition {index} requires a source node id"))?;
        let target = object
            .get("target")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("workflow transition {index} requires a target node id"))?;
        if !node_ids.contains(source) {
            return Err(format!(
                "workflow transition {index} source '{source}' does not exist"
            ));
        }
        if !node_ids.contains(target) {
            return Err(format!(
                "workflow transition {index} target '{target}' does not exist"
            ));
        }
    }
    Ok(())
}

fn validate_simple_chat_graph(document: &Value) -> Result<(), String> {
    validate_editable_workflow_graph(document)?;
    if exact_simple_chat_graph(document, "agent.1", "agent") {
        return validate_simple_chat_executable_configuration(document);
    }
    Err(
        "this build can run only the Simple Chat graph: Input → Agent → Output → Wait for Input"
            .into(),
    )
}

fn validate_simple_chat_executable_configuration(document: &Value) -> Result<(), String> {
    if document.get("id").and_then(Value::as_str) != Some(WORKFLOW_ID) {
        return Err("Simple Chat executable document id must be workflow.simple-chat".into());
    }
    let serialized_size = serde_json::to_vec(document)
        .map_err(|error| format!("cannot encode Simple Chat workflow: {error}"))?
        .len();
    if serialized_size > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES {
        return Err(format!(
            "Simple Chat workflow exceeds the executable {} KiB persistence bound; remove oversized preserved metadata before running it",
            MAXIMUM_WORKFLOW_SNAPSHOT_BYTES / 1024
        ));
    }
    let nodes = document["nodes"]
        .as_array()
        .expect("validated exact Simple Chat nodes");
    for id in ["input.1", "output.1", "wait.1"] {
        let node = nodes
            .iter()
            .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
            .expect("exact graph contains required node");
        if let Some(configuration) = node.get("configuration")
            && configuration
                .as_object()
                .is_none_or(|object| !object.is_empty())
        {
            return Err(format!(
                "Simple Chat node '{id}' configuration must be omitted or an empty object"
            ));
        }
    }

    let agent = nodes
        .iter()
        .find(|node| node.get("id").and_then(Value::as_str) == Some("agent.1"))
        .expect("exact graph contains the Agent node");
    let configuration = agent
        .get("configuration")
        .and_then(Value::as_object)
        .ok_or_else(|| "Simple Chat Agent configuration must be an object".to_owned())?;
    let keys = configuration
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let required = BTreeSet::from(["maxTurns", "modelTierId", "toolIds"]);
    let allowed = BTreeSet::from(["instructions", "maxTurns", "modelTierId", "toolIds"]);
    if !required.is_subset(&keys) || !keys.is_subset(&allowed) {
        return Err(
            "Simple Chat Agent configuration accepts exactly modelTierId, toolIds, maxTurns, and optional instructions"
                .into(),
        );
    }
    if configuration.get("modelTierId").and_then(Value::as_str) != Some("tier:balanced") {
        return Err(
            "Simple Chat Agent must reference the portable tier:balanced model tier".into(),
        );
    }
    let tool_ids = configuration
        .get("toolIds")
        .and_then(Value::as_array)
        .ok_or_else(|| "Simple Chat Agent toolIds must be an array".to_owned())?;
    let mut seen = BTreeSet::new();
    for tool_id in tool_ids {
        let tool_id = tool_id
            .as_str()
            .ok_or_else(|| "Simple Chat Agent toolIds must contain strings".to_owned())?;
        if !matches!(tool_id, "tool.files.read" | "tool.files.search") {
            return Err(
                "Simple Chat can bind only project file read/search; edit, shell, and Python require an approval path"
                    .into(),
            );
        }
        if !seen.insert(tool_id) {
            return Err("Simple Chat Agent toolIds must be unique".into());
        }
    }
    let maximum_turns = configuration
        .get("maxTurns")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Simple Chat Agent maxTurns must be an integer".to_owned())?;
    if (tool_ids.is_empty() && maximum_turns != 1)
        || (!tool_ids.is_empty() && !(2..=8).contains(&maximum_turns))
    {
        return Err(
            "Simple Chat requires maxTurns=1 without tools or maxTurns=2..8 with project read/search"
                .into(),
        );
    }
    if let Some(instructions) = configuration.get("instructions") {
        let instructions = instructions
            .as_str()
            .filter(|value| {
                !value.trim().is_empty()
                    && value.len() <= MAXIMUM_AGENT_INSTRUCTIONS_BYTES
                    && !value.contains('\0')
            })
            .ok_or_else(|| {
                "Simple Chat Agent instructions must be a non-empty string of at most 64 KiB"
                    .to_owned()
            })?;
        debug_assert!(!instructions.is_empty());
    }

    let edges = document["edges"]
        .as_array()
        .expect("validated exact Simple Chat edges");
    if edges.iter().any(|edge| {
        edge.get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| StableId::parse(id.to_owned()).is_err())
    }) {
        return Err("Simple Chat transition IDs must be stable identifiers".into());
    }
    Ok(())
}

fn exact_simple_chat_graph(document: &Value, middle_id: &str, middle_type: &str) -> bool {
    let nodes = document["nodes"]
        .as_array()
        .expect("validated workflow nodes");
    let edges = document["edges"]
        .as_array()
        .expect("validated workflow edges");
    let required_nodes = [
        ("input.1", "input"),
        (middle_id, middle_type),
        ("output.1", "output"),
        ("wait.1", "wait"),
    ];
    let nodes_match = nodes.len() == required_nodes.len()
        && required_nodes.iter().all(|(id, kind)| {
            nodes
                .iter()
                .filter(|node| {
                    node.get("id").and_then(Value::as_str) == Some(*id)
                        && node.get("type").and_then(Value::as_str) == Some(*kind)
                })
                .count()
                == 1
        });
    let required_edges = [
        ("input.1", middle_id),
        (middle_id, "output.1"),
        ("output.1", "wait.1"),
    ];
    let edges_match = edges.len() == required_edges.len()
        && required_edges.iter().all(|(source, target)| {
            edges
                .iter()
                .filter(|edge| {
                    edge.get("source").and_then(Value::as_str) == Some(*source)
                        && edge.get("target").and_then(Value::as_str) == Some(*target)
                })
                .count()
                == 1
        });
    nodes_match && edges_match
}

fn migrate_rescue_model_node(document: &mut Value) -> bool {
    if validate_workflow_document(document).is_err()
        || !exact_simple_chat_graph(document, "model.1", "model")
    {
        return false;
    }
    let nodes = document["nodes"]
        .as_array_mut()
        .expect("validated workflow nodes");
    if let Some(model) = nodes
        .iter_mut()
        .find(|node| node.get("id").and_then(Value::as_str) == Some("model.1"))
        .and_then(Value::as_object_mut)
    {
        model.insert("id".into(), Value::String("agent.1".into()));
        model.insert("type".into(), Value::String("agent".into()));
        model.insert("label".into(), Value::String("Agent".into()));
        model.insert(
            "configuration".into(),
            json!({
                "modelTierId": "tier:balanced",
                "toolIds": [],
                "maxTurns": 1
            }),
        );
    }
    for edge in document["edges"]
        .as_array_mut()
        .expect("validated workflow edges")
    {
        let Some(edge) = edge.as_object_mut() else {
            continue;
        };
        for field in ["source", "target"] {
            if edge.get(field).and_then(Value::as_str) == Some("model.1") {
                edge.insert(field.into(), Value::String("agent.1".into()));
            }
        }
    }
    if let Some(object) = document.as_object_mut() {
        object.insert(
            "comments".into(),
            Value::String("Simple Chat: Chat Input → Agent → Chat Output → Wait for Input.".into()),
        );
    }
    true
}

pub(crate) fn default_simple_chat_workflow() -> Value {
    json!({
        "schemaVersion": 1,
        "id": "workflow.simple-chat",
        "name": "Simple Chat",
        "nodes": [
            {"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}},
            {"id":"agent.1","label":"Agent","type":"agent","position":{"x":245,"y":205},"configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":1}},
            {"id":"output.1","label":"Output","type":"output","position":{"x":470,"y":205}},
            {"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":695,"y":205}}
        ],
        "edges": [
            {"id":"input-agent","source":"input.1","target":"agent.1"},
            {"id":"agent-output","source":"agent.1","target":"output.1"},
            {"id":"output-wait","source":"output.1","target":"wait.1"}
        ],
        "comments": "Simple Chat: Chat Input → Agent → Chat Output → Wait for Input."
    })
}

#[cfg(test)]
mod tests {
    use crate::runtime::settings_v2::{IntegrationTransportV2, McpServerConfigurationV2};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn exact_simple_chat_graph_is_supported() {
        validate_simple_chat_graph(&default_simple_chat_workflow()).unwrap();
    }

    #[test]
    fn simple_chat_accepts_bounded_project_read_search_and_rejects_approval_tools() {
        let mut workflow = default_simple_chat_workflow();
        workflow["nodes"][1]["configuration"]["toolIds"] =
            json!(["tool.files.read", "tool.files.search"]);
        workflow["nodes"][1]["configuration"]["maxTurns"] = json!(4);
        validate_simple_chat_graph(&workflow).unwrap();
        workflow["nodes"][1]["configuration"]["toolIds"] = json!(["tool.files.edit"]);
        assert!(
            validate_simple_chat_graph(&workflow)
                .unwrap_err()
                .contains("approval path")
        );
    }

    #[test]
    fn simple_chat_rejects_unknown_or_inert_node_configuration_before_execution() {
        let mut workflow = default_simple_chat_workflow();
        workflow["nodes"][1]["configuration"]["ignoredAtRuntime"] = json!(true);
        assert!(
            validate_simple_chat_graph(&workflow)
                .unwrap_err()
                .contains("accepts exactly")
        );

        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        assert!(documents.save_workflow(1, workflow).is_err());
        assert_eq!(documents.workflow_snapshot().version, 1);

        let mut workflow = default_simple_chat_workflow();
        workflow["nodes"][0]["configuration"] = json!({"unused": true});
        assert!(
            validate_simple_chat_graph(&workflow)
                .unwrap_err()
                .contains("omitted or an empty object")
        );
    }

    #[test]
    fn simple_chat_accepts_bounded_agent_instructions() {
        let mut workflow = default_simple_chat_workflow();
        workflow["nodes"][1]["configuration"]["instructions"] =
            json!("Use project evidence and cite the path.");
        validate_simple_chat_graph(&workflow).unwrap();

        workflow["nodes"][1]["configuration"]["instructions"] = json!("   ");
        assert!(validate_simple_chat_graph(&workflow).is_err());
    }

    #[test]
    fn oversized_exact_simple_chat_is_preserved_as_a_draft_but_never_saved_as_executable() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let mut oversized = default_simple_chat_workflow();
        oversized["preservedMetadata"] = Value::String("x".repeat(MAXIMUM_WORKFLOW_SNAPSHOT_BYTES));

        let error = documents.save_workflow(1, oversized).unwrap_err();
        assert!(error.contains("executable 128 KiB persistence bound"));
        assert_eq!(documents.workflow_snapshot().version, 1);
        assert!(documents.require_supported_simple_chat().is_ok());
    }

    #[test]
    fn rescue_model_graph_is_losslessly_migrated_to_the_spec_agent_graph() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let legacy = json!({
            "schemaVersion": 1,
            "id": "workflow.simple-chat",
            "name": "Simple Chat",
            "customMetadata": {"preserve": true},
            "nodes": [
                {"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}},
                {"id":"model.1","label":"Custom label","type":"model","position":{"x":245,"y":205},"unknown":{"kept":true}},
                {"id":"output.1","label":"Output","type":"output","position":{"x":470,"y":205}},
                {"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":695,"y":205}}
            ],
            "edges": [
                {"id":"input-model","source":"input.1","target":"model.1","unknown":1},
                {"id":"model-output","source":"model.1","target":"output.1"},
                {"id":"output-wait","source":"output.1","target":"wait.1"}
            ]
        });
        repository
            .save(
                DocumentKind::Workflow,
                WORKFLOW_ID,
                None,
                &json_document(&legacy).unwrap(),
            )
            .unwrap();
        let documents = CanonicalDocuments::open(root.path()).unwrap();
        let workflow = documents.workflow_snapshot();
        assert_eq!(workflow.version, 2);
        assert_eq!(workflow.document["customMetadata"]["preserve"], true);
        assert!(
            workflow.document["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| {
                    node["id"] == "agent.1"
                        && node["type"] == "agent"
                        && node["configuration"]["modelTierId"] == "tier:balanced"
                        && node["unknown"]["kept"] == true
                })
        );
        assert!(
            workflow.document["edges"]
                .as_array()
                .unwrap()
                .iter()
                .all(|edge| { edge["source"] != "model.1" && edge["target"] != "model.1" })
        );
        validate_simple_chat_graph(&workflow.document).unwrap();
    }

    #[test]
    fn v1_settings_are_atomically_migrated_to_v2_once() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let legacy = json!({
            "schemaVersion": 1,
            "appearance": "dark",
            "portableHistoryEnabled": true,
            "projectRoots": ["/workspace/atlas"],
            "provider": {
                "baseUrl": "https://provider.example/v1",
                "model": "fixture-model",
                "credentialRef": "credential.fixture",
                "credentialEndpoint": "https://provider.example/v1",
                "credentialFields": ["api_key"],
                "credentialRevision": 3
            }
        });
        repository
            .save(
                DocumentKind::Configuration,
                SETTINGS_ID,
                None,
                &json_document(&legacy).unwrap(),
            )
            .unwrap();

        let migrated = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(migrated.settings_version, 2);
        assert_eq!(migrated.settings.schema_version, 2);
        assert_eq!(migrated.settings.appearance.mode, AppearanceModeV2::Dark);
        assert!(!migrated.settings.data.portable_history_enabled);
        assert!(!migrated.settings.projects[0].portable_history_enabled);
        assert_eq!(migrated.settings.providers.len(), 1);
        assert_eq!(
            migrated.settings.providers[0].models[0].remote_id,
            "fixture-model"
        );
        assert_eq!(migrated.settings.credentials[0].revision, 3);
        assert_eq!(
            migrated.settings.projects[0].workspace.location,
            "/workspace/atlas"
        );
        assert_eq!(migrated.settings.tools.len(), 5);
        assert!(migrated.settings.tools.iter().all(|tool| !tool.enabled));
        let canonical = repository
            .export_lossless(DocumentKind::Configuration, SETTINGS_ID)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&canonical).unwrap()["schemaVersion"],
            json!(2)
        );
        drop(migrated);

        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.settings_version, 2);
        assert_eq!(reopened.settings.schema_version, 2);
    }

    #[test]
    fn persisted_v2_inactive_controls_are_disabled_and_rewritten_once() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let mut settings = SettingsConfigurationV2::default();
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.legacy".into(),
            name: "Legacy MCP".into(),
            enabled: true,
            auto_connect: true,
            transport: IntegrationTransportV2::Stdio {
                command: "legacy-mcp".into(),
                args: Vec::new(),
                cwd: None,
                env: Vec::new(),
            },
        });
        settings.data.portable_history_enabled = true;
        settings.data.detailed_capture_enabled = true;
        settings.data.detailed_capture_retention_days = Some(7);
        settings.data.local_history_retention_days = Some(30);
        settings.projects.push(ProjectConfigurationV2 {
            id: "project.legacy".into(),
            name: "Legacy project".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::LocalDirectory,
                location: "/workspace/legacy".into(),
            },
            default_workflow_id: None,
            portable_history_enabled: true,
        });
        repository
            .save(
                DocumentKind::Configuration,
                SETTINGS_ID,
                None,
                &json_document(&settings).unwrap(),
            )
            .unwrap();

        let repaired = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(repaired.settings_version, 2);
        assert!(!repaired.settings.mcp_servers[0].auto_connect);
        assert!(!repaired.settings.data.portable_history_enabled);
        assert!(!repaired.settings.data.detailed_capture_enabled);
        assert_eq!(repaired.settings.data.detailed_capture_retention_days, None);
        assert_eq!(repaired.settings.data.local_history_retention_days, None);
        assert!(!repaired.settings.projects[0].portable_history_enabled);
        drop(repaired);

        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.settings_version, 2);
        assert!(!reopened.settings.mcp_servers[0].auto_connect);
        assert!(!reopened.settings.projects[0].portable_history_enabled);
    }

    #[test]
    fn persisted_legacy_project_tool_limits_are_narrowed_once_before_validation() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let mut settings = SettingsConfigurationV2::default();
        settings.tools[0]
            .configuration
            .insert("maximumBytes".into(), Value::from(1_048_576_u64));
        settings.tools[1]
            .configuration
            .insert("maximumResults".into(), Value::from(1_024_u64));
        repository
            .save(
                DocumentKind::Configuration,
                SETTINGS_ID,
                None,
                &json_document(&settings).unwrap(),
            )
            .unwrap();

        let repaired = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(repaired.settings_version, 2);
        assert_eq!(
            repaired.settings.tools[0].configuration["maximumBytes"],
            Value::from(crate::runtime::PROJECT_FILE_READ_MAXIMUM_BYTES_V1)
        );
        assert_eq!(
            repaired.settings.tools[1].configuration["maximumResults"],
            Value::from(crate::runtime::PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1)
        );
        drop(repaired);

        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.settings_version, 2);
    }

    #[test]
    fn unsupported_future_settings_remain_lossless_and_are_not_opened_as_editable() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let raw = br#"{"schemaVersion":3,"futureSection":{"value":42}}"#;
        repository
            .import_inert(
                DocumentKind::Configuration,
                SETTINGS_ID,
                None,
                &JsonDocument::parse(raw.to_vec()).unwrap(),
            )
            .unwrap();

        let error = CanonicalDocuments::open(root.path())
            .err()
            .expect("future settings must fail closed");
        assert!(error.contains("schemaVersion 3 is unsupported"));
        assert_eq!(
            repository
                .export_lossless(DocumentKind::Configuration, SETTINGS_ID)
                .unwrap()
                .unwrap(),
            raw
        );
    }

    #[test]
    fn unsupported_future_workflow_remains_lossless_and_read_only() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let raw = br#"{
            "schemaVersion": 2,
            "nodes": [{"id":"future.1","type":"future@2","configuration":{"new":true}}],
            "edges": [],
            "futureRoot": {"retained": true}
        }"#;
        repository
            .import_inert(
                DocumentKind::Workflow,
                WORKFLOW_ID,
                None,
                &JsonDocument::parse(raw.to_vec()).unwrap(),
            )
            .unwrap();

        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let snapshot = documents.workflow_snapshot();
        assert_eq!(snapshot.version, 1);
        assert!(!snapshot.editable);
        assert_eq!(snapshot.document["futureRoot"]["retained"], json!(true));
        assert!(documents.require_supported_simple_chat().is_err());
        let error = documents
            .save_workflow(snapshot.version, snapshot.document)
            .unwrap_err();
        assert!(error.contains("inspectable read-only"));
        drop(documents);

        assert_eq!(
            repository
                .export_lossless(DocumentKind::Workflow, WORKFLOW_ID)
                .unwrap()
                .unwrap(),
            raw
        );
    }

    #[test]
    fn arbitrary_structurally_valid_v1_workflow_saves_reloads_and_stays_non_runnable() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let arbitrary = json!({
            "schemaVersion": 1,
            "id": "workflow.advanced",
            "name": "Advanced future harness",
            "nodes": [
                {
                    "id": "future.1",
                    "type": "plugin.future@2",
                    "position": {"x": 17, "y": 29, "futureLayout": [1, 2]},
                    "configuration": {"futureOption": {"retained": true}},
                    "capabilityStatus": "missing",
                    "futureNode": {"retained": true}
                },
                {"id": "output.custom", "type": "output"}
            ],
            "edges": [{
                "id": "future-edge",
                "source": "future.1",
                "target": "output.custom",
                "sourcePort": "future.out",
                "targetPort": "future.in",
                "futureEdge": {"retained": true}
            }],
            "futureRoot": {"retained": true}
        });
        assert_eq!(documents.save_workflow(1, arbitrary.clone()).unwrap(), 2);
        assert_eq!(documents.workflow_snapshot().document, arbitrary);
        assert!(documents.workflow_snapshot().editable);
        assert!(documents.require_supported_simple_chat().is_err());

        drop(documents);
        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.workflow_snapshot().version, 2);
        assert_eq!(reopened.workflow_snapshot().document, arbitrary);
        assert!(reopened.workflow_snapshot().editable);
        assert!(reopened.require_supported_simple_chat().is_err());
    }

    #[test]
    fn workflow_save_rejects_broken_references_without_replacing_the_document() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let broken = json!({
            "schemaVersion": 1,
            "nodes": [{"id": "input", "type": "input"}],
            "edges": [{"id": "broken", "source": "input", "target": "missing"}]
        });
        let error = documents.save_workflow(1, broken).unwrap_err();
        assert!(error.contains("target 'missing' does not exist"));
        assert_eq!(documents.workflow_snapshot().version, 1);
        assert!(documents.require_supported_simple_chat().is_ok());
    }

    #[test]
    fn extra_or_duplicate_nodes_are_rejected() {
        let mut extra = default_simple_chat_workflow();
        extra["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"tool.1","type":"tool"}));
        assert!(validate_simple_chat_graph(&extra).is_err());

        let mut duplicate = default_simple_chat_workflow();
        duplicate["nodes"] = json!([
            {"id":"input.1","type":"input"},
            {"id":"input.1","type":"input"},
            {"id":"output.1","type":"output"},
            {"id":"wait.1","type":"wait"}
        ]);
        assert!(validate_simple_chat_graph(&duplicate).is_err());
    }

    #[test]
    fn extra_or_duplicate_edges_are_rejected() {
        let mut extra = default_simple_chat_workflow();
        extra["edges"]
            .as_array_mut()
            .unwrap()
            .push(json!({"source":"wait.1","target":"input.1"}));
        assert!(validate_simple_chat_graph(&extra).is_err());

        let mut duplicate = default_simple_chat_workflow();
        duplicate["edges"] = json!([
            {"source":"input.1","target":"model.1"},
            {"source":"input.1","target":"model.1"},
            {"source":"output.1","target":"wait.1"}
        ]);
        assert!(validate_simple_chat_graph(&duplicate).is_err());
    }
}
