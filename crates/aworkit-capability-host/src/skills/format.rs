//! Literal harness catalog and loaded-skill prompting.

use super::{ResourceBase, SkillContent, SkillSummary, is_skill_name};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
}

pub fn catalog_entries(skills: &[SkillSummary], maximum: usize) -> Vec<CatalogEntry> {
    skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(|s| {
            let normalized = s
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            // JavaScript strings count UTF-16 code units, not Unicode scalar values.
            let units = normalized.encode_utf16().collect::<Vec<_>>();
            let description = if units.len() > maximum {
                format!(
                    "{}...",
                    String::from_utf16_lossy(&units[..maximum.saturating_sub(3)])
                )
            } else {
                normalized
            };
            CatalogEntry {
                name: s.name.clone(),
                description,
            }
        })
        .collect()
}

pub fn render_catalog(entries: &[CatalogEntry], update: bool) -> String {
    let intro = if update {
        "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:"
    } else {
        "A skill is a reusable set of task-specific instructions. The following skills are available in this session:"
    };
    let availability = if !update {
        "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.\nA user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill."
    } else if entries.is_empty() {
        "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.\nA user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it."
    } else {
        "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting.\nA user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill."
    };
    let mut lines = vec![
        "<system-reminder>".into(),
        intro.into(),
        String::new(),
        "<available_skills>".into(),
    ];
    lines.extend(
        entries
            .iter()
            .map(|e| format!("- `{}`: {}", e.name, escape(&e.description))),
    );
    lines.extend([
        "</available_skills>".into(),
        String::new(),
        availability.into(),
        "</system-reminder>".into(),
    ]);
    lines.join("\n")
}

pub fn render_content(skill: &SkillContent) -> String {
    let ResourceBase::Directory { path } = &skill.resource_base;
    format!(
        "<skill_content name=\"{}\">\n<skill_resources>\nBase directory for this skill: {}\nResolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n</skill_resources>\n\n<skill_instructions>\n{}\n</skill_instructions>\n</skill_content>",
        skill.name,
        escape(&path.to_string_lossy()),
        skill.content
    )
}

/// Scan only direct user text, deduplicating whitespace-bounded /name gestures.
pub fn invoked_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for word in text.split_whitespace() {
        if let Some(name) = word.strip_prefix('/') {
            if is_skill_name(name) && !names.iter().any(|s| s == name) {
                names.push(name.into());
            }
        }
    }
    names
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
