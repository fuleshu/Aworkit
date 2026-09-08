//! Stateless preparation against durable history and an independently selected
//! model context. The caller atomically admits the returned event before dispatch.

use super::{Action, Change, Configuration, FileObservation, InstructionFiles, RenderItem, check_cancelled, digest, render};
use crate::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, path::{Component, Path, PathBuf}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Owner {
    pub chat: String,
    pub node: String,
    /// Frozen branch/child execution context, never inferred from model text.
    pub context: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub producer: String,
    pub owner: Owner,
    pub identity: String,
    pub baseline: bool,
    pub reset: bool,
    pub pending: bool,
    pub text: String,
    pub changes: Vec<Change>,
    pub observed_directories: Vec<String>,
}

/// Only IDs resolved against authentic loader records establish visibility.
/// A compaction summary's prose, even with identical headings, supplies no IDs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub revision: String,
    pub event_ids: BTreeSet<String>,
}

pub struct Preparation<'a> {
    pub configuration: &'a Configuration,
    pub owner: &'a Owner,
    pub workspace: Option<&'a Path>,
    pub cwd: Option<&'a Path>,
    pub files: Option<&'a dyn InstructionFiles>,
    pub history: &'a [Event],
    pub selection: &'a Selection,
    pub event_id: &'a str,
    pub settled_touches: &'a [PathBuf],
    /// First/new Chat input or resume; a retry instead reuses its admitted plan.
    pub refresh: bool,
    pub cancellation: &'a CancellationToken,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub event: Option<Event>,
    pub retain: BTreeSet<String>,
    pub diagnostics: Vec<String>,
}

type Effective<'a> = BTreeMap<String, (&'a Event, &'a Change)>;
fn effective<'a>(events: impl Iterator<Item = &'a Event>, durable: bool) -> Effective<'a> {
    let mut state = BTreeMap::new();
    for event in events {
        if event.baseline && (!durable || event.reset) { state.clear(); }
        for change in &event.changes {
            state.insert(change.scope.clone(), (event, change));
        }
    }
    state
}

fn directories(root: &Path, target: &Path) -> Vec<PathBuf> {
    let Ok(relative) = target.strip_prefix(root) else { return Vec::new(); };
    if relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) { return Vec::new(); }
    let mut result = vec![root.to_owned()];
    let mut current = root.to_owned();
    for part in relative.components() {
        if let Component::Normal(part) = part { current.push(part); result.push(current.clone()); }
    }
    result
}
fn display(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if value.is_empty() { ".".into() } else { value }
}

/// Compute an exact, bounded delta from current files and actual model visibility.
/// Calling this is side-effect free; admission/replay belongs to the context store.
pub fn prepare(request: Preparation<'_>) -> Result<Plan, String> {
    let Preparation { configuration: config, owner, workspace, cwd, files, history, selection,
        event_id, settled_touches, refresh, cancellation } = request;
    check_cancelled(cancellation)?;
    config.validate()?;
    let mut plan = Plan { diagnostics: config.diagnostics(), ..Default::default() };
    if config.max_bytes == 0 { return Ok(plan); }
    let Some(files) = files else {
        plan.diagnostics.push("Workspace instructions unavailable: no authorized filesystem provider".into());
        return Ok(plan);
    };
    let project = workspace.zip(cwd).filter(|(root, cwd)| cwd.starts_with(root));
    let mut root = project.map(|(_, cwd)| cwd.to_owned());
    if let Some((boundary, cwd)) = project {
        'ancestors: for directory in directories(boundary, cwd).into_iter().rev() {
            for marker in config.project_root_markers.iter().filter(|s| super::config::valid_name(s)) {
                check_cancelled(cancellation)?;
                match files.exists(&directory.join(marker), cancellation) {
                    Ok(true) => { root = Some(directory); break 'ancestors; }
                    Ok(false) => {},
                    Err(error) => plan.diagnostics.push(format!("Root marker unavailable: {error}")),
                }
            }
        }
    }
    let identity = digest(&serde_json::to_string(&(config, workspace, cwd, &root)).map_err(|e| e.to_string())?);
    let owned: Vec<_> = history.iter().filter(|e| e.owner == *owner && e.producer == "aworkit.workspace_instructions.v1").collect();
    let previous_baseline = owned.iter().rev().find(|e| e.baseline).copied();
    let compatible = previous_baseline.is_some_and(|e| e.identity == identity);
    let visible: Vec<_> = owned.iter().copied().filter(|e| selection.event_ids.contains(&e.id)).collect();
    plan.retain = visible.iter().map(|e| e.id.clone()).collect();
    let visible_baseline = visible.iter().rev().find(|e| e.baseline).copied();
    let baseline = !compatible || visible_baseline.is_none_or(|e| e.identity != identity);
    let known = effective(owned.iter().copied(), true);
    let current = effective(visible.iter().copied(), false);
    let lost = known.iter().any(|(scope, (_, change))| current.get(scope).is_none_or(|(_, c)| c != change));
    let mut scopes = BTreeSet::new();
    let mut baseline_dirs = BTreeSet::new();
    if let (Some(root), Some((_, cwd))) = (&root, project) {
        for dir in directories(root, cwd) {
            baseline_dirs.insert(display(dir.strip_prefix(root).unwrap_or(Path::new(""))));
        }
        scopes.extend(baseline_dirs.iter().cloned());
        if compatible {
            for event in &owned {
                if event.identity == identity { scopes.extend(event.observed_directories.iter().cloned()); }
            }
        }
        for touch in settled_touches {
            let path = if touch.is_absolute() { touch.clone() } else { cwd.join(touch) };
            if let Some(parent) = path.parent() {
                for dir in directories(cwd, parent) {
                    scopes.insert(display(dir.strip_prefix(root).unwrap_or(Path::new(""))));
                }
            }
        }
    }
    if scopes.len() > 8192 { return Err("workspace instruction directory budget exceeded".into()); }
    // Omitted candidates retain their discovery scope and remain retryable.
    let pending = owned.last().is_some_and(|e| e.pending);
    if !baseline && !lost && !refresh && settled_touches.is_empty() && !pending { return Ok(plan); }
    let mut ordered: Vec<_> = scopes.iter().cloned().collect();
    ordered.sort_by_key(|s| (s.split('/').count(), s.clone()));
    let mut candidates = vec![("user-global".to_owned(), "AGENTS.md".to_owned(), config.aworkit_home.join("AGENTS.md"), display(&config.aworkit_home.join("AGENTS.md")), false)];
    if let Some(root) = &root {
        for dir in &ordered {
            for name in config.candidates() {
                let relative = if dir == "." { PathBuf::from(name) } else { Path::new(dir).join(name) };
                candidates.push((dir.clone(), name.to_owned(), root.join(&relative), display(&relative), !baseline_dirs.contains(dir)));
            }
        }
    }
    let mut seen_paths = BTreeSet::new();
    let mut seen_text = BTreeSet::new();
    let mut items = Vec::new();
    let mut available = false;
    for (dir, name, path, label, nested) in candidates {
        check_cancelled(cancellation)?;
        if !seen_paths.insert(path.clone()) { continue; }
        let scope = format!("{dir}\0{name}");
        let prior = current.get(&scope).filter(|(_, c)| c.action != Action::Remove);
        let durable = compatible.then(|| known.get(&scope)).flatten().filter(|(_, c)| c.action != Action::Remove);
        let observation = files.read(&path, config.max_source_bytes, cancellation)?;
        let content = match observation {
            FileObservation::Present(text) => {
                available = true;
                if !seen_text.insert((dir, digest(text.trim()))) { None } else { Some((text.clone(), digest(&text))) }
            }
            FileObservation::Absent => { available = true; None }
            FileObservation::Unavailable(error) => {
                plan.diagnostics.push(format!("Instructions temporarily unavailable: {label}: {error}"));
                if let Some((event, change)) = durable.or(prior) {
                    if !baseline && prior.is_some() { continue; }
                    if let Some((begin, end)) = change.body {
                        if let Some(text) = event.text.get(begin..end) {
                            plan.diagnostics.push(format!("Restored last admitted instructions for {label}; source is stale/unavailable"));
                            Some((text.to_owned(), change.digest.clone().unwrap_or_default()))
                        } else { continue; }
                    } else { continue; }
                } else { continue; }
            }
        };
        let (action, content, fingerprint) = match content {
            Some((text, fingerprint)) => {
                if !baseline && prior.is_some_and(|(_, c)| c.digest.as_deref() == Some(&fingerprint)) { continue; }
                (if !baseline && prior.is_some() { Action::Replace } else { Action::Set }, text, Some(fingerprint))
            }
            None if prior.is_some() || (baseline && durable.is_some()) => (Action::Remove, String::new(), None),
            None => continue,
        };
        items.push(RenderItem { change: Change { action, scope, path: label, digest: fingerprint, body: None }, content, nested });
    }
    check_cancelled(cancellation)?;
    if !available && items.is_empty() { return Ok(plan); }
    let rendered = if baseline || !items.is_empty() { render(&items, config.max_bytes, baseline, previous_baseline.is_some() && (!compatible || visible_baseline.is_some())) } else { Default::default() };
    plan.diagnostics.extend(rendered.diagnostics);
    // Recording discovery-only observations keeps budget-omitted scopes alive.
    plan.event = Some(Event { id: event_id.into(), producer: "aworkit.workspace_instructions.v1".into(),
        owner: owner.clone(), identity, baseline, reset: !compatible,
        pending: items.len() > rendered.changes.len(), text: rendered.text, changes: rendered.changes,
        observed_directories: scopes.into_iter().collect() });
    Ok(plan)
}
