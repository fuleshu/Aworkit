//! Skill configuration, durable catalog publication, and direct user invocation.

use super::*;
use aworkit_capability_host::{
    ModelToolContextV1,
    skills::{self as library, CatalogEntry, SkillConfiguration},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    include_default_roots: bool,
    aworkit_home: String,
    agents_home: String,
    custom_skill_dirs: Vec<PathBuf>,
    bundled_skill_dir: String,
    catalog_description_max_length: usize,
}

/// Blank home fields resolve at Chat freeze; persisted paths are absolute.
pub(crate) fn configuration(value: &Value) -> Result<SkillConfiguration, String> {
    let settings: Settings = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let home_path = |configured: String, suffix: &str| -> Result<PathBuf, String> {
        if configured.is_empty() {
            home.as_ref().map(|home| home.join(suffix)).ok_or_else(|| {
                "user home directory is unavailable; configure the skill home explicitly".into()
            })
        } else {
            Ok(PathBuf::from(configured))
        }
    };
    let configuration = SkillConfiguration {
        include_default_roots: settings.include_default_roots,
        aworkit_home: home_path(settings.aworkit_home, ".aworkit")?,
        agents_home: home_path(settings.agents_home, ".agents")?,
        custom_skill_dirs: settings.custom_skill_dirs,
        bundled_skill_dir: (!settings.bundled_skill_dir.is_empty())
            .then(|| PathBuf::from(settings.bundled_skill_dir)),
        catalog_description_max_length: settings.catalog_description_max_length,
    };
    configuration.validate()?;
    Ok(configuration)
}

pub(crate) fn freeze_settings(value: &Value) -> Result<Value, String> {
    let config = configuration(value)?;
    Ok(json!({
        "includeDefaultRoots": config.include_default_roots,
        "aworkitHome": config.aworkit_home,
        "agentsHome": config.agents_home,
        "customSkillDirs": config.custom_skill_dirs,
        "bundledSkillDir": config.bundled_skill_dir.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "catalogDescriptionMaxLength": config.catalog_description_max_length,
    }))
}

pub(super) fn schema() -> Value {
    super::super::tool_registry::native_tool(SKILL_CAPABILITY_ID)
        .expect("skill manifest")
        .input_schema
        .clone()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StepContext {
    outer_invocation_id: StableId,
    after_exchanges: usize,
    catalog: Option<Vec<CatalogEntry>>,
    messages: Vec<ModelToolContextV1>,
}

impl BoundFileToolAuthorityV1 {
    /// Each step records even an unchanged observation, fencing replay from live edits.
    pub(super) fn skill_context(
        &self,
        outer: &StableId,
        after_exchanges: usize,
        definitions: &[ModelToolDefinitionV1],
        direct_user: bool,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ModelToolContextV1>, String> {
        let binding = self
            .context
            .bindings
            .iter()
            .find(|b| b.capability_id == SKILL_CAPABILITY_ID);
        let Some(binding) = binding else {
            return Ok(Vec::new());
        };
        let StoredFileToolLimitV1::Skill { configuration } = &binding.limit else {
            return Err("invalid frozen skill binding".into());
        };
        let visible = definitions
            .iter()
            .any(|d| d.capability_id == SKILL_CAPABILITY_ID && d.name == "skill");
        let mut history = self
            .runtime
            .records
            .events("pipeline.skill-context")
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(serde_json::from_value::<StepContext>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        history.retain(|step| {
            step.outer_invocation_id == *outer && step.after_exchanges <= after_exchanges
        });
        history.sort_by_key(|step| step.after_exchanges);
        let mut messages = history
            .iter()
            .flat_map(|step| step.messages.clone())
            .collect::<Vec<_>>();
        if history
            .last()
            .is_some_and(|step| step.after_exchanges == after_exchanges)
        {
            return Ok(messages);
        }
        let mut catalog = history.last().and_then(|step| step.catalog.clone());
        let mut added = Vec::new();
        let snapshot = library::discover(
            configuration,
            Some(&self.context.workspace.root),
            cancellation,
        )?;
        for warning in &snapshot.warnings {
            eprintln!("skill discovery: {warning}");
        }
        if snapshot.complete {
            let entries = if visible {
                library::catalog_entries(
                    &snapshot.skills,
                    configuration.catalog_description_max_length,
                )
            } else {
                Vec::new()
            };
            if catalog.as_ref() != Some(&entries) && (catalog.is_some() || !entries.is_empty()) {
                added.push(ModelToolContextV1 {
                    after_exchanges,
                    content: library::render_catalog(&entries, catalog.is_some()),
                    ..Default::default()
                });
                catalog = Some(entries);
            }
        }
        if direct_user && after_exchanges == 0 && visible {
            if let Some(message) = self
                .context
                .review_messages
                .last()
                .filter(|m| m.role == "user")
            {
                for name in library::invoked_names(&message.content) {
                    if let Some(skill) = library::load(
                        configuration,
                        Some(&self.context.workspace.root),
                        &name,
                        true,
                        cancellation,
                    )? {
                        added.push(ModelToolContextV1 {
                            after_exchanges,
                            content: library::render_content(&skill),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        let step = StepContext {
            outer_invocation_id: outer.clone(),
            after_exchanges,
            catalog,
            messages: added.clone(),
        };
        let value = serde_json::to_value(&step).map_err(|e| e.to_string())?;
        enforce_result_bound(&value)?;
        self.runtime
            .records
            .append(
                "pipeline.skill-context",
                &digest_id(
                    "record.skill-context",
                    &format!("{outer}:{after_exchanges}"),
                )
                .map_err(|e| e.to_string())?,
                value,
            )
            .map_err(|e| e.to_string())?;
        messages.extend(added);
        Ok(messages)
    }
}

/// Keep the canonical structured host result and render the harness text for the model.
pub(crate) fn model_result(result: &ModelToolResultV1, capability_id: &str) -> ModelToolResultV1 {
    if capability_id != SKILL_CAPABILITY_ID {
        return result.clone();
    }
    let content = if result.is_error {
        let error = result
            .content
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| result.content.as_str())
            .unwrap_or("skill lookup failed");
        format!("Error: {error}")
    } else {
        match serde_json::from_value::<library::SkillContent>(result.content.clone()) {
            Ok(skill) => library::render_content(&skill),
            Err(_) => return result.clone(),
        }
    };
    ModelToolResultV1 {
        content: Value::String(content),
        ..result.clone()
    }
}
