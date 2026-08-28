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
    dto::{
        ProviderSettingsSnapshot, SettingsSnapshot, WorkflowEntryDto, WorkflowLibrarySnapshot,
        WorkflowSnapshot,
    },
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
const WORKFLOW_LIBRARY_ID: &str = "workflow-library.desktop";
const WORKFLOW_LIBRARY_SCHEMA_VERSION: u64 = 1;
const SUPPORTED_WORKFLOW_SCHEMA_VERSION: u64 = 1;
const BUNDLED_WORKFLOW_LIBRARY_JSON: &str =
    include_str!("../../../workflows/default-workflows.json");
const MAXIMUM_WORKFLOW_NAME_BYTES: usize = 128;
const MAXIMUM_MODEL_CALL_INSTRUCTIONS_BYTES: usize = 64 * 1024;
const MAXIMUM_MODEL_CALL_TOKENS: u64 = 8192;
const MINIMUM_AGENT_TURNS: u64 = 1;
const MAXIMUM_AGENT_TURNS: u64 = 12;
pub(crate) const KNOWN_NODE_TYPES: &[&str] = &[
    "input",
    "agent",
    "model_call",
    "tool",
    "condition",
    "parallel",
    "approval",
    "output",
    "wait",
    "completion",
];

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
    library_version: u64,
    default_workflow_id: String,
    workflows: BTreeMap<String, StoredWorkflowState>,
}

#[derive(Clone, Debug)]
struct StoredWorkflowState {
    version: u64,
    document: Value,
    editable: bool,
}

/// Build-time workflow assets. The runtime treats these exactly like imported
/// JSON documents; neither identity, graph shape, nor default selection is
/// duplicated in Rust.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundledWorkflowLibrary {
    schema_version: u64,
    default_workflow_id: String,
    creation_default_template_id: String,
    workflows: Vec<BundledWorkflowTemplate>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundledWorkflowTemplate {
    template_id: String,
    seed_on_fresh_profile: bool,
    document: Value,
}

impl CanonicalDocuments {
    pub(crate) fn open(data_root: &Path) -> Result<Self, String> {
        let repository = RepositoryRoot::open(data_root.join("documents"))
            .map_err(|error| format!("cannot open desktop document store: {error}"))?;
        let bundled = bundled_workflow_library()?;
        let (settings_version, settings) = load_or_create_settings(&repository)?;
        let (workflows, fresh_profile) = load_or_create_workflows(&repository, &bundled)?;
        let (library_version, default_workflow_id) =
            load_or_create_workflow_library(&repository, &workflows, &bundled, fresh_profile)?;
        let default_workflow_id = if workflows.contains_key(default_workflow_id.as_str()) {
            default_workflow_id
        } else {
            workflows
                .keys()
                .next()
                .ok_or_else(|| "workflow library contains no workflow documents".to_owned())?
                .clone()
        };
        Ok(Self {
            repository,
            settings_version,
            settings,
            library_version,
            default_workflow_id,
            workflows,
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
        self.workflow_snapshot_for(&self.default_workflow_id)
    }

    pub(crate) fn workflow_snapshot_for(&self, workflow_id: &str) -> WorkflowSnapshot {
        match self.workflows.get(workflow_id) {
            Some(state) => WorkflowSnapshot {
                version: state.version,
                document: state.document.clone(),
                editable: state.editable,
            },
            None => WorkflowSnapshot {
                version: 0,
                document: Value::Null,
                editable: false,
            },
        }
    }

    pub(crate) fn workflow_library(&self) -> WorkflowLibrarySnapshot {
        WorkflowLibrarySnapshot {
            version: self.library_version,
            default_workflow_id: self.default_workflow_id.clone(),
            entries: self
                .workflows
                .iter()
                .map(|(id, state)| WorkflowEntryDto {
                    id: id.clone(),
                    name: workflow_display_name(&state.document, id),
                    version: state.version,
                    editable: state.editable,
                    is_default: *id == self.default_workflow_id,
                })
                .collect(),
        }
    }

    pub(crate) fn save_workflow(
        &mut self,
        expected_version: u64,
        workflow: Value,
    ) -> Result<u64, String> {
        let workflow_id = self.default_workflow_id.clone();
        self.save_workflow_document(&workflow_id, expected_version, workflow)
    }

    pub(crate) fn save_workflow_document(
        &mut self,
        workflow_id: &str,
        expected_version: u64,
        workflow: Value,
    ) -> Result<u64, String> {
        let state = self.workflows.get(workflow_id).ok_or_else(|| {
            format!("workflow '{workflow_id}' does not exist in the workflow library")
        })?;
        if expected_version != state.version {
            return Err(format!(
                "workflow version conflict: expected {expected_version}, actual {}",
                state.version
            ));
        }
        if !state.editable {
            return Err(
                "stored workflow uses an inspectable read-only schema and cannot be overwritten"
                    .into(),
            );
        }
        validate_editable_workflow_graph(&workflow)?;
        if workflow.get("id").and_then(Value::as_str) != Some(workflow_id) {
            return Err(format!(
                "workflow document id must remain '{workflow_id}' when saving this library entry"
            ));
        }
        if validate_v1_executable_catalog(&workflow).is_ok()
            && serde_json::to_vec(&workflow)
                .map_err(|error| format!("cannot encode workflow: {error}"))?
                .len()
                > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES
        {
            return Err("workflow exceeds the executable 128 KiB persistence bound".into());
        }
        let document = json_document(&workflow)?;
        let saved = self
            .repository
            .save(
                DocumentKind::Workflow,
                workflow_id,
                Some(expected_version),
                &document,
            )
            .map_err(|error| format!("cannot commit workflow: {error}"))?;
        self.workflows.insert(
            workflow_id.to_owned(),
            StoredWorkflowState {
                version: saved.version,
                document: workflow,
                editable: true,
            },
        );
        Ok(saved.version)
    }

    pub(crate) fn create_workflow(
        &mut self,
        name: &str,
        template: Option<&str>,
    ) -> Result<(String, u64), String> {
        validate_workflow_name(name)?;
        let workflow_id = self.next_custom_workflow_id()?;
        let bundled = bundled_workflow_library()?;
        let template_id = template.unwrap_or(&bundled.creation_default_template_id);
        let mut document = bundled
            .workflows
            .iter()
            .find(|candidate| candidate.template_id == template_id)
            .map(|candidate| candidate.document.clone())
            .ok_or_else(|| format!("unknown bundled workflow template '{template_id}'"))?;
        document["id"] = Value::String(workflow_id.clone());
        document["name"] = Value::String(name.to_owned());
        let saved = self
            .repository
            .save(
                DocumentKind::Workflow,
                &workflow_id,
                None,
                &json_document(&document)?,
            )
            .map_err(|error| format!("cannot create workflow: {error}"))?;
        self.workflows.insert(
            workflow_id.clone(),
            StoredWorkflowState {
                version: saved.version,
                document,
                editable: true,
            },
        );
        Ok((workflow_id, saved.version))
    }

    pub(crate) fn rename_workflow(&mut self, workflow_id: &str, name: &str) -> Result<u64, String> {
        validate_workflow_name(name)?;
        let state = self.workflows.get(workflow_id).ok_or_else(|| {
            format!("workflow '{workflow_id}' does not exist in the workflow library")
        })?;
        if !state.editable {
            return Err("stored workflow is inspectable and read-only".into());
        }
        let mut document = state.document.clone();
        document["name"] = Value::String(name.to_owned());
        let saved = self
            .repository
            .save(
                DocumentKind::Workflow,
                workflow_id,
                Some(state.version),
                &json_document(&document)?,
            )
            .map_err(|error| format!("cannot rename workflow: {error}"))?;
        self.workflows.insert(
            workflow_id.to_owned(),
            StoredWorkflowState {
                version: saved.version,
                document,
                editable: true,
            },
        );
        Ok(saved.version)
    }

    pub(crate) fn duplicate_workflow(
        &mut self,
        workflow_id: &str,
        name: &str,
    ) -> Result<(String, u64), String> {
        validate_workflow_name(name)?;
        let state = self.workflows.get(workflow_id).ok_or_else(|| {
            format!("workflow '{workflow_id}' does not exist in the workflow library")
        })?;
        if !state.editable {
            return Err("stored workflow is inspectable and cannot be duplicated".into());
        }
        let new_id = self.next_custom_workflow_id()?;
        let mut document = state.document.clone();
        document["id"] = Value::String(new_id.clone());
        document["name"] = Value::String(name.to_owned());
        let saved = self
            .repository
            .save(
                DocumentKind::Workflow,
                &new_id,
                None,
                &json_document(&document)?,
            )
            .map_err(|error| format!("cannot duplicate workflow: {error}"))?;
        self.workflows.insert(
            new_id.clone(),
            StoredWorkflowState {
                version: saved.version,
                document,
                editable: true,
            },
        );
        Ok((new_id, saved.version))
    }

    pub(crate) fn delete_workflow(&mut self, workflow_id: &str) -> Result<(), String> {
        if self.workflows.len() <= 1 {
            return Err("at least one workflow must remain in the workflow library".into());
        }
        let state = self.workflows.get(workflow_id).ok_or_else(|| {
            format!("workflow '{workflow_id}' does not exist in the workflow library")
        })?;
        self.repository
            .delete(DocumentKind::Workflow, workflow_id, Some(state.version))
            .map_err(|error| format!("cannot delete workflow: {error}"))?;
        self.workflows.remove(workflow_id);
        if self.default_workflow_id == workflow_id {
            let fallback = self
                .workflows
                .keys()
                .next()
                .expect("at least one workflow remains")
                .clone();
            self.persist_default_workflow(&fallback)?;
        }
        Ok(())
    }

    pub(crate) fn set_default_workflow(&mut self, workflow_id: &str) -> Result<u64, String> {
        if !self.workflows.contains_key(workflow_id) {
            return Err(format!(
                "workflow '{workflow_id}' does not exist in the workflow library"
            ));
        }
        if self.default_workflow_id == workflow_id {
            return Ok(self.library_version);
        }
        self.persist_default_workflow(workflow_id)
    }

    fn persist_default_workflow(&mut self, workflow_id: &str) -> Result<u64, String> {
        let library = json!({
            "schemaVersion": WORKFLOW_LIBRARY_SCHEMA_VERSION,
            "defaultWorkflowId": workflow_id,
        });
        let saved = self
            .repository
            .save(
                DocumentKind::Configuration,
                WORKFLOW_LIBRARY_ID,
                (self.library_version > 0).then_some(self.library_version),
                &json_document(&library)?,
            )
            .map_err(|error| format!("cannot commit workflow library default: {error}"))?;
        self.library_version = saved.version;
        self.default_workflow_id = workflow_id.to_owned();
        Ok(saved.version)
    }

    fn next_custom_workflow_id(&self) -> Result<String, String> {
        for index in 1..=10_000_u32 {
            let candidate = format!("workflow.custom.{index}");
            if !self.workflows.contains_key(&candidate)
                && StableId::parse(candidate.clone()).is_ok()
            {
                return Ok(candidate);
            }
        }
        Err("workflow library id space is exhausted".into())
    }

    pub(crate) fn require_executable_workflow(&self, workflow_id: &str) -> Result<(), String> {
        let workflow = self
            .workflows
            .get(workflow_id)
            .map(|state| state.document.clone())
            .unwrap_or(Value::Null);
        validate_v1_executable_catalog(&workflow)?;
        if serde_json::to_vec(&workflow)
            .map_err(|error| format!("cannot encode workflow: {error}"))?
            .len()
            > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES
        {
            return Err("workflow exceeds the executable 128 KiB persistence bound".into());
        }
        Ok(())
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
                | settings.normalize_legacy_project_tool_limits()
                | settings.reconcile_builtin_tools();
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
            capabilities: vec!["text".into(), "tools".into()],
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

fn load_or_create_workflows(
    repository: &RepositoryRoot,
    bundled: &BundledWorkflowLibrary,
) -> Result<(BTreeMap<String, StoredWorkflowState>, bool), String> {
    let mut workflows = BTreeMap::new();
    for (id, version, _schema) in repository
        .list(DocumentKind::Workflow)
        .map_err(|error| format!("cannot list workflow library: {error}"))?
    {
        let Some(stored) = repository
            .load(DocumentKind::Workflow, &id)
            .map_err(|error| format!("cannot load workflow '{id}': {error}"))?
        else {
            continue;
        };
        let editable = stored.access == DocumentAccessMode::Editable;
        let mut document = stored_value(stored, "workflow")?;
        let migrated = if editable {
            let rescue_migrated = migrate_rescue_model_node(&mut document);
            let plan_migrated = migrate_standard_agent_plan_contract(&mut document);
            rescue_migrated || plan_migrated
        } else {
            false
        };
        if migrated {
            let saved = repository
                .save(
                    DocumentKind::Workflow,
                    &id,
                    Some(version),
                    &json_document(&document)?,
                )
                .map_err(|error| format!("cannot migrate built-in workflow contract: {error}"))?;
            workflows.insert(
                id,
                StoredWorkflowState {
                    version: saved.version,
                    document,
                    editable: true,
                },
            );
        } else {
            workflows.insert(
                id,
                StoredWorkflowState {
                    version,
                    document,
                    editable,
                },
            );
        }
    }
    let fresh_profile = workflows.is_empty();
    if fresh_profile {
        for template in bundled
            .workflows
            .iter()
            .filter(|template| template.seed_on_fresh_profile)
        {
            let document = template.document.clone();
            let workflow_id = document
                .get("id")
                .and_then(Value::as_str)
                .expect("validated bundled workflow id");
            let workflow_name = workflow_display_name(&document, workflow_id);
            let saved = repository
                .save(
                    DocumentKind::Workflow,
                    workflow_id,
                    None,
                    &json_document(&document)?,
                )
                .map_err(|error| {
                    format!("cannot create bundled workflow '{workflow_name}': {error}")
                })?;
            workflows.insert(
                workflow_id.to_owned(),
                StoredWorkflowState {
                    version: saved.version,
                    document,
                    editable: true,
                },
            );
        }
    }
    Ok((workflows, fresh_profile))
}

/// Loads or creates the small secret-free workflow-library configuration
/// document holding the per-profile default workflow selection. A fresh
/// profile takes its initial selection from the checked-in JSON bundle; an
/// existing profile without a library document keeps its first stored entry.
fn load_or_create_workflow_library(
    repository: &RepositoryRoot,
    workflows: &BTreeMap<String, StoredWorkflowState>,
    bundled: &BundledWorkflowLibrary,
    fresh_profile: bool,
) -> Result<(u64, String), String> {
    match repository
        .load(DocumentKind::Configuration, WORKFLOW_LIBRARY_ID)
        .map_err(|error| format!("cannot load workflow library: {error}"))?
    {
        Some(stored) => {
            let value = stored_value(stored, "workflow library")?;
            let default_workflow_id = value
                .get("defaultWorkflowId")
                .and_then(Value::as_str)
                .filter(|id| StableId::parse((*id).to_owned()).is_ok())
                .ok_or_else(|| {
                    "stored workflow library has no valid defaultWorkflowId".to_owned()
                })?;
            Ok((1, default_workflow_id.to_owned()))
        }
        None => {
            let default_workflow_id = if fresh_profile
                && workflows.contains_key(&bundled.default_workflow_id)
            {
                bundled.default_workflow_id.clone()
            } else {
                workflows
                    .keys()
                    .next()
                    .ok_or_else(|| "workflow library contains no workflow documents".to_owned())?
                    .clone()
            };
            let library = json!({
                "schemaVersion": WORKFLOW_LIBRARY_SCHEMA_VERSION,
                "defaultWorkflowId": &default_workflow_id,
            });
            let saved = repository
                .save(
                    DocumentKind::Configuration,
                    WORKFLOW_LIBRARY_ID,
                    None,
                    &json_document(&library)?,
                )
                .map_err(|error| format!("cannot create workflow library: {error}"))?;
            Ok((saved.version, default_workflow_id))
        }
    }
}

fn bundled_workflow_library() -> Result<BundledWorkflowLibrary, String> {
    let bundled: BundledWorkflowLibrary = serde_json::from_str(BUNDLED_WORKFLOW_LIBRARY_JSON)
        .map_err(|error| format!("bundled workflow JSON is invalid: {error}"))?;
    if bundled.schema_version != WORKFLOW_LIBRARY_SCHEMA_VERSION {
        return Err(format!(
            "bundled workflow library schema {} is unsupported",
            bundled.schema_version
        ));
    }
    let mut template_ids = BTreeSet::new();
    let mut workflow_ids = BTreeSet::new();
    for template in &bundled.workflows {
        if StableId::parse(template.template_id.clone()).is_err()
            || !template_ids.insert(template.template_id.clone())
        {
            return Err("bundled workflow template IDs must be unique StableIds".into());
        }
        validate_editable_workflow_graph(&template.document)
            .map_err(|error| format!("bundled workflow '{}': {error}", template.template_id))?;
        let workflow_id = template
            .document
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "bundled workflow '{}' has no document id",
                    template.template_id
                )
            })?;
        if !workflow_ids.insert(workflow_id.to_owned()) {
            return Err(format!(
                "bundled workflow document id '{workflow_id}' is duplicated"
            ));
        }
        if template.seed_on_fresh_profile {
            validate_v1_executable_catalog(&template.document)
                .map_err(|error| format!("bundled workflow '{}': {error}", template.template_id))?;
        }
    }
    if !bundled.workflows.iter().any(|template| {
        template.seed_on_fresh_profile
            && template.document.get("id").and_then(Value::as_str)
                == Some(bundled.default_workflow_id.as_str())
    }) {
        return Err("bundled defaultWorkflowId must reference a seeded workflow document".into());
    }
    if !bundled
        .workflows
        .iter()
        .any(|template| template.template_id == bundled.creation_default_template_id)
    {
        return Err("bundled creationDefaultTemplateId must reference a workflow template".into());
    }
    Ok(bundled)
}

#[cfg(test)]
pub(crate) fn bundled_workflow_template(template_id: &str) -> Result<Value, String> {
    bundled_workflow_library()?
        .workflows
        .into_iter()
        .find(|template| template.template_id == template_id)
        .map(|template| template.document)
        .ok_or_else(|| format!("unknown bundled workflow template '{template_id}'"))
}

fn validate_workflow_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("workflow name is required".into());
    }
    if trimmed.len() > MAXIMUM_WORKFLOW_NAME_BYTES || trimmed.contains('\0') {
        return Err(format!(
            "workflow name must be at most {MAXIMUM_WORKFLOW_NAME_BYTES} bytes without NUL"
        ));
    }
    Ok(())
}

fn workflow_display_name(document: &Value, fallback_id: &str) -> String {
    document
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_id.to_owned())
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

fn exact_legacy_rescue_graph(document: &Value, middle_id: &str, middle_type: &str) -> bool {
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
        || !exact_legacy_rescue_graph(document, "model.1", "model")
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
            Value::String("Migrated legacy Chat graph.".into()),
        );
    }
    true
}

const LEGACY_STANDARD_PLAN_INSTRUCTIONS: &str = "Analyze the user request and produce a concise plan before acting. Note open questions, needed evidence, and the intended tool order.";
const STRUCTURED_STANDARD_PLAN_INSTRUCTIONS: &str = "Analyze the user request without answering it. Return only one JSON object with exactly these fields: goal (string), openQuestions (string array), evidenceNeeded (string array), and toolOrder (non-empty string array).";

/// Migrates only the exact legacy bundled Plan prompt. Any user-edited Plan
/// instructions remain untouched and can opt into the contract explicitly.
fn migrate_standard_agent_plan_contract(document: &mut Value) -> bool {
    if document.get("id").and_then(Value::as_str) != Some("workflow.standard-agent") {
        return false;
    }
    let Some(configuration) = document
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .and_then(|nodes| {
            nodes.iter_mut().find(|node| {
                node.get("id").and_then(Value::as_str) == Some("plan.1")
                    && node.get("type").and_then(Value::as_str) == Some("model_call")
            })
        })
        .and_then(|node| node.get_mut("configuration"))
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    if configuration.get("instructions").and_then(Value::as_str)
        != Some(LEGACY_STANDARD_PLAN_INSTRUCTIONS)
    {
        return false;
    }
    configuration.insert(
        "instructions".into(),
        Value::String(STRUCTURED_STANDARD_PLAN_INSTRUCTIONS.into()),
    );
    configuration.insert("outputContract".into(), Value::String("plan".into()));
    true
}

/// The closed v1 executable catalog: known node types, per-type configuration
/// contracts, acyclic structure, reachability, and condition-route labels.
/// Unknown node types and inert configuration are preserved by the editor but
/// block execution. Tool bindings are checked against the built-in id set;
/// Settings enablement is enforced separately at freeze time.
pub(crate) fn validate_v1_executable_catalog(document: &Value) -> Result<(), String> {
    validate_editable_workflow_graph(document)?;
    let nodes = document["nodes"]
        .as_array()
        .expect("validated workflow nodes");
    let edges = document["edges"]
        .as_array()
        .expect("validated workflow edges");
    let mut node_ids = BTreeSet::new();
    for node in nodes {
        let object = node.as_object().expect("validated workflow node object");
        let node_id = object
            .get("id")
            .and_then(Value::as_str)
            .expect("validated workflow node id");
        let node_type = object
            .get("type")
            .and_then(Value::as_str)
            .expect("validated workflow node type");
        node_ids.insert(node_id.to_owned());
        if !KNOWN_NODE_TYPES.contains(&node_type) {
            return Err(format!(
                "workflow node '{node_id}' has node type '{node_type}' with no installed executor in this build"
            ));
        }
        let configuration = object.get("configuration").cloned().unwrap_or(json!({}));
        let config = configuration
            .as_object()
            .expect("validated workflow configuration object");
        match node_type {
            "input" | "output" | "wait" | "completion" | "parallel" => {
                if !config.is_empty() {
                    return Err(format!(
                        "workflow node '{node_id}' of type {node_type} accepts no configuration"
                    ));
                }
            }
            "agent" => validate_agent_configuration(node_id, config)?,
            "model_call" => validate_model_call_configuration(node_id, config)?,
            "tool" => validate_tool_configuration(node_id, config)?,
            "condition" => validate_condition_configuration(node_id, config)?,
            "approval" => validate_approval_configuration(node_id, config)?,
            _ => unreachable!("catalog node type"),
        }
        validate_declared_ports(node_id, object)?;
    }

    // Structural execution contract: exactly one input entry, at least one
    // terminal (wait or completion), no cycles, and every node on a path from
    // the input to a terminal.
    let input_ids: Vec<&str> = nodes
        .iter()
        .filter(|node| node.get("type").and_then(Value::as_str) == Some("input"))
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .collect();
    if input_ids.len() != 1 {
        return Err("an executable v1 workflow requires exactly one input node".into());
    }
    let terminal_ids: BTreeSet<&str> = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.get("type").and_then(Value::as_str),
                Some("wait" | "completion")
            )
        })
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .collect();
    if terminal_ids.is_empty() {
        return Err("an executable v1 workflow requires a wait or completion node".into());
    }
    let successors: BTreeMap<&str, Vec<&str>> = {
        let mut map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in nodes {
            let id = node.get("id").and_then(Value::as_str).expect("node id");
            map.entry(id).or_default();
        }
        for edge in edges {
            let source = edge
                .get("source")
                .and_then(Value::as_str)
                .expect("validated edge source");
            let target = edge
                .get("target")
                .and_then(Value::as_str)
                .expect("validated edge target");
            map.entry(source).or_default().push(target);
        }
        map
    };
    if has_cycle(&successors) {
        return Err("an executable v1 workflow graph must be acyclic".into());
    }
    let entry = input_ids[0];
    let mut reachable = BTreeSet::from([entry]);
    let mut frontier = vec![entry];
    while let Some(id) = frontier.pop() {
        for next in successors.get(id).into_iter().flatten() {
            if reachable.insert(next) {
                frontier.push(next);
            }
        }
    }
    if reachable.len() != node_ids.len() {
        return Err(
            "an executable v1 workflow requires every node to be reachable from the input node"
                .into(),
        );
    }

    // Condition nodes must declare exactly one true and one false (or
    // fallback) outgoing route.
    for node in nodes {
        if node.get("type").and_then(Value::as_str) != Some("condition") {
            continue;
        }
        let id = node.get("id").and_then(Value::as_str).expect("node id");
        let mut routes = BTreeSet::new();
        for edge in edges {
            if edge.get("source").and_then(Value::as_str) == Some(id) {
                let route = edge
                    .get("configuration")
                    .and_then(|configuration| configuration.get("route"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "transition leaving condition node '{id}' requires configuration.route of true, false, or fallback"
                        )
                    })?;
                if !matches!(route, "true" | "false" | "fallback") {
                    return Err(format!(
                        "transition leaving condition node '{id}' has unsupported route '{route}'"
                    ));
                }
                routes.insert(route);
            }
        }
        if !routes.contains("true") || !routes.contains("false") {
            return Err(format!(
                "condition node '{id}' requires one true route and one false or fallback route"
            ));
        }
    }
    Ok(())
}

/// The tool binding ids this build can execute. MCP tools match the mcp:
/// prefix and are resolved to an enabled, core-attested server at freeze.
pub(crate) fn builtin_tool_binding_ids() -> BTreeSet<String> {
    [
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn is_tool_binding_id(value: &str) -> bool {
    value.starts_with("tool.") || value.starts_with("mcp:")
}

fn validate_declared_ports(
    node_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    for port_key in ["inputPorts", "outputPorts"] {
        let Some(ports) = object.get(port_key) else {
            continue;
        };
        let ports = ports
            .as_array()
            .ok_or_else(|| format!("workflow node '{node_id}' {port_key} must be an array"))?;
        for (index, port) in ports.iter().enumerate() {
            let port = port.as_object().ok_or_else(|| {
                format!("workflow node '{node_id}' {port_key}[{index}] must be an object")
            })?;
            port.get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "workflow node '{node_id}' {port_key}[{index}] requires a non-empty name"
                    )
                })?;
        }
    }
    Ok(())
}

fn configuration_keys(config: &serde_json::Map<String, Value>) -> BTreeSet<&str> {
    config.keys().map(String::as_str).collect()
}

fn validate_agent_configuration(
    node_id: &str,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let keys = configuration_keys(config);
    let required = BTreeSet::from(["maxTurns", "modelTierId", "toolIds"]);
    let allowed = BTreeSet::from([
        "instructions",
        "maxTurns",
        "modelTierId",
        "timeoutSeconds",
        "toolIds",
    ]);
    if !required.is_subset(&keys) || !keys.is_subset(&allowed) {
        return Err(format!(
            "workflow node '{node_id}' agent configuration accepts exactly modelTierId, toolIds, maxTurns, and optional timeoutSeconds and instructions"
        ));
    }
    if config
        .get("modelTierId")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("tier:"))
        .is_none()
    {
        return Err(format!(
            "workflow node '{node_id}' agent modelTierId must reference a tier:<name> model tier"
        ));
    }
    let tool_ids = config
        .get("toolIds")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("workflow node '{node_id}' agent toolIds must be an array"))?;
    let mut seen = BTreeSet::new();
    for tool_id in tool_ids {
        let tool_id = tool_id
            .as_str()
            .filter(|value| is_tool_binding_id(value))
            .ok_or_else(|| {
                format!(
                    "workflow node '{node_id}' agent toolIds must reference tool.<name> or mcp:<server> bindings"
                )
            })?;
        if !builtin_tool_binding_ids().contains(tool_id) && !tool_id.starts_with("mcp:") {
            return Err(format!(
                "workflow node '{node_id}' agent binds tool '{tool_id}' with no installed executor in this build"
            ));
        }
        if !seen.insert(tool_id) {
            return Err(format!(
                "workflow node '{node_id}' agent toolIds must be unique"
            ));
        }
    }
    let maximum_turns = config
        .get("maxTurns")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("workflow node '{node_id}' agent maxTurns must be an integer"))?;
    if !(MINIMUM_AGENT_TURNS..=MAXIMUM_AGENT_TURNS).contains(&maximum_turns) {
        return Err(format!(
            "workflow node '{node_id}' agent maxTurns must be {MINIMUM_AGENT_TURNS}..={MAXIMUM_AGENT_TURNS}"
        ));
    }
    if let Some(timeout_seconds) = config.get("timeoutSeconds") {
        let timeout_seconds = timeout_seconds.as_u64().ok_or_else(|| {
            format!("workflow node '{node_id}' agent timeoutSeconds must be an integer")
        })?;
        if !(30..=3_600).contains(&timeout_seconds) {
            return Err(format!(
                "workflow node '{node_id}' agent timeoutSeconds must be 30..=3600"
            ));
        }
    }
    validate_optional_instructions(node_id, config.get("instructions"))?;
    Ok(())
}

fn validate_model_call_configuration(
    node_id: &str,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let keys = configuration_keys(config);
    let required = BTreeSet::from(["modelTierId"]);
    let allowed = BTreeSet::from([
        "instructions",
        "maximumTokens",
        "modelTierId",
        "outputContract",
    ]);
    if !required.is_subset(&keys) || !keys.is_subset(&allowed) {
        return Err(format!(
            "workflow node '{node_id}' model_call configuration accepts exactly modelTierId plus optional instructions, maximumTokens, and outputContract"
        ));
    }
    if config
        .get("modelTierId")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("tier:"))
        .is_none()
    {
        return Err(format!(
            "workflow node '{node_id}' model_call modelTierId must reference a tier:<name> model tier"
        ));
    }
    if config
        .get("outputContract")
        .is_some_and(|contract| contract.as_str() != Some("plan"))
    {
        return Err(format!(
            "workflow node '{node_id}' model_call outputContract must be 'plan' when present"
        ));
    }
    if let Some(tokens) = config.get("maximumTokens") {
        let tokens = tokens
            .as_u64()
            .ok_or_else(|| format!("workflow node '{node_id}' maximumTokens must be an integer"))?;
        if tokens == 0 || tokens > MAXIMUM_MODEL_CALL_TOKENS {
            return Err(format!(
                "workflow node '{node_id}' maximumTokens must be 1..={MAXIMUM_MODEL_CALL_TOKENS}"
            ));
        }
    }
    validate_optional_instructions(node_id, config.get("instructions"))?;
    Ok(())
}

fn validate_tool_configuration(
    node_id: &str,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let keys = configuration_keys(config);
    let required = BTreeSet::from(["toolId"]);
    let allowed = BTreeSet::from(["parameters", "toolId"]);
    if !required.is_subset(&keys) || !keys.is_subset(&allowed) {
        return Err(format!(
            "workflow node '{node_id}' tool configuration accepts exactly toolId plus optional parameters"
        ));
    }
    config
        .get("toolId")
        .and_then(Value::as_str)
        .filter(|value| is_tool_binding_id(value))
        .ok_or_else(|| {
            format!(
                "workflow node '{node_id}' tool toolId must reference a tool.<name> or mcp:<server> binding"
            )
        })?;
    let tool_id = config
        .get("toolId")
        .and_then(Value::as_str)
        .expect("validated toolId");
    if !builtin_tool_binding_ids().contains(tool_id) && !tool_id.starts_with("mcp:") {
        return Err(format!(
            "workflow node '{node_id}' tool binds '{tool_id}' with no installed executor in this build"
        ));
    }
    if config
        .get("parameters")
        .is_some_and(|parameters| !parameters.is_object())
    {
        return Err(format!(
            "workflow node '{node_id}' tool parameters must be a JSON object"
        ));
    }
    Ok(())
}

fn validate_condition_configuration(
    node_id: &str,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let keys = configuration_keys(config);
    let allowed = BTreeSet::from(["predicate"]);
    if !keys.is_subset(&allowed) || !config.contains_key("predicate") {
        return Err(format!(
            "workflow node '{node_id}' condition configuration accepts exactly a predicate object"
        ));
    }
    validate_predicate(
        node_id,
        config.get("predicate").expect("predicate present"),
        0,
    )
}

fn validate_predicate(node_id: &str, value: &Value, depth: u32) -> Result<(), String> {
    if depth > 4 {
        return Err(format!(
            "workflow node '{node_id}' predicate nesting exceeds 4 levels"
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("workflow node '{node_id}' predicate must be an object"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("workflow node '{node_id}' predicate requires a kind"))?;
    if !matches!(
        kind,
        "always" | "exists" | "eq" | "neq" | "and" | "or" | "not"
    ) {
        return Err(format!(
            "workflow node '{node_id}' predicate kind '{kind}' is unsupported"
        ));
    }
    if matches!(kind, "eq" | "neq") && !object.contains_key("value") {
        return Err(format!(
            "workflow node '{node_id}' predicate kind {kind} requires a comparison value"
        ));
    }
    if matches!(kind, "exists") && !object.contains_key("path") {
        return Err(format!(
            "workflow node '{node_id}' predicate kind exists requires a path"
        ));
    }
    if matches!(kind, "and" | "or") {
        let operands = object
            .get("operands")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!("workflow node '{node_id}' predicate kind {kind} requires operands")
            })?;
        if operands.is_empty() || operands.len() > 8 {
            return Err(format!(
                "workflow node '{node_id}' predicate operands must contain 1..=8 items"
            ));
        }
        for operand in operands {
            validate_predicate(node_id, operand, depth + 1)?;
        }
    }
    if kind == "not" {
        let operand = object.get("operand").ok_or_else(|| {
            format!("workflow node '{node_id}' predicate kind not requires operand")
        })?;
        validate_predicate(node_id, operand, depth + 1)?;
    }
    Ok(())
}

fn validate_approval_configuration(
    node_id: &str,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let keys = configuration_keys(config);
    let allowed = BTreeSet::from(["message", "title"]);
    if !keys.is_subset(&allowed) {
        return Err(format!(
            "workflow node '{node_id}' approval configuration accepts only title and message"
        ));
    }
    for (key, maximum) in [("title", 4 * 1024_usize), ("message", 16 * 1024_usize)] {
        if let Some(value) = config.get(key) {
            let text = value.as_str().ok_or_else(|| {
                format!("workflow node '{node_id}' approval {key} must be a string")
            })?;
            if text.len() > maximum || text.contains('\0') {
                return Err(format!(
                    "workflow node '{node_id}' approval {key} exceeds its bound"
                ));
            }
        }
    }
    Ok(())
}

fn validate_optional_instructions(
    node_id: &str,
    instructions: Option<&Value>,
) -> Result<(), String> {
    let Some(instructions) = instructions else {
        return Ok(());
    };
    let instructions = instructions
        .as_str()
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= MAXIMUM_MODEL_CALL_INSTRUCTIONS_BYTES
                && !value.contains('\0')
        })
        .ok_or_else(|| {
            format!(
                "workflow node '{node_id}' instructions must be a non-empty string of at most {} KiB",
                MAXIMUM_MODEL_CALL_INSTRUCTIONS_BYTES / 1024
            )
        })?;
    debug_assert!(!instructions.is_empty());
    Ok(())
}

fn has_cycle(successors: &BTreeMap<&str, Vec<&str>>) -> bool {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Visit {
        Unseen,
        Visiting,
        Done,
    }
    fn visit(
        id: &str,
        successors: &BTreeMap<&str, Vec<&str>>,
        states: &mut BTreeMap<String, Visit>,
    ) -> bool {
        match states.get(id).copied().unwrap_or(Visit::Unseen) {
            Visit::Done => false,
            Visit::Visiting => true,
            Visit::Unseen => {
                states.insert(id.to_owned(), Visit::Visiting);
                for next in successors.get(id).into_iter().flatten() {
                    if visit(next, successors, states) {
                        return true;
                    }
                }
                states.insert(id.to_owned(), Visit::Done);
                false
            }
        }
    }
    let mut states = BTreeMap::new();
    successors
        .keys()
        .any(|id| visit(id, successors, &mut states))
}

#[cfg(test)]
mod tests {
    use crate::runtime::settings_v2::{IntegrationTransportV2, McpServerConfigurationV2};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn every_seeded_json_workflow_is_executable() {
        let bundled = bundled_workflow_library().unwrap();
        for template in bundled
            .workflows
            .iter()
            .filter(|template| template.seed_on_fresh_profile)
        {
            validate_v1_executable_catalog(&template.document).unwrap();
        }
    }

    #[test]
    fn oversized_workflow_is_preserved_as_a_draft_but_never_saved_as_executable() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let mut oversized = documents.workflow_snapshot().document;
        oversized["preservedMetadata"] = Value::String("x".repeat(MAXIMUM_WORKFLOW_SNAPSHOT_BYTES));

        let error = documents.save_workflow(1, oversized).unwrap_err();
        assert!(error.contains("executable 128 KiB persistence bound"));
        assert_eq!(documents.workflow_snapshot().version, 1);
        let default_id = documents.workflow_library().default_workflow_id;
        assert!(documents.require_executable_workflow(&default_id).is_ok());
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
                legacy["id"].as_str().unwrap(),
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
        validate_v1_executable_catalog(&workflow.document).unwrap();
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
        assert_eq!(
            migrated.settings.providers[0].models[0].capabilities,
            vec!["text", "tools"]
        );
        assert_eq!(migrated.settings.credentials[0].revision, 3);
        assert_eq!(
            migrated.settings.projects[0].workspace.location,
            "/workspace/atlas"
        );
        assert_eq!(migrated.settings.tools.len(), 12);
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
    fn persisted_v2_settings_gain_new_builtin_tools_disabled_on_open() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let mut settings = SettingsConfigurationV2::default();
        // Simulate a document written before tool.subagent existed: drop the
        // newest built-in entry while preserving one user-enabled entry.
        settings.tools.retain(|tool| tool.id != "tool.subagent");
        assert_eq!(settings.tools.len(), 11);
        settings.tools[3].enabled = true;
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
        assert_eq!(repaired.settings.tools.len(), 12);
        assert!(
            repaired
                .settings
                .tools
                .iter()
                .any(|tool| tool.id == "tool.subagent" && !tool.enabled)
        );
        assert!(repaired.settings.tools.iter().any(|tool| tool.enabled));
        drop(repaired);

        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.settings.tools.len(), 12);
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
        let workflow_id = "workflow.future";
        let raw = br#"{
            "schemaVersion": 2,
            "id": "workflow.future",
            "name": "Future workflow",
            "nodes": [{"id":"future.1","type":"future@2","configuration":{"new":true}}],
            "edges": [],
            "futureRoot": {"retained": true}
        }"#;
        repository
            .import_inert(
                DocumentKind::Workflow,
                workflow_id,
                None,
                &JsonDocument::parse(raw.to_vec()).unwrap(),
            )
            .unwrap();

        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let snapshot = documents.workflow_snapshot_for(workflow_id);
        assert_eq!(snapshot.version, 1);
        assert!(!snapshot.editable);
        assert_eq!(snapshot.document["futureRoot"]["retained"], json!(true));
        assert!(documents.require_executable_workflow(workflow_id).is_err());
        let error = documents
            .save_workflow_document(workflow_id, snapshot.version, snapshot.document)
            .unwrap_err();
        assert!(error.contains("inspectable read-only"));
        drop(documents);

        assert_eq!(
            repository
                .export_lossless(DocumentKind::Workflow, workflow_id)
                .unwrap()
                .unwrap(),
            raw
        );
    }

    #[test]
    fn arbitrary_structurally_valid_v1_workflow_saves_reloads_and_stays_non_runnable() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let workflow_id = documents.workflow_library().default_workflow_id;
        let mut arbitrary = documents.workflow_snapshot_for(&workflow_id).document;
        arbitrary["name"] = json!("Advanced future harness");
        arbitrary["nodes"] = json!([
            {
                "id": "future.1",
                "type": "plugin.future@2",
                "position": {"x": 17, "y": 29, "futureLayout": [1, 2]},
                "configuration": {"futureOption": {"retained": true}},
                "capabilityStatus": "missing",
                "futureNode": {"retained": true}
            },
            {"id": "output.custom", "type": "output"}
        ]);
        arbitrary["edges"] = json!([{
            "id": "future-edge",
            "source": "future.1",
            "target": "output.custom",
            "sourcePort": "future.out",
            "targetPort": "future.in",
            "futureEdge": {"retained": true}
        }]);
        arbitrary["futureRoot"] = json!({"retained": true});
        assert_eq!(documents.save_workflow(1, arbitrary.clone()).unwrap(), 2);
        assert_eq!(
            documents.workflow_snapshot_for(&workflow_id).document,
            arbitrary
        );
        assert!(documents.workflow_snapshot_for(&workflow_id).editable);
        assert!(documents.require_executable_workflow(&workflow_id).is_err());

        drop(documents);
        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.workflow_snapshot_for(&workflow_id).version, 2);
        assert_eq!(
            reopened.workflow_snapshot_for(&workflow_id).document,
            arbitrary
        );
        assert!(reopened.workflow_snapshot_for(&workflow_id).editable);
        assert!(reopened.require_executable_workflow(&workflow_id).is_err());
    }

    #[test]
    fn workflow_save_rejects_broken_references_without_replacing_the_document() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        let workflow_id = documents.workflow_library().default_workflow_id;
        let mut broken = documents.workflow_snapshot_for(&workflow_id).document;
        broken["nodes"] = json!([{"id": "input", "type": "input"}]);
        broken["edges"] = json!([{"id": "broken", "source": "input", "target": "missing"}]);
        let error = documents.save_workflow(1, broken).unwrap_err();
        assert!(error.contains("target 'missing' does not exist"));
        assert_eq!(documents.workflow_snapshot().version, 1);
        assert!(documents.require_executable_workflow(&workflow_id).is_ok());
    }

    #[test]
    fn duplicate_nodes_are_rejected_for_every_workflow_shape() {
        let mut duplicate = bundled_workflow_template("simple-chat").unwrap();
        duplicate["nodes"] = json!([
            {"id":"input.1","type":"input"},
            {"id":"input.1","type":"input"},
            {"id":"output.1","type":"output"},
            {"id":"wait.1","type":"wait"}
        ]);
        assert!(validate_editable_workflow_graph(&duplicate).is_err());
    }

    #[test]
    fn duplicate_edges_are_rejected_for_every_workflow_shape() {
        let mut duplicate = bundled_workflow_template("simple-chat").unwrap();
        duplicate["edges"] = json!([
            {"id":"duplicate","source":"input.1","target":"agent.1"},
            {"id":"duplicate","source":"input.1","target":"agent.1"},
            {"source":"output.1","target":"wait.1"}
        ]);
        assert!(validate_editable_workflow_graph(&duplicate).is_err());
    }

    #[test]
    fn library_seeds_exactly_the_json_bundle_with_its_declared_default() {
        let root = TempDir::new().unwrap();
        let documents = CanonicalDocuments::open(root.path()).unwrap();
        let bundled = bundled_workflow_library().unwrap();
        let seeded = bundled
            .workflows
            .iter()
            .filter(|template| template.seed_on_fresh_profile)
            .collect::<Vec<_>>();
        let library = documents.workflow_library();
        assert_eq!(library.entries.len(), seeded.len());
        assert_eq!(library.default_workflow_id, bundled.default_workflow_id);
        for template in seeded {
            let id = template.document["id"].as_str().unwrap();
            assert!(library.entries.iter().any(|entry| entry.id == id));
            assert_eq!(
                documents.workflow_snapshot_for(id).document,
                template.document
            );
            assert!(documents.require_executable_workflow(id).is_ok());
        }

        drop(documents);
        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(
            reopened.workflow_library().entries.len(),
            library.entries.len()
        );
        assert_eq!(
            reopened.workflow_library().default_workflow_id,
            library.default_workflow_id
        );
    }

    #[test]
    fn exact_legacy_standard_agent_plan_is_migrated_to_the_structured_contract() {
        let mut workflow = bundled_workflow_template("standard-agent").unwrap();
        let configuration = workflow["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == "plan.1")
            .unwrap()["configuration"]
            .as_object_mut()
            .unwrap();
        configuration.insert(
            "instructions".into(),
            Value::String(LEGACY_STANDARD_PLAN_INSTRUCTIONS.into()),
        );
        configuration.remove("outputContract");

        assert!(migrate_standard_agent_plan_contract(&mut workflow));
        let configuration = workflow["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "plan.1")
            .unwrap()["configuration"]
            .as_object()
            .unwrap();
        assert_eq!(
            configuration.get("instructions").and_then(Value::as_str),
            Some(STRUCTURED_STANDARD_PLAN_INSTRUCTIONS)
        );
        assert_eq!(
            configuration.get("outputContract").and_then(Value::as_str),
            Some("plan")
        );
        assert!(!migrate_standard_agent_plan_contract(&mut workflow));
    }

    #[test]
    fn an_existing_profile_is_not_backfilled_with_bundled_workflows() {
        let root = TempDir::new().unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let workflow_id = "workflow.existing";
        let mut existing = bundled_workflow_template("blank").unwrap();
        existing["id"] = json!(workflow_id);
        existing["name"] = json!("Existing workflow");
        repository
            .save(
                DocumentKind::Workflow,
                workflow_id,
                None,
                &json_document(&existing).unwrap(),
            )
            .unwrap();
        let documents = CanonicalDocuments::open(root.path()).unwrap();
        let library = documents.workflow_library();
        assert_eq!(library.default_workflow_id, workflow_id);
        assert_eq!(library.entries.len(), 1);
        assert_eq!(library.entries[0].id, workflow_id);
    }

    #[test]
    fn library_crud_create_rename_duplicate_delete_and_set_default() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();

        let (created_id, created_version) = documents
            .create_workflow("Research loop", Some("standard-agent"))
            .unwrap();
        assert_eq!(created_id, "workflow.custom.1");
        assert_eq!(created_version, 1);
        let snapshot = documents.workflow_snapshot_for(&created_id);
        assert_eq!(snapshot.document["name"], "Research loop");
        assert_eq!(
            snapshot.document["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| node.get("type").and_then(Value::as_str) == Some("agent"))
                .count(),
            1
        );

        assert_eq!(
            documents
                .rename_workflow(&created_id, "Research v2")
                .unwrap(),
            2
        );
        assert_eq!(
            documents.workflow_snapshot_for(&created_id).document["name"],
            "Research v2"
        );

        let (duplicate_id, duplicate_version) = documents
            .duplicate_workflow(&created_id, "Research copy")
            .unwrap();
        assert_eq!(duplicate_id, "workflow.custom.2");
        assert_eq!(duplicate_version, 1);
        assert_eq!(
            documents.workflow_snapshot_for(&duplicate_id).document["name"],
            "Research copy"
        );

        let library_version = documents.set_default_workflow(&duplicate_id).unwrap();
        assert_eq!(
            documents.workflow_library().default_workflow_id,
            duplicate_id
        );

        let expected_fallback = documents
            .workflow_library()
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .filter(|id| *id != duplicate_id)
            .min()
            .unwrap()
            .to_owned();
        documents.delete_workflow(&duplicate_id).unwrap();
        assert!(
            !documents
                .workflow_library()
                .entries
                .iter()
                .any(|entry| entry.id == duplicate_id)
        );
        assert_eq!(
            documents.workflow_library().default_workflow_id,
            expected_fallback
        );
        assert_eq!(documents.workflow_library().version, library_version + 1);

        drop(documents);
        let reopened = CanonicalDocuments::open(root.path()).unwrap();
        assert_eq!(reopened.workflow_library().entries.len(), 3);
        assert!(
            !reopened
                .workflow_library()
                .entries
                .iter()
                .any(|entry| entry.id == duplicate_id)
        );
    }

    #[test]
    fn library_refuses_deleting_the_last_workflow_and_unknown_targets() {
        let root = TempDir::new().unwrap();
        let mut documents = CanonicalDocuments::open(root.path()).unwrap();
        while documents.workflow_library().entries.len() > 1 {
            let id = documents.workflow_library().entries[0].id.clone();
            documents.delete_workflow(&id).unwrap();
        }
        let last_id = documents.workflow_library().entries[0].id.clone();
        assert!(
            documents
                .delete_workflow(&last_id)
                .unwrap_err()
                .contains("at least one workflow")
        );
        assert!(
            documents
                .create_workflow("nope", Some("missing-template"))
                .is_err()
        );
        assert!(
            documents
                .set_default_workflow("workflow.missing")
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(documents.rename_workflow("workflow.missing", "x").is_err());
        assert!(
            documents
                .duplicate_workflow("workflow.missing", "x")
                .is_err()
        );
    }

    #[test]
    fn catalog_rejects_unknown_nodes_cycles_bad_conditions_and_bad_configs() {
        let unknown = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"plugin.1","type":"plugin.future@2"},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"plugin.1"},
                {"id":"e2","source":"plugin.1","target":"wait.1"}
            ]
        });
        assert!(
            validate_v1_executable_catalog(&unknown)
                .unwrap_err()
                .contains("no installed executor")
        );

        let cycle = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":2}},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"agent.1"},
                {"id":"e2","source":"agent.1","target":"input.1"},
                {"id":"e3","source":"agent.1","target":"wait.1"}
            ]
        });
        assert!(
            validate_v1_executable_catalog(&cycle)
                .unwrap_err()
                .contains("acyclic")
        );

        let unrouted_condition = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"check.1","type":"condition","configuration":{"predicate":{"kind":"always"}}},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":2}},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"check.1"},
                {"id":"e2","source":"check.1","target":"agent.1"},
                {"id":"e3","source":"agent.1","target":"wait.1"}
            ]
        });
        assert!(
            validate_v1_executable_catalog(&unrouted_condition)
                .unwrap_err()
                .contains("configuration.route")
        );

        let bad_agent = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":99}},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"agent.1"},
                {"id":"e2","source":"agent.1","target":"wait.1"}
            ]
        });
        assert!(
            validate_v1_executable_catalog(&bad_agent)
                .unwrap_err()
                .contains("maxTurns")
        );

        let unreachable = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":2}},
                {"id":"orphan.1","type":"output"},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"agent.1"},
                {"id":"e2","source":"agent.1","target":"wait.1"}
            ]
        });
        assert!(
            validate_v1_executable_catalog(&unreachable)
                .unwrap_err()
                .contains("reachable")
        );
    }

    #[test]
    fn catalog_accepts_the_standard_agent_graph_with_conditions_and_parallelism() {
        validate_v1_executable_catalog(&bundled_workflow_template("standard-agent").unwrap())
            .unwrap();
        let branched = json!({
            "schemaVersion": 1,
            "nodes": [
                {"id":"input.1","type":"input"},
                {"id":"check.1","type":"condition","configuration":{"predicate":{"kind":"exists","path":"text"}}},
                {"id":"plan.1","type":"model_call","configuration":{"modelTierId":"tier:balanced"}},
                {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":["tool.todo"],"maxTurns":6}},
                {"id":"output.1","type":"output"},
                {"id":"wait.1","type":"wait"}
            ],
            "edges": [
                {"id":"e1","source":"input.1","target":"check.1"},
                {"id":"e2","source":"check.1","target":"plan.1","configuration":{"route":"true"}},
                {"id":"e3","source":"check.1","target":"agent.1","configuration":{"route":"false"}},
                {"id":"e4","source":"plan.1","target":"agent.1"},
                {"id":"e5","source":"agent.1","target":"output.1"},
                {"id":"e6","source":"output.1","target":"wait.1"}
            ]
        });
        validate_v1_executable_catalog(&branched).unwrap();
    }
}
