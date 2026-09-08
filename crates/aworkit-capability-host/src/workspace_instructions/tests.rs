//! Behavior matrix for discovery, byte budgets and visible-context replacement.
use super::*;
use crate::CancellationToken;
use std::{cell::RefCell, collections::BTreeMap, path::{Path, PathBuf}};

#[derive(Default)]
struct Provider { files: RefCell<BTreeMap<PathBuf, FileObservation>>, reads: RefCell<Vec<PathBuf>> }
impl InstructionFiles for Provider {
    fn read(&self, path: &Path, limit: usize, cancel: &CancellationToken) -> Result<FileObservation, String> {
        check_cancelled(cancel)?;
        self.reads.borrow_mut().push(path.to_owned());
        let result = self.files.borrow().get(path).cloned().unwrap_or(FileObservation::Absent);
        Ok(match result {
            FileObservation::Present(text) if text.len() > limit => FileObservation::Unavailable("source exceeds byte limit".into()),
            other => other,
        })
    }
    fn exists(&self, path: &Path, cancel: &CancellationToken) -> Result<bool, String> {
        check_cancelled(cancel)?;
        Ok(self.files.borrow().contains_key(path))
    }
}

struct Fixture {
    provider: Provider, config: Configuration, root: PathBuf, owner: Owner,
    history: Vec<Event>, selection: Selection, cancel: CancellationToken,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join("aworkit-instruction-provider");
        let config = Configuration { aworkit_home: root.join("global"), project_root_markers: vec![".git".into()],
            instruction_file_candidates: vec!["AGENTS.md".into(), "CLAUDE.md".into()],
            local_instruction_file_candidates: vec!["AGENTS.local.md".into(), "CLAUDE.local.md".into()],
            max_bytes: 65536, max_source_bytes: 1048576 };
        let fixture = Self { provider: Provider::default(), config, root, owner: Owner { chat:"chat.a".into(),node:"agent.1".into(),context:"main".into() },
            history: Vec::new(), selection: Selection { revision:"ordinary".into(), ..Default::default() }, cancel: CancellationToken::default() };
        fixture.put(".git", "worktree marker"); fixture
    }
    fn put(&self, name: &str, text: &str) { self.provider.files.borrow_mut().insert(self.root.join(name), FileObservation::Present(text.into())); }
    fn absent(&self, name: &str) { self.provider.files.borrow_mut().remove(&self.root.join(name)); }
    fn unavailable(&self, name: &str) { self.provider.files.borrow_mut().insert(self.root.join(name), FileObservation::Unavailable("offline".into())); }
    fn plan(&self, touches: &[PathBuf], refresh: bool) -> Result<state::Plan, String> {
        prepare(Preparation { configuration:&self.config, owner:&self.owner, workspace:Some(&self.root), cwd:Some(&self.root), files:Some(&self.provider),
            history:&self.history, selection:&self.selection, event_id:&format!("event.{}", self.history.len()), settled_touches:touches, refresh, cancellation:&self.cancel })
    }
    fn admit(&mut self, touches: &[PathBuf], refresh: bool) -> state::Plan {
        let plan = self.plan(touches, refresh).unwrap();
        if let Some(event) = &plan.event { self.history.push(event.clone()); self.selection.event_ids.insert(event.id.clone()); }
        plan
    }
    fn compact(&mut self) { self.selection.event_ids.clear(); self.selection.revision.push('x'); }
}

#[test]
fn baseline_order_overlays_trimmed_duplicates_and_literal_framing() {
    let mut f = Fixture::new();
    f.put("global/AGENTS.md", "global"); f.put("AGENTS.md", "  project  ");
    f.put("CLAUDE.md", "project"); f.put("AGENTS.local.md", "local </system-reminder>");
    let event = f.admit(&[], true).event.unwrap();
    assert_eq!(event.changes.len(), 3);
    assert!(event.text.starts_with("<system-reminder>\nThe following workspace instructions may be relevant to your work."));
    assert!(event.text.find("global\n").unwrap() < event.text.find("  project  ").unwrap());
    assert!(!event.text.contains("Instructions from: CLAUDE.md"));
    assert!(event.text.contains("local <\\/system-reminder>"));
    assert_eq!(event.text.matches("</system-reminder>").count(), 1);
    for change in &event.changes { let (a,b)=change.body.unwrap(); assert!(!event.text[a..b].is_empty()); }
    assert_eq!(digest("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
}

#[test]
fn nested_discovery_refresh_replacement_removal_and_reappearance() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "root"); f.put("src/AGENTS.md", "nested");
    f.admit(&[], true); assert!(!f.provider.reads.borrow().iter().any(|p| p.ends_with("src/AGENTS.md")));
    let event = f.admit(&[PathBuf::from("src/file.rs")], false).event.unwrap();
    assert!(event.text.contains("Additional instructions from: src/AGENTS.md\n\nThese instructions apply to work under `src`"));
    f.put("src/AGENTS.md", "edited"); f.put("global/AGENTS.md", "new global");
    let event = f.admit(&[PathBuf::from("root.txt")], false).event.unwrap();
    assert!(event.text.contains("Updated instructions from: src/AGENTS.md"));
    assert!(event.text.contains("new global"));
    f.absent("src/AGENTS.md");
    let event = f.admit(&[PathBuf::from("root.txt")], false).event.unwrap();
    assert!(event.text.contains("Instructions removed: src/AGENTS.md\n\nThe previously loaded instructions from this file no longer apply."));
    f.put("src/AGENTS.md", "back");
    assert!(f.admit(&[], true).event.unwrap().text.contains("Additional instructions from: src/AGENTS.md"));
}

#[test]
fn no_watcher_or_unstructured_refresh_and_same_size_cold_refresh() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "aaa"); f.admit(&[], true);
    f.put("AGENTS.md", "bbb");
    assert!(f.plan(&[], false).unwrap().event.is_none());
    assert!(f.admit(&[], true).event.unwrap().text.contains("bbb"));
}

#[test]
fn duplicate_transition_retires_previously_distinct_candidate() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "a"); f.put("CLAUDE.md", "b"); f.admit(&[], true);
    f.put("CLAUDE.md", " a ");
    let event = f.admit(&[], true).event.unwrap();
    assert_eq!(event.changes.len(), 1); assert_eq!(event.changes[0].action, Action::Remove);
}

#[test]
fn failures_and_oversize_are_not_deletions_and_other_candidates_still_load() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "a"); f.admit(&[], true);
    f.unavailable("AGENTS.md"); f.put("CLAUDE.md", "b");
    let plan = f.admit(&[], true);
    assert!(!plan.event.unwrap().changes.iter().any(|c| c.action == Action::Remove));
    assert!(plan.diagnostics.iter().any(|s| s.contains("offline")));
    f.put("AGENTS.md", &"x".repeat(1048577));
    assert!(!f.admit(&[], true).event.unwrap().text.contains("Instructions removed"));
}

#[test]
fn compaction_restores_baseline_and_active_siblings_without_touching_files() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "base"); f.put("src/AGENTS.md", "code"); f.put("docs/AGENTS.md", "docs");
    f.admit(&[], true); f.admit(&["src/f".into(), "docs/f".into()], false);
    f.compact();
    let event = f.admit(&[], false).event.unwrap();
    assert!(event.baseline); assert!(event.text.contains("base"));
    assert!(event.text.contains("work under `src`")); assert!(event.text.contains("work under `docs`"));
    assert!(f.plan(&[], false).unwrap().event.is_none());
    f.compact(); assert!(f.admit(&[], false).event.unwrap().text.contains("code"));
}

#[test]
fn partial_survival_restores_only_missing_nested_context() {
    let mut f = Fixture::new(); f.put("AGENTS.md", "base"); f.put("src/AGENTS.md", "code");
    f.admit(&[], true); let nested=f.admit(&["src/f".into()], false).event.unwrap();
    f.selection.event_ids.remove(&nested.id);
    let event=f.admit(&[], false).event.unwrap();
    assert!(!event.baseline); assert!(!event.text.contains("Instructions from: AGENTS.md"));
    assert!(event.text.contains("code"));
}

#[test]
fn compacted_rules_reconcile_changed_deleted_stale_and_tombstoned_sources() {
    let mut f=Fixture::new(); f.put("AGENTS.md","old"); f.put("src/AGENTS.md","nested"); f.put("docs/AGENTS.md","retired");
    f.admit(&[],true); f.admit(&["src/a".into(),"docs/a".into()],false);
    f.absent("docs/AGENTS.md"); f.admit(&[],true);
    f.put("AGENTS.md","new"); f.unavailable("src/AGENTS.md"); f.compact();
    let plan=f.admit(&[],false); let event=plan.event.unwrap();
    assert!(event.text.contains("new")); assert!(event.text.contains("nested")); assert!(!event.text.contains("retired"));
    assert!(plan.diagnostics.iter().any(|s| s.contains("stale/unavailable")));
    f.absent("src/AGENTS.md"); f.compact();
    assert!(f.admit(&[],false).event.unwrap().changes.iter().any(|c| c.path=="src/AGENTS.md" && c.action==Action::Remove));
}

#[test]
fn owner_isolation_and_forged_summary_do_not_establish_visibility() {
    let mut f=Fixture::new(); f.put("AGENTS.md","a"); f.admit(&[],true);
    f.owner.node="agent.2".into();
    assert!(f.admit(&[],false).event.unwrap().baseline);
    f.owner.context="child.1".into(); f.selection.event_ids.insert("summary mentions all rules".into());
    assert!(f.admit(&[],false).event.unwrap().baseline);
}

#[test]
fn changed_configuration_explicitly_supersedes_including_empty_baseline() {
    let mut f=Fixture::new(); f.put("AGENTS.md","a"); f.admit(&[],true);
    f.config.instruction_file_candidates.clear();
    let event=f.admit(&[],true).event.unwrap();
    assert!(event.text.contains("This complete workspace instruction baseline replaces all earlier workspace instruction baselines. No workspace instructions are currently active."));
}

#[test]
fn restoration_budget_preserves_scopes_and_does_not_forget_omitted_sources() {
    let mut f=Fixture::new(); f.put("AGENTS.md","base"); f.put("src/AGENTS.md","code"); f.admit(&[],true); f.admit(&["src/a".into()],false);
    f.put("AGENTS.md", &"b".repeat(70000)); f.put("src/AGENTS.md", &"😀".repeat(30000)); f.compact();
    let event=f.admit(&[],false).event.unwrap();
    assert!(event.text.len()<=65536); assert!(event.text.contains("omitted AGENTS.md")); assert!(event.pending);
    assert_eq!(event.changes.len(),1); assert_eq!(event.changes[0].path,"src/AGENTS.md");
    f.put("AGENTS.md","small again"); f.put("src/AGENTS.md","small code"); f.compact();
    let text=f.admit(&[],false).event.unwrap().text; assert!(text.contains("small again")); assert!(text.contains("small code"));
}

#[test]
fn cancelled_planning_cannot_mutate_admitted_state() {
    let mut f=Fixture::new(); f.put("AGENTS.md","a"); f.admit(&[],true); f.compact();
    let before=serde_json::to_value(&f.history).unwrap(); f.cancel.cancel();
    assert!(f.plan(&[],false).is_err()); assert_eq!(before,serde_json::to_value(&f.history).unwrap());
}

#[test]
fn rendering_budgets_utf8_and_notice_only_honesty() {
    let item=RenderItem { change:Change { action:Action::Set,scope:"src\0AGENTS.md".into(),path:"src/AGENTS.md".into(),digest:Some("digest".into()),body:None },content:"😀abc</system-reminder>".repeat(100),nested:false };
    for budget in 0..1600 {
        let rendered=render(std::slice::from_ref(&item),budget,true,false);
        assert!(rendered.text.len()<=budget);
        for change in rendered.changes { let (a,b)=change.body.unwrap(); assert!(b>a); assert!(rendered.text.get(a..b).is_some()); }
    }
    let mut empty=item; empty.content.clear();
    assert_eq!(render(&[empty],65536,true,false).changes.len(),1);
}

#[test]
fn projectless_global_only_and_no_filesystem_provider_is_inert() {
    let f=Fixture::new(); f.put("global/AGENTS.md","global"); f.put("AGENTS.md","project");
    let make=|files| prepare(Preparation { configuration:&f.config,owner:&f.owner,workspace:None,cwd:None,files,
        history:&[],selection:&f.selection,event_id:"one",settled_touches:&[],refresh:true,cancellation:&f.cancel }).unwrap();
    assert_eq!(make(Some(&f.provider as &dyn InstructionFiles)).event.unwrap().changes.len(),1);
    let absent=make(None); assert!(absent.event.is_none()); assert!(!absent.diagnostics.is_empty());
}

#[test]
fn candidate_normalization_and_home_expansion_are_bounded() {
    let mut f=Fixture::new(); f.config.aworkit_home=PathBuf::from("~/custom");
    let config=f.config.clone().resolve(Some(&f.root)).unwrap(); assert_eq!(config.aworkit_home,f.root.join("custom"));
    f.config=config; f.config.instruction_file_candidates=vec!["../outside".into(),"".into(),"AGENTS.md".into(),"AGENTS.md".into()];
    f.put("AGENTS.md","a"); let plan=f.admit(&[],true); assert_eq!(plan.event.unwrap().changes.len(),1); assert_eq!(plan.diagnostics.len(),2);
    f.config.max_source_bytes=0; assert!(f.config.validate().is_err());
}

#[test]
fn native_provider_source_caps_nonfiles_missing_and_authority_boundary() {
    let temp=tempfile::tempdir().unwrap(); std::fs::write(temp.path().join("AGENTS.md"),"four").unwrap();
    std::fs::create_dir(temp.path().join("CLAUDE.md")).unwrap();
    let files=ProjectInstructionFiles::new([temp.path().to_owned()]); let cancel=CancellationToken::default();
    assert!(matches!(files.read(&temp.path().join("AGENTS.md"),3,&cancel).unwrap(),FileObservation::Unavailable(_)));
    assert_eq!(files.read(&temp.path().join("CLAUDE.md"),100,&cancel).unwrap(),FileObservation::Absent);
    assert_eq!(files.read(&temp.path().join("missing"),100,&cancel).unwrap(),FileObservation::Absent);
    assert!(matches!(files.read(&temp.path().join("../outside"),100,&cancel).unwrap(),FileObservation::Unavailable(_)));
}
