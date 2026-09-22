//! Explicit user-initiated actions on a file path shown in a conversation.
//!
//! A model-authored path never drives the desktop shell by itself. The webview
//! renders the path, the user chooses an action from a context menu, and only
//! then does the core resolve the path inside the Chat's frozen workspace and
//! hand it to the operating system. The webview therefore gains no general
//! path-opening capability, and every action is mediated by the same workspace
//! containment rule the file tools already follow.

use std::{
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

/// One action a user may choose for a path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathActionV1 {
    /// Report whether the path can be acted on, without acting.
    Inspect,
    /// Open with the operating system's default application for the format.
    OpenDefault,
    /// Open in the configured editor command, or the platform default when the
    /// user has not configured one.
    OpenEditor,
    /// Reveal the file in the platform file manager.
    Reveal,
}

/// One request from the presentation layer.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PathActionRequestV1 {
    /// Chat whose frozen workspace owns the path.
    pub chat_id: String,
    /// The path exactly as the conversation showed it: absolute, or relative to
    /// the Chat's workspace.
    pub path: String,
    pub action: PathActionV1,
}

/// The outcome of one inspect or action request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PathActionOutcomeV1 {
    /// Resolved path inside the workspace, when it could be resolved at all.
    pub absolute_path: String,
    /// Whether the path is inside the workspace and exists.
    pub eligible: bool,
    /// Stable explanation when the path is not eligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether an action was actually performed.
    pub performed: bool,
}

/// Largest accepted path string.
const MAXIMUM_PATH_BYTES: usize = 16 * 1024;

/// Resolves one requested path against the Chat's workspace root.
///
/// Containment is checked twice: lexically, so a parent traversal is refused
/// even when it points at something that does not exist, and again after
/// canonicalization, so a symlink inside the workspace cannot lead outside it.
pub(crate) fn resolve_in_workspace(
    workspace_root: &Path,
    requested: &str,
) -> Result<PathBuf, String> {
    if requested.trim().is_empty()
        || requested.len() > MAXIMUM_PATH_BYTES
        || requested.contains('\0')
    {
        return Err("the path is empty or exceeds the accepted size".into());
    }
    let root = std::fs::canonicalize(workspace_root)
        .map_err(|_| "the Chat's workspace folder is unavailable".to_owned())?;
    let candidate = Path::new(requested);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let normalized = lexical_normalize(&joined);
    if !normalized.starts_with(&root) {
        return Err("the path is outside the Chat's workspace".into());
    }
    match std::fs::canonicalize(&normalized) {
        Ok(canonical) => {
            if !canonical.starts_with(&root) {
                return Err("the path is outside the Chat's workspace".into());
            }
            Ok(canonical)
        }
        // A missing target is still a resolvable location: the menu reports it
        // as ineligible instead of pretending the workspace rule failed.
        Err(_) => Ok(normalized),
    }
}

/// Removes `.` and resolves `..` lexically, without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never step above an absolute root; the containment check then
                // refuses the result instead of silently rewriting it.
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Reports whether one path can be acted on, without performing anything.
#[must_use]
pub(crate) fn inspect_in_workspace(workspace_root: &Path, requested: &str) -> PathActionOutcomeV1 {
    match resolve_in_workspace(workspace_root, requested) {
        Ok(path) => {
            let exists = path.exists();
            PathActionOutcomeV1 {
                absolute_path: path.display().to_string(),
                eligible: exists,
                reason: (!exists).then(|| "the file no longer exists".to_owned()),
                performed: false,
            }
        }
        Err(reason) => PathActionOutcomeV1 {
            absolute_path: requested.to_owned(),
            eligible: false,
            reason: Some(reason),
            performed: false,
        },
    }
}

/// Performs one action on a resolved path.
///
/// `editor` is the user's configured editor command, if any; without one,
/// open-in-editor falls back to the platform default for the file format.
pub(crate) fn perform_in_workspace(
    workspace_root: &Path,
    requested: &str,
    action: PathActionV1,
    editor: Option<&str>,
) -> Result<PathActionOutcomeV1, String> {
    let inspected = inspect_in_workspace(workspace_root, requested);
    if !inspected.eligible {
        return Ok(inspected);
    }
    if action == PathActionV1::Inspect {
        return Ok(inspected);
    }
    let path = PathBuf::from(&inspected.absolute_path);
    let (program, arguments) = match action {
        PathActionV1::Inspect => unreachable!("inspect returns above"),
        PathActionV1::OpenDefault => open_argv(&path),
        PathActionV1::Reveal => reveal_argv(&path),
        PathActionV1::OpenEditor => match editor.map(str::trim).filter(|e| !e.is_empty()) {
            Some(editor) => editor_argv(editor, &path),
            None => open_argv(&path),
        },
    };
    launch(&program, &arguments).map_err(|error| format!("could not open the path: {error}"))?;
    Ok(PathActionOutcomeV1 {
        performed: true,
        ..inspected
    })
}

/// Starts one detached user-facing process and reaps it on a helper thread so a
/// long-lived desktop process never accumulates zombies.
fn launch(program: &str, arguments: &[String]) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(arguments)
        .spawn()
        .map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Exact argv the platform uses to open a file with its default application.
fn open_argv(path: &Path) -> (String, Vec<String>) {
    let text = path.display().to_string();
    if cfg!(target_os = "macos") {
        ("open".to_owned(), vec![text])
    } else if cfg!(windows) {
        // `start` is a cmd builtin; the empty argument is the window title.
        (
            "cmd".to_owned(),
            vec!["/C".into(), "start".into(), String::new(), text],
        )
    } else {
        ("xdg-open".to_owned(), vec![text])
    }
}

/// Exact argv the platform uses to reveal a file in the file manager.
fn reveal_argv(path: &Path) -> (String, Vec<String>) {
    let text = path.display().to_string();
    if cfg!(target_os = "macos") {
        ("open".to_owned(), vec!["-R".into(), text])
    } else if cfg!(windows) {
        // One argument: explorer requires the switch and value together.
        ("explorer".to_owned(), vec![format!("/select,{text}")])
    } else {
        let parent = path
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or(text);
        ("xdg-open".to_owned(), vec![parent])
    }
}

/// Exact argv one configured editor command uses for a file.
fn editor_argv(editor: &str, path: &Path) -> (String, Vec<String>) {
    (editor.to_owned(), vec![path.display().to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn workspace() -> (TempDir, PathBuf, PathBuf) {
        let root = TempDir::new().expect("temporary directory");
        let folder = root.path().join("project");
        std::fs::create_dir(&folder).expect("project folder");
        std::fs::write(folder.join("summary.md"), b"# Summary").expect("file");
        let canonical = std::fs::canonicalize(&folder).expect("canonical root");
        (root, folder, canonical)
    }

    #[test]
    fn a_relative_or_absolute_path_inside_the_workspace_resolves() {
        let (_guard, folder, canonical) = workspace();
        assert_eq!(
            resolve_in_workspace(&folder, "summary.md").expect("relative path"),
            canonical.join("summary.md")
        );
        assert_eq!(
            resolve_in_workspace(&folder, "reports/../summary.md").expect("normalized path"),
            canonical.join("summary.md")
        );
        let absolute = canonical.join("summary.md");
        assert_eq!(
            resolve_in_workspace(&folder, &absolute.display().to_string()).expect("absolute path"),
            absolute
        );
        let inspected = inspect_in_workspace(&folder, "summary.md");
        assert!(inspected.eligible);
        assert!(inspected.reason.is_none());
        assert_eq!(inspected.absolute_path, absolute.display().to_string());
    }

    #[test]
    fn a_parent_traversal_or_an_outside_path_is_refused() {
        let (_guard, folder, canonical) = workspace();
        let outside = canonical.parent().expect("parent").join("elsewhere.md");
        for requested in [
            "../elsewhere.md",
            "../../etc/passwd",
            "/etc/passwd",
            outside.display().to_string().as_str(),
            "",
            "   ",
        ] {
            let error = resolve_in_workspace(&folder, requested)
                .expect_err(&format!("{requested} must be refused"));
            assert!(
                error.contains("outside the Chat's workspace") || error.contains("empty"),
                "{requested}: {error}"
            );
            let inspected = inspect_in_workspace(&folder, requested);
            assert!(!inspected.eligible, "{requested}");
            assert!(inspected.reason.is_some(), "{requested}");
        }
    }

    #[test]
    fn a_missing_file_inside_the_workspace_is_ineligible_but_resolvable() {
        let (_guard, folder, canonical) = workspace();
        let inspected = inspect_in_workspace(&folder, "gone.md");
        assert_eq!(
            inspected.absolute_path,
            canonical.join("gone.md").display().to_string()
        );
        assert!(!inspected.eligible);
        assert_eq!(
            inspected.reason.as_deref(),
            Some("the file no longer exists")
        );
        // An action on it performs nothing rather than opening something else.
        let performed = perform_in_workspace(&folder, "gone.md", PathActionV1::Reveal, None)
            .expect("ineligible is not an error");
        assert!(!performed.performed);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_workspace_is_refused() {
        let (_guard, folder, canonical) = workspace();
        let outside = canonical.parent().expect("parent").join("outside.md");
        std::fs::write(&outside, b"# Outside").expect("outside file");
        let link = folder.join("link.md");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        let error = resolve_in_workspace(&folder, "link.md").expect_err("symlink escape");
        assert!(error.contains("outside the Chat's workspace"), "{error}");
    }

    #[test]
    fn inspect_performs_nothing_and_refuses_nothing_eligible() {
        let (_guard, folder, _canonical) = workspace();
        let inspected = perform_in_workspace(&folder, "summary.md", PathActionV1::Inspect, None)
            .expect("inspect succeeds");
        assert!(inspected.eligible);
        assert!(!inspected.performed);
    }

    #[test]
    fn platform_arguments_are_the_exact_ones_this_system_uses() {
        let path = Path::new("/tmp/project/summary.md");
        let (open, open_arguments) = open_argv(path);
        let (reveal, reveal_arguments) = reveal_argv(path);
        if cfg!(target_os = "macos") {
            assert_eq!(
                (open.as_str(), open_arguments),
                ("open", vec!["/tmp/project/summary.md".to_owned()])
            );
            assert_eq!(
                (reveal.as_str(), reveal_arguments),
                (
                    "open",
                    vec!["-R".to_owned(), "/tmp/project/summary.md".to_owned()]
                )
            );
        } else if cfg!(windows) {
            assert_eq!(open, "cmd");
            assert_eq!(open_arguments[1], "start");
            assert_eq!(open_arguments[3], "/tmp/project/summary.md");
            assert_eq!(reveal, "explorer");
            assert_eq!(
                reveal_arguments,
                vec!["/select,/tmp/project/summary.md".to_owned()]
            );
        } else {
            assert_eq!(
                (open.as_str(), open_arguments),
                ("xdg-open", vec!["/tmp/project/summary.md".to_owned()])
            );
            assert_eq!(
                (reveal.as_str(), reveal_arguments),
                ("xdg-open", vec!["/tmp/project".to_owned()])
            );
        }
        let (editor, editor_arguments) = editor_argv("code", path);
        assert_eq!(
            (editor.as_str(), editor_arguments),
            ("code", vec!["/tmp/project/summary.md".to_owned()])
        );
    }
}
