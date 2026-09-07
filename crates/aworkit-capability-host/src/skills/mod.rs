//! DeepSeek Harness compatible filesystem skill discovery and lazy loading.

mod filesystem;
mod format;
mod parser;
#[cfg(test)]
mod tests;

pub use filesystem::{SkillConfiguration, SkillSnapshot, discover, load};
pub use format::{CatalogEntry, catalog_entries, invoked_names, render_catalog, render_content};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const TOOL_DESCRIPTION: &str = "Load the full instructions for an available skill. Call this with the exact skill name from the session skill catalog before acting on a task that names or clearly matches that skill.";

/// Parsed metadata and locator. Bodies are reread only when requested.
#[derive(Clone, Debug)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub model_invocable: bool,
    pub user_invocable: bool,
    pub path: PathBuf,
    pub directory: PathBuf,
}

/// The canonical tool result, before the provider-facing text renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillContent {
    pub name: String,
    pub provider: String,
    pub resource_base: ResourceBase,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ResourceBase {
    Directory { path: PathBuf },
}

/// Harness names are lowercase ASCII kebab-case, including numeric segments.
pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}
