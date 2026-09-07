//! Ranked, one-level discovery. Each pre-step rescans so edits need no watcher.

use super::{ResourceBase, SkillContent, SkillSummary, is_skill_name, parser};
use crate::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

/// Frozen search roots and catalog limit; environment defaults resolve once at freeze.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillConfiguration {
    pub include_default_roots: bool,
    pub aworkit_home: PathBuf,
    pub agents_home: PathBuf,
    pub custom_skill_dirs: Vec<PathBuf>,
    pub bundled_skill_dir: Option<PathBuf>,
    pub catalog_description_max_length: usize,
}

impl SkillConfiguration {
    pub fn validate(&self) -> Result<(), String> {
        if self.catalog_description_max_length < 3 {
            return Err(
                "catalogDescriptionMaxLength must be an integer greater than or equal to 3".into(),
            );
        }
        if self
            .custom_skill_dirs
            .iter()
            .chain([&self.aworkit_home, &self.agents_home])
            .chain(self.bundled_skill_dir.iter())
            .any(|p| !p.is_absolute())
        {
            return Err("skill roots must be absolute paths".into());
        }
        Ok(())
    }

    fn roots(&self, cwd: Option<&Path>) -> io::Result<Vec<(PathBuf, bool)>> {
        let mut roots = Vec::new();
        if self.include_default_roots {
            if let Some(cwd) = cwd {
                let mut project = cwd;
                for ancestor in cwd.ancestors() {
                    match fs::metadata(ancestor.join(".git")) {
                        Ok(_) => {
                            project = ancestor;
                            break;
                        }
                        Err(e) if absent(&e) => {}
                        Err(e) => return Err(e),
                    }
                }
                roots.push((project.join(".aworkit/skills"), false));
                roots.push((project.join(".agents/skills"), false));
            }
        }
        roots.extend(self.custom_skill_dirs.iter().cloned().map(|p| (p, false)));
        if self.include_default_roots {
            roots.push((self.aworkit_home.join("skills"), true));
            roots.push((self.agents_home.join("skills"), false));
        }
        if let Some(root) = &self.bundled_skill_dir {
            roots.push((root.clone(), false));
        }
        Ok(roots)
    }
}

#[derive(Default)]
pub struct SkillSnapshot {
    pub skills: Vec<SkillSummary>,
    pub complete: bool,
    pub warnings: Vec<String>,
}

/// Malformed files are skipped; unexpected I/O preserves the last-good catalog.
pub fn discover(
    config: &SkillConfiguration,
    cwd: Option<&Path>,
    cancel: &CancellationToken,
) -> Result<SkillSnapshot, String> {
    config.validate()?;
    let mut result = SkillSnapshot {
        complete: true,
        ..Default::default()
    };
    let roots = match config.roots(cwd) {
        Ok(roots) => roots,
        Err(e) => {
            result.complete = false;
            result.warnings.push(e.to_string());
            return Ok(result);
        }
    };
    let mut winners = BTreeMap::new();
    for (root, skip_system) in roots {
        cancelled(cancel)?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if absent(&e) => continue,
            Err(e) => {
                result.complete = false;
                result.warnings.push(format!("{}: {e}", root.display()));
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(e) => {
                    result.complete = false;
                    result.warnings.push(e.to_string());
                }
            }
        }
        paths.sort();
        for path in paths {
            cancelled(cancel)?;
            if skip_system && path.file_name().is_some_and(|s| s == ".system") {
                continue;
            }
            let info = match fs::metadata(&path) {
                Ok(info) => info,
                Err(e) if absent(&e) => continue,
                Err(e) => {
                    result.complete = false;
                    result.warnings.push(e.to_string());
                    continue;
                }
            };
            let (file, directory) = if info.is_dir() {
                (path.join("SKILL.md"), path)
            } else if info.is_file() && path.extension().is_some_and(|s| s == "md") {
                (path, root.clone())
            } else {
                continue;
            };
            let raw = match read(&file) {
                Ok(Some(raw)) => raw,
                Ok(None) => continue,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    result.warnings.push(format!("{}: {e}", file.display()));
                    continue;
                }
                Err(e) => {
                    result.complete = false;
                    result.warnings.push(format!("{}: {e}", file.display()));
                    continue;
                }
            };
            match parser::parse(&raw) {
                Ok(parsed) => {
                    let skill = SkillSummary {
                        name: parsed.name,
                        description: parsed.description,
                        model_invocable: parsed.model_invocable,
                        user_invocable: parsed.user_invocable,
                        path: file,
                        directory,
                    };
                    if winners.contains_key(&skill.name) {
                        result
                            .warnings
                            .push(format!("duplicate skill \"{}\" ignored", skill.name));
                    }
                    winners.entry(skill.name.clone()).or_insert(skill);
                }
                Err(e) => result.warnings.push(format!("{}: {e}", file.display())),
            }
        }
    }
    cancelled(cancel)?;
    result.skills = winners.into_values().collect();
    Ok(result)
}

/// Resolve a name from ranked discovery, then reread and recheck its current policy.
pub fn load(
    config: &SkillConfiguration,
    cwd: Option<&Path>,
    name: &str,
    user: bool,
    cancel: &CancellationToken,
) -> Result<Option<SkillContent>, String> {
    if !is_skill_name(name) {
        return Err(format!("invalid skill name \"{name}\""));
    }
    let snapshot = discover(config, cwd, cancel)?;
    let Some(summary) = snapshot.skills.iter().find(|s| s.name == name) else {
        return Ok(None);
    };
    if !user && !summary.model_invocable {
        return Err(format!(
            "skill \"{name}\" is not available for model invocation"
        ));
    }
    let Some(raw) = read(&summary.path).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    cancelled(cancel)?;
    let Ok(parsed) = parser::parse(&raw) else {
        return Ok(None);
    };
    if parsed.name != name || (user && !parsed.user_invocable) {
        return Ok(None);
    }
    if !user && !parsed.model_invocable {
        return Err(format!(
            "skill \"{name}\" is not available for model invocation"
        ));
    }
    Ok(Some(SkillContent {
        name: parsed.name,
        provider: "filesystem".into(),
        resource_base: ResourceBase::Directory {
            path: summary.directory.clone(),
        },
        content: parsed.body,
    }))
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::metadata(path) {
        Ok(info) if !info.is_file() => return Ok(None),
        Ok(_) => {}
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(e),
    }
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if absent(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn cancelled(token: &CancellationToken) -> Result<(), String> {
    if token.is_cancelled() {
        Err("skill lookup cancelled".into())
    } else {
        Ok(())
    }
}
