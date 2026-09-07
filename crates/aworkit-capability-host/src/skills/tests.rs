use super::*;
use crate::CancellationToken;
use std::{fs, path::Path};

fn config(root: &Path) -> SkillConfiguration {
    SkillConfiguration {
        include_default_roots: true,
        aworkit_home: root.join("global"),
        agents_home: root.join("shared"),
        custom_skill_dirs: vec![root.join("custom")],
        bundled_skill_dir: Some(root.join("bundled")),
        catalog_description_max_length: 500,
    }
}

fn write(path: &Path, name: &str, extra: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("---\nname: {name}\ndescription: >-\n  Task-specific\n  instructions <&>\n{extra}---\n\n{body}\n")).unwrap();
}

#[test]
fn ranked_roots_nearest_git_flat_files_and_no_deepseek_discovery() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let project = root.path().join("project");
    let cwd = project.join("nested");
    fs::create_dir_all(&cwd).unwrap();
    fs::write(project.join(".git"), "gitdir: test").unwrap();
    for (path, body) in [
        (project.join(".aworkit/skills/test/SKILL.md"), "project"),
        (project.join(".agents/skills/test.md"), "shared project"),
        (config.custom_skill_dirs[0].join("test.md"), "custom"),
        (config.aworkit_home.join("skills/test.md"), "user"),
        (config.agents_home.join("skills/test.md"), "shared"),
        (
            config.bundled_skill_dir.as_ref().unwrap().join("test.md"),
            "bundle",
        ),
    ] {
        write(&path, "test", "", body);
    }
    write(
        &project.join(".dsh/skills/ignored.md"),
        "ignored",
        "",
        "wrong product",
    );
    write(
        &config.aworkit_home.join("skills/.system/SKILL.md"),
        "hidden",
        "",
        "system",
    );
    write(
        &project.join(".agents/skills/deep/nested/SKILL.md"),
        "nested",
        "",
        "nested",
    );
    let cancel = CancellationToken::default();
    let snapshot = discover(&config, Some(&cwd), &cancel).unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.skills.len(), 1);
    assert_eq!(
        load(&config, Some(&cwd), "test", false, &cancel)
            .unwrap()
            .unwrap()
            .content,
        "project"
    );
    fs::remove_file(project.join(".aworkit/skills/test/SKILL.md")).unwrap();
    let skill = load(&config, Some(&cwd), "test", false, &cancel)
        .unwrap()
        .unwrap();
    assert_eq!(skill.content, "shared project");
    assert_eq!(
        serde_json::to_value(skill).unwrap()["resourceBase"]["path"],
        project.join(".agents/skills").to_string_lossy().as_ref()
    );
}

#[test]
fn invocation_policy_fails_closed_and_human_gestures_bypass_only_model_flag() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config(root.path());
    config.include_default_roots = false;
    for (name, extra) in [
        ("human", "disable-model-invocation: YES\n"),
        ("model", "user-invocable: 0\n"),
        ("invalid", "user-invocable: perhaps\n"),
        ("legacy", "modelInvocable: false\n"),
    ] {
        write(
            &config.custom_skill_dirs[0].join(format!("{name}.md")),
            name,
            extra,
            name,
        );
    }
    let cancel = CancellationToken::default();
    let snapshot = discover(&config, None, &cancel).unwrap();
    assert_eq!(snapshot.skills.len(), 2);
    assert!(snapshot.complete);
    assert_eq!(
        catalog_entries(&snapshot.skills, 500)
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["model"]
    );
    assert!(
        load(&config, None, "human", false, &cancel)
            .unwrap_err()
            .contains("not available for model invocation")
    );
    assert!(
        load(&config, None, "human", true, &cancel)
            .unwrap()
            .is_some()
    );
    assert!(
        load(&config, None, "model", true, &cancel)
            .unwrap()
            .is_none()
    );
    assert!(
        load(&config, None, "model", false, &cancel)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        invoked_names("Use /human /model /human /usr/bin 5/8 /model, x/human"),
        ["human", "model"]
    );
}

#[test]
fn bodies_reload_without_catalog_change_and_renames_retire_old_names() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let path = config.custom_skill_dirs[0].join("test.md");
    let cancel = CancellationToken::default();
    write(&path, "test", "", "first");
    let before = catalog_entries(&discover(&config, None, &cancel).unwrap().skills, 500);
    write(&path, "test", "", "second");
    assert_eq!(
        before,
        catalog_entries(&discover(&config, None, &cancel).unwrap().skills, 500)
    );
    assert_eq!(
        load(&config, None, "test", false, &cancel)
            .unwrap()
            .unwrap()
            .content,
        "second"
    );
    write(&path, "renamed", "", "third");
    assert!(
        load(&config, None, "test", false, &cancel)
            .unwrap()
            .is_none()
    );
    assert!(
        load(&config, None, "../test", false, &cancel)
            .unwrap_err()
            .contains("invalid skill name")
    );
}

#[test]
fn prompt_templates_keep_exact_markup_and_only_catalog_summaries() {
    let entries = vec![CatalogEntry {
        name: "example".into(),
        description: "Use <&>".into(),
    }];
    let initial = render_catalog(&entries, false);
    assert!(initial.contains("- `example`: Use &lt;&amp;&gt;"));
    assert!(initial.contains("Load all applicable skills, then follow their full instructions."));
    assert!(!initial.contains("Base directory"));
    let empty = render_catalog(&[], true);
    assert!(empty.contains("<available_skills>\n</available_skills>"));
    assert!(empty.contains("Do not use names from earlier skill catalogs."));
    let skill = SkillContent {
        name: "example".into(),
        provider: "filesystem".into(),
        resource_base: ResourceBase::Directory {
            path: "/a&b".into(),
        },
        content: "# Exact body\n<raw>".into(),
    };
    assert_eq!(
        render_content(&skill),
        "<skill_content name=\"example\">\n<skill_resources>\nBase directory for this skill: /a&amp;b\nResolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n</skill_resources>\n\n<skill_instructions>\n# Exact body\n<raw>\n</skill_instructions>\n</skill_content>"
    );
}

#[test]
fn missing_deleted_recreated_roots_and_cancellation_are_observed() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let cancel = CancellationToken::default();
    assert!(discover(&config, None, &cancel).unwrap().skills.is_empty());
    let path = config.custom_skill_dirs[0].join("created.md");
    write(&path, "created", "", "body");
    assert_eq!(discover(&config, None, &cancel).unwrap().skills.len(), 1);
    fs::remove_file(&path).unwrap();
    assert!(discover(&config, None, &cancel).unwrap().skills.is_empty());
    write(&path, "created", "", "body");
    assert_eq!(discover(&config, None, &cancel).unwrap().skills.len(), 1);
    cancel.cancel();
    assert!(discover(&config, None, &cancel).is_err());
}

#[test]
fn yaml_block_scalars_and_boolean_aliases_match_the_harness() {
    for (value, enabled) in [
        ("true", true),
        ("false", false),
        ("YES", true),
        ("off", false),
        ("1", true),
        ("0", false),
        ("'On'", true),
    ] {
        let parsed = parser::parse(&format!("---\r\nname: test\r\ndescription: |\r\n  first\r\n  second\r\nuser-invocable: {value}\r\n---\r\n body \r\n")).unwrap();
        assert_eq!(parsed.user_invocable, enabled);
        assert_eq!(parsed.body, "body");
        assert!(parsed.description.contains("first\nsecond"));
    }
    for raw in [
        "no yaml",
        "---\nname: test\n---",
        "---\nname: Bad\ndescription: test\n---",
        "---\nname: test\nname: test\ndescription: test\n---",
    ] {
        assert!(parser::parse(raw).is_err());
    }
}

#[test]
fn non_file_and_non_text_skill_entries_are_skipped_without_retiring_the_catalog() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let directory = &config.custom_skill_dirs[0];
    fs::create_dir_all(directory.join("not-a-file/SKILL.md")).unwrap();
    fs::write(directory.join("not-text.md"), [0xff, 0xfe]).unwrap();
    write(&directory.join("valid.md"), "valid", "", "valid body");
    let snapshot = discover(&config, None, &CancellationToken::default()).unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.skills.len(), 1);
    assert_eq!(snapshot.skills[0].name, "valid");
}
