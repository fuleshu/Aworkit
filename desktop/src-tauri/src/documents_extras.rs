//! Places the bundled example workflows and the reference FFmpeg plugin into
//! the user's documents folder.
//!
//! The installer ships the extras as inert application resources. This module
//! copies them out into `<documents>/Aworkit`, so the human-facing files live
//! somewhere the user can find, open, edit and keep. It is on by default and
//! remembers that it ran, so deleting the folder is not fought with on every
//! launch; Settings can turn it off, write the folder again, or reveal it.
//!
//! Files are only ever created when missing. An edited example workflow or an
//! edited plugin is therefore never overwritten by a later launch.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

/// Folder created inside the user's documents directory.
pub const DOCUMENTS_FOLDER_NAME: &str = "Aworkit";
/// The app data root resolved at startup, so a debug QA profile override is
/// respected instead of writing the preference into the real profile.
pub struct DocumentsExtrasRoot(pub PathBuf);
/// The extras layout this build writes. A newer build with more files bumps
/// this, and the next launch adds what is missing without touching the rest.
const EXTRAS_VERSION: u32 = 1;
/// Preference file inside the app data root.
const PREFERENCE_FILE: &str = "documents-extras.json";
/// Resource folders copied into the documents folder.
const EXAMPLES_RESOURCE: &str = "workflows/examples";
const PLUGIN_RESOURCE: &str = "tool-plugins/ffmpeg";
/// Destination folder names inside `<documents>/Aworkit`.
const EXAMPLES_FOLDER: &str = "Example Workflows";
const PLUGIN_FOLDER: &str = "Tool Plugins";
/// Folder name of the plugin inside the tool-plugins destination.
const PLUGIN_NAME: &str = "ffmpeg";
/// An interpreter cache that must never be copied into a user's folder.
const SKIPPED_DIRECTORY: &str = "__pycache__";
/// The suffix that marks an importable workflow document.
const WORKFLOW_SUFFIX: &str = ".aworkit.json";

/// Whether the extras are written into the documents folder, and whether this
/// build already wrote them once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsExtrasPreference {
    #[serde(default = "enabled_by_default")]
    pub write_to_documents: bool,
    #[serde(default)]
    pub written_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_folder: Option<String>,
}

const fn enabled_by_default() -> bool {
    true
}

impl Default for DocumentsExtrasPreference {
    fn default() -> Self {
        Self {
            write_to_documents: true,
            written_version: 0,
            last_folder: None,
        }
    }
}

/// The state the Settings control and the help page show.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsExtrasReport {
    /// Whether the folder is written on a later launch.
    pub enabled: bool,
    /// `<documents>/Aworkit`, or null when this system has no documents folder.
    pub folder: Option<String>,
    /// Whether the folder exists right now.
    pub exists: bool,
    /// How many importable example workflow documents the folder holds.
    pub example_workflow_count: usize,
    /// Whether the FFmpeg plugin package is present in the folder.
    pub plugin_present: bool,
    /// The extras layout this profile already wrote (0 = never).
    pub written_version: u32,
}

/// The resolved destinations and the source resources for one operation.
struct Environment {
    documents_folder: PathBuf,
    app_data_root: PathBuf,
    resources: PathBuf,
}

impl Environment {
    fn examples_folder(&self) -> PathBuf {
        self.documents_folder.join(EXAMPLES_FOLDER)
    }

    fn plugin_folder(&self) -> PathBuf {
        self.documents_folder.join(PLUGIN_FOLDER).join(PLUGIN_NAME)
    }
}

/// The preference file beside the app's other state.
fn preference_path(app_data_root: &Path) -> PathBuf {
    app_data_root.join(PREFERENCE_FILE)
}

/// Reads the preference. A missing or unreadable file means the default: on.
fn read_preference(path: &Path) -> DocumentsExtrasPreference {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Persists the preference, creating the app data root when it is new.
fn write_preference(path: &Path, preference: &DocumentsExtrasPreference) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create '{}': {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(preference)
        .map_err(|error| format!("cannot serialize the documents-extras preference: {error}"))?;
    fs::write(path, text)
        .map_err(|error| format!("cannot write '{}': {error}", path.display()))
}

/// Copies every missing file from `source` into `destination`.
///
/// Existing files are left exactly as the user has them. Symbolic links are
/// skipped, because the extras are plain files and a link inside a copied
/// package must not become a copy of something outside it.
fn copy_missing(source: &Path, destination: &Path) -> Result<usize, String> {
    if !source.is_dir() {
        return Err(format!(
            "bundled folder '{}' is missing from the installation",
            source.display()
        ));
    }
    let mut copied = 0;
    copy_directory(source, destination, &mut copied)?;
    Ok(copied)
}

fn copy_directory(source: &Path, destination: &Path, copied: &mut usize) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("cannot create '{}': {error}", destination.display()))?;
    let entries = fs::read_dir(source)
        .map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read '{}': {error}", source.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect '{}': {error}", entry.path().display()))?;
        if file_type.is_symlink() || entry.file_name() == SKIPPED_DIRECTORY {
            continue;
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target, copied)?;
        } else if file_type.is_file() && !target.exists() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("cannot copy '{}': {error}", target.display()))?;
            *copied += 1;
        }
    }
    Ok(())
}

/// Counts the importable example workflow documents in a folder.
fn count_workflow_files(folder: &Path) -> usize {
    fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(WORKFLOW_SUFFIX))
                .count()
        })
        .unwrap_or(0)
}

/// Writes both extras into `<documents>/Aworkit`.
fn materialize(resources: &Path, documents_folder: &Path) -> Result<(), String> {
    copy_missing(
        &resources.join(EXAMPLES_RESOURCE),
        &documents_folder.join(EXAMPLES_FOLDER),
    )?;
    copy_missing(
        &resources.join(PLUGIN_RESOURCE),
        &documents_folder.join(PLUGIN_FOLDER).join(PLUGIN_NAME),
    )?;
    Ok(())
}

/// Resolves the documents folder, the app data root and the resource root.
fn environment<R: Runtime>(
    app: &AppHandle<R>,
    app_data_root: &Path,
) -> Result<Environment, String> {
    let documents = app
        .path()
        .document_dir()
        .map_err(|error| format!("this system has no documents folder: {error}"))?;
    let resources = app
        .path()
        .resource_dir()
        .map_err(|error| format!("resource directory: {error}"))?;
    Ok(Environment {
        documents_folder: documents.join(DOCUMENTS_FOLDER_NAME),
        app_data_root: app_data_root.to_path_buf(),
        resources,
    })
}

/// Builds the report from the current folder and preference.
fn report(environment: &Environment, preference: &DocumentsExtrasPreference) -> DocumentsExtrasReport {
    DocumentsExtrasReport {
        enabled: preference.write_to_documents,
        folder: Some(environment.documents_folder.display().to_string()),
        exists: environment.documents_folder.is_dir(),
        example_workflow_count: count_workflow_files(&environment.examples_folder()),
        plugin_present: environment.plugin_folder().join("tool-plugin.json").is_file(),
        written_version: preference.written_version,
    }
}

/// Current state for the Settings control.
pub fn status<R: Runtime>(
    app: &AppHandle<R>,
    app_data_root: &Path,
) -> Result<DocumentsExtrasReport, String> {
    let environment = environment(app, app_data_root)?;
    let preference = read_preference(&preference_path(&environment.app_data_root));
    Ok(report(&environment, &preference))
}

/// Turns writing on or off. Turning it on writes the folder immediately.
pub fn set_enabled<R: Runtime>(
    app: &AppHandle<R>,
    app_data_root: &Path,
    enabled: bool,
) -> Result<DocumentsExtrasReport, String> {
    let environment = environment(app, app_data_root)?;
    let path = preference_path(&environment.app_data_root);
    let mut preference = read_preference(&path);
    preference.write_to_documents = enabled;
    if enabled {
        materialize(&environment.resources, &environment.documents_folder)?;
        preference.written_version = EXTRAS_VERSION;
        preference.last_folder = Some(environment.documents_folder.display().to_string());
    }
    write_preference(&path, &preference)?;
    Ok(report(&environment, &preference))
}

/// Writes any missing files now, and turns writing back on.
pub fn write_now<R: Runtime>(
    app: &AppHandle<R>,
    app_data_root: &Path,
) -> Result<DocumentsExtrasReport, String> {
    let environment = environment(app, app_data_root)?;
    let path = preference_path(&environment.app_data_root);
    let mut preference = read_preference(&path);
    materialize(&environment.resources, &environment.documents_folder)?;
    preference.write_to_documents = true;
    preference.written_version = EXTRAS_VERSION;
    preference.last_folder = Some(environment.documents_folder.display().to_string());
    write_preference(&path, &preference)?;
    Ok(report(&environment, &preference))
}

/// Reveals `<documents>/Aworkit` in the platform file manager.
pub fn open_folder<R: Runtime>(
    app: &AppHandle<R>,
    app_data_root: &Path,
) -> Result<(), String> {
    let folder = environment(app, app_data_root)?.documents_folder;
    fs::create_dir_all(&folder)
        .map_err(|error| format!("cannot create '{}': {error}", folder.display()))?;
    crate::runtime::reveal(&folder)
}

/// First-run work: write the extras once, unless the user turned them off.
///
/// Runs off the event loop, and a failure is reported rather than fatal: the
/// app must start even when the documents folder is redirected, read-only or
/// absent.
pub fn start<R: Runtime>(app: AppHandle<R>, app_data_root: PathBuf) {
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = ensure_on_start(&app, &app_data_root) {
            eprintln!("aworkit: example workflows and plugin folder: {error}");
        }
    });
}

fn ensure_on_start<R: Runtime>(app: &AppHandle<R>, app_data_root: &Path) -> Result<(), String> {
    let environment = environment(app, app_data_root)?;
    let path = preference_path(&environment.app_data_root);
    let mut preference = read_preference(&path);
    if !preference.write_to_documents || preference.written_version >= EXTRAS_VERSION {
        return Ok(());
    }
    materialize(&environment.resources, &environment.documents_folder)?;
    preference.written_version = EXTRAS_VERSION;
    preference.last_folder = Some(environment.documents_folder.display().to_string());
    write_preference(&path, &preference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Builds a resource tree shaped like the installed application.
    fn resources(root: &Path) {
        let examples = root.join(EXAMPLES_RESOURCE);
        fs::create_dir_all(&examples).unwrap();
        fs::write(examples.join("triage-router.aworkit.json"), b"{}").unwrap();
        fs::write(examples.join("evidence-brief.aworkit.json"), b"{}").unwrap();
        fs::write(examples.join("README.md"), b"# Examples").unwrap();
        let plugin = root.join(PLUGIN_RESOURCE);
        fs::create_dir_all(plugin.join("skills/ffmpeg")).unwrap();
        fs::write(plugin.join("tool-plugin.json"), b"{}").unwrap();
        fs::write(plugin.join("ffmpeg_bridge.py"), b"print()").unwrap();
        fs::write(plugin.join("skills/ffmpeg/SKILL.md"), b"# FFmpeg").unwrap();
        fs::create_dir_all(plugin.join(SKIPPED_DIRECTORY)).unwrap();
        fs::write(plugin.join(SKIPPED_DIRECTORY).join("cached.pyc"), b"x").unwrap();
    }

    #[test]
    fn the_default_preference_writes_the_folder() {
        assert!(DocumentsExtrasPreference::default().write_to_documents);
    }

    #[test]
    fn a_missing_or_malformed_preference_means_enabled() {
        let root = TempDir::new().unwrap();
        let missing = preference_path(root.path());
        assert!(read_preference(&missing).write_to_documents);
        fs::write(&missing, b"not json").unwrap();
        assert!(read_preference(&missing).write_to_documents);
    }

    #[test]
    fn materialize_writes_both_extras_and_skips_the_cache() {
        let resources_root = TempDir::new().unwrap();
        let documents = TempDir::new().unwrap();
        resources(resources_root.path());
        let folder = documents.path().join(DOCUMENTS_FOLDER_NAME);

        materialize(resources_root.path(), &folder).unwrap();

        assert_eq!(count_workflow_files(&folder.join(EXAMPLES_FOLDER)), 2);
        assert!(folder.join(EXAMPLES_FOLDER).join("README.md").is_file());
        assert!(
            folder
                .join(PLUGIN_FOLDER)
                .join(PLUGIN_NAME)
                .join("tool-plugin.json")
                .is_file()
        );
        assert!(
            folder
                .join(PLUGIN_FOLDER)
                .join(PLUGIN_NAME)
                .join("skills/ffmpeg/SKILL.md")
                .is_file()
        );
        assert!(
            !folder
                .join(PLUGIN_FOLDER)
                .join(PLUGIN_NAME)
                .join(SKIPPED_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn materialize_never_overwrites_an_edited_file() {
        let resources_root = TempDir::new().unwrap();
        let documents = TempDir::new().unwrap();
        resources(resources_root.path());
        let folder = documents.path().join(DOCUMENTS_FOLDER_NAME);
        materialize(resources_root.path(), &folder).unwrap();

        let edited = folder.join(EXAMPLES_FOLDER).join("README.md");
        fs::write(&edited, b"my own notes").unwrap();
        fs::write(
            resources_root.path().join(EXAMPLES_RESOURCE).join("README.md"),
            b"# replaced",
        )
        .unwrap();

        materialize(resources_root.path(), &folder).unwrap();
        assert_eq!(fs::read(&edited).unwrap(), b"my own notes");
    }

    #[test]
    fn ensure_on_start_writes_once_and_respects_the_opt_out() {
        let root = TempDir::new().unwrap();
        let preference = preference_path(root.path());
        // A disabled preference never writes, and never records a version.
        write_preference(
            &preference,
            &DocumentsExtrasPreference {
                write_to_documents: false,
                ..DocumentsExtrasPreference::default()
            },
        )
        .unwrap();
        let stored = read_preference(&preference);
        assert!(!stored.write_to_documents);
        assert_eq!(stored.written_version, 0);
    }
}
