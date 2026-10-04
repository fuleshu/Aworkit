//! The bundled standard skills: what ships, that a fresh profile finds them,
//! that precedence still belongs to project and user skills, and that the
//! workflow node catalog and the chat history schema stay in step with the code.
use super::*;
use aworkit_capability_host::skills::{
    self as library, SkillConfiguration, SkillSnapshot, catalog_entries, render_catalog,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Words each bundled description must contain for the request it answers to
/// trigger it. A description is the only text the model sees before loading.
const TRIGGERS: &[(&str, &[&str])] = &[
    ("aworkit-skills", &["skill"]),
    ("aworkit-plugins", &["plugin"]),
    ("aworkit-workflows", &["workflow"]),
    ("aworkit-chat-storage", &["chat", "sqlite"]),
];

/// The folder this repository ships as its bundled skills.
fn shipped() -> PathBuf {
    let folder = super::bundled_skills::source_tree().expect("source tree");
    assert!(
        folder.is_dir(),
        "the shipped bundled skill folder is missing: {}",
        folder.display()
    );
    folder
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// A fresh profile: no configured folders, no project, blank bundled folder.
fn fresh_configuration(root: &Path, workspace: &Path) -> SkillConfiguration {
    let configuration = json!({
        "includeDefaultRoots": true,
        "aworkitHome": root.join("global").to_string_lossy(),
        "agentsHome": root.join("shared").to_string_lossy(),
        "customSkillDirs": [],
        "bundledSkillDir": "",
        "catalogDescriptionMaxLength": 500,
    });
    super::skills::configuration(&configuration).expect("fresh profile skills configuration")
}

fn snapshot(configuration: &SkillConfiguration, workspace: Option<&Path>) -> SkillSnapshot {
    library::discover(configuration, workspace, &CancellationToken::default()).expect("discovery")
}

/// The first markdown table whose header contains `header_marker`.
fn markdown_table(markdown: &str, header_marker: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut started = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            if started {
                break;
            }
            continue;
        }
        let cells: Vec<String> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect();
        if !started {
            if cells.iter().any(|cell| cell.contains(header_marker)) {
                started = true;
            }
            continue;
        }
        if cells
            .iter()
            .all(|cell| cell.chars().all(|c| c == '-' || c == ' '))
        {
            continue;
        }
        rows.push(cells);
    }
    assert!(!rows.is_empty(), "no table found for '{header_marker}'");
    rows
}

/// Backticked, comma-separated keys of one skill table cell.
fn table_keys(cell: &str) -> BTreeSet<String> {
    cell.split(',')
        .map(|key| key.trim().trim_matches('`').trim().to_string())
        .filter(|key| !key.is_empty() && key != "—")
        .collect()
}

#[test]
fn every_standard_skill_ships_as_a_readable_skill_folder() {
    let shipped = shipped();
    for name in super::bundled_skills::STANDARD_SKILLS {
        let file = shipped.join(name).join("SKILL.md");
        let raw = read(&file);
        let frontmatter = raw
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---"))
            .map(|(frontmatter, _)| frontmatter)
            .unwrap_or_else(|| panic!("{name} has no YAML frontmatter"));
        assert!(
            frontmatter.contains(&format!("name: {name}")),
            "{name} must declare its own name"
        );
        assert!(
            frontmatter.contains("description:"),
            "{name} must declare a description"
        );
    }
    assert!(shipped.join("aworkit-workflows/references/nodes.md").is_file());
    assert!(shipped.join("aworkit-chat-storage/references/schema.md").is_file());
}

#[test]
fn a_fresh_profile_discovers_every_standard_skill() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let configuration = fresh_configuration(root.path(), &workspace);
    assert_eq!(
        configuration.bundled_skill_dir.as_deref(),
        Some(shipped().as_path()),
        "a blank bundledSkillDir must resolve to the folder this build ships"
    );

    let snapshot = snapshot(&configuration, Some(&workspace));
    assert!(snapshot.complete, "{:?}", snapshot.warnings);
    assert!(snapshot.warnings.is_empty(), "{:?}", snapshot.warnings);
    for name in super::bundled_skills::STANDARD_SKILLS {
        let skill = snapshot
            .skills
            .iter()
            .find(|skill| skill.name.as_str() == *name)
            .unwrap_or_else(|| panic!("{name} was not discovered on a fresh profile"));
        assert!(skill.model_invocable && skill.user_invocable);
        assert!(skill.description.len() <= 500);
    }

    let entries = catalog_entries(&snapshot.skills, 500);
    for (name, words) in TRIGGERS {
        let entry = entries
            .iter()
            .find(|entry| entry.name == *name)
            .unwrap_or_else(|| panic!("{name} is missing from the catalog"));
        let description = entry.description.to_lowercase();
        for word in *words {
            assert!(
                description.contains(word),
                "{name} description does not mention '{word}': {}",
                entry.description
            );
        }
    }
    let rendered = render_catalog(&entries, false);
    assert!(rendered.contains("`aworkit-workflows`"));
    assert!(rendered.contains("`aworkit-chat-storage`"));
}

#[test]
fn a_project_skill_of_the_same_name_wins_over_the_bundled_copy() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let project_skill = workspace.join(".aworkit/skills/aworkit-workflows");
    std::fs::create_dir_all(&project_skill).unwrap();
    std::fs::write(
        project_skill.join("SKILL.md"),
        "---\nname: aworkit-workflows\ndescription: project override that must win\n---\nproject body\n",
    )
    .unwrap();
    let configuration = fresh_configuration(root.path(), &workspace);

    let snapshot = snapshot(&configuration, Some(&workspace));
    assert!(snapshot.complete, "{:?}", snapshot.warnings);
    let winner = snapshot
        .skills
        .iter()
        .find(|skill| skill.name == "aworkit-workflows")
        .expect("the name is discovered once");
    assert_eq!(winner.description, "project override that must win");
    assert_eq!(
        dunce::canonicalize(&winner.path).unwrap(),
        dunce::canonicalize(project_skill.join("SKILL.md")).unwrap()
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("duplicate skill")),
        "the bundled copy must be skipped as a duplicate: {:?}",
        snapshot.warnings
    );

    // The shadowed name does not take the rest of the bundled folder with it.
    for name in super::bundled_skills::STANDARD_SKILLS {
        assert!(
            snapshot
                .skills
                .iter()
                .any(|skill| skill.name.as_str() == *name),
            "{name} must stay available"
        );
    }

    let loaded = library::load(
        &configuration,
        Some(&workspace),
        "aworkit-workflows",
        false,
        &CancellationToken::default(),
    )
    .unwrap()
    .expect("the winning skill loads");
    assert_eq!(loaded.content, "project body");
}

#[test]
fn bundled_root_prefers_the_setting_then_the_installed_copy_then_the_source_tree() {
    let root = tempfile::tempdir().unwrap();
    let environment = root.path().join("environment");
    let installed = root.path().join("installed");
    let source = root.path().join("source");
    for folder in [&environment, &installed, &source] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let native_source = super::bundled_skills::source_tree().expect("source tree");
    assert_eq!(
        super::bundled_skills::resolve_root(Some(environment.to_str().unwrap())),
        Some(environment.clone()),
        "a configured folder replaces the bundled default"
    );
    assert_eq!(
        super::bundled_skills::select_default(
            Some(environment.clone()),
            Some(installed.clone()),
            Some(source.clone())
        ),
        Some(environment)
    );
    assert_eq!(
        super::bundled_skills::select_default(None, Some(installed.clone()), Some(source.clone())),
        Some(installed)
    );
    assert_eq!(
        super::bundled_skills::select_default(None, None, Some(source)),
        Some(root.path().join("source"))
    );
    assert_eq!(
        super::bundled_skills::select_default(None, None, None),
        None
    );
    assert_eq!(
        super::bundled_skills::select_default(None, Some(root.path().join("missing")), None),
        None,
        "a candidate that does not exist is not a bundled folder"
    );
    assert!(native_source.join("aworkit-skills/SKILL.md").is_file());
}

#[test]
fn workflow_skill_enumerates_every_node_type_and_configuration_key() {
    let reference = read(&shipped().join("aworkit-workflows/references/nodes.md"));
    let rows = markdown_table(&reference, "node type");
    let mut documented: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for row in &rows {
        assert_eq!(row.len(), 3, "node catalog row {row:?}");
        let name = row[0].trim_matches('`').to_string();
        assert!(
            documented
                .insert(name.clone(), (table_keys(&row[1]), table_keys(&row[2])))
                .is_none(),
            "{name} is documented twice"
        );
    }

    let contracts = crate::runtime::documents::NODE_CONFIGURATION_CONTRACTS_V1
        .iter()
        .map(|contract| (contract.node_type.to_string(), contract))
        .collect::<Vec<_>>();
    let contract_names = contracts
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        contract_names,
        crate::runtime::documents::KNOWN_NODE_TYPES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
        "the configuration contracts and the known node types are one catalog"
    );

    let documented_names = rows
        .iter()
        .map(|row| row[0].trim_matches('`').to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        documented_names, contract_names,
        "the workflow skill must list every node type in catalog order"
    );
    for (name, contract) in contracts {
        let (required, optional) = documented
            .get(&name)
            .unwrap_or_else(|| panic!("{name} is missing from the node catalog"));
        assert_eq!(
            *required,
            contract
                .required
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "required configuration of {name}"
        );
        assert_eq!(
            *optional,
            contract
                .optional
                .iter()
                .map(|key| (*key).to_string())
                .collect::<BTreeSet<String>>(),
            "optional configuration of {name}"
        );
    }
}

#[test]
fn chat_storage_skill_matches_the_real_history_schema() {
    let skill = read(&shipped().join("aworkit-chat-storage/SKILL.md"));
    let reference = read(&shipped().join("aworkit-chat-storage/references/schema.md"));
    for fact in [
        "history/aworkit.sqlite3",
        "portable_runtime_journal_v2",
        "aworkit-portable-session",
    ] {
        assert!(
            skill.contains(fact) || reference.contains(fact),
            "the chat storage skill must state {fact}"
        );
    }

    let mut documented: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in markdown_table(&reference, "table") {
        assert_eq!(row.len(), 2, "schema row {row:?}");
        let columns = row[1]
            .split(',')
            .map(|column| column.trim().trim_matches('`').trim().to_string())
            .filter(|column| !column.is_empty())
            .collect::<Vec<_>>();
        assert!(
            documented
                .insert(row[0].trim_matches('`').to_string(), columns)
                .is_none(),
            "{} is documented twice",
            row[0]
        );
    }

    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("history").join("aworkit.sqlite3");
    let _store = aworkit_local_store::LocalHistoryStore::open(&database).expect("history store");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let documented_names = documented.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        documented_names, tables,
        "the chat storage skill must document every table this build creates"
    );
    for (table, columns) in &documented {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let actual = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(&actual, columns, "columns of {table}");
    }
}
