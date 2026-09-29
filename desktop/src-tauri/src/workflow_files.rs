//! Native file handling for the Workflow designer's Import and Export surface.
//!
//! The native workflow library remains the one canonical store, so a workflow
//! file is only a copy the user chose: nothing here parses, validates, repairs,
//! or interprets a document, and nothing here writes anywhere except the path
//! the user picked in the operating system's own dialog.
//!
//! Replacing an existing file is a two-step protocol. [`write_document`] refuses
//! to overwrite unless its caller passes `overwrite`, so the webview must first
//! ask the user through the application's own confirmation dialog and then write
//! again. A webview that forgot to ask cannot destroy a file by accident, and a
//! refused write leaves the existing document byte-for-byte untouched.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::{DialogExt, FilePath};

/// Largest workflow document this surface reads or writes.
pub const MAXIMUM_WORKFLOW_FILE_BYTES: usize = 4 * 1024 * 1024;

/// Suffix Aworkit suggests for a workflow document it writes itself.
pub const WORKFLOW_FILE_SUFFIX: &str = ".aworkit.json";

/// Bound on the file name a caller may propose to the save dialog.
const MAXIMUM_SUGGESTED_FILE_NAME_BYTES: usize = 96;

/// What one workflow-file write did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowFileWriteOutcomeV1 {
    /// The complete document was written to the chosen path.
    Written,
    /// The path already holds a file and the caller had not confirmed replacing it.
    Exists,
}

/// Shows the operating system's open dialog for one workflow document to import.
///
/// Returns the chosen path as UTF-8 text, or `None` when the user cancelled.
pub fn pick_open_path<R: Runtime>(app: &AppHandle<R>) -> Result<Option<String>, String> {
    let chosen = crate::dialog_session::seed(
        app.dialog()
            .file()
            .set_title("Import workflow")
            .add_filter("Aworkit workflow", &["aworkit.json"])
            .add_filter("JSON", &["json"]),
    )
    .blocking_pick_file();
    if let Some(path) = chosen.as_ref() {
        crate::dialog_session::remember_chosen_file(path);
    }
    chosen.map(path_text).transpose()
}

/// Shows the operating system's save dialog for exporting one workflow document.
///
/// `suggested_name` only seeds the dialog's file name; the chosen path is the
/// only thing that matters, so a file name and a workflow name may differ.
pub fn pick_save_path<R: Runtime>(
    app: &AppHandle<R>,
    suggested_name: Option<&str>,
) -> Result<Option<String>, String> {
    let mut dialog = crate::dialog_session::seed(
        app.dialog()
            .file()
            .set_title("Export workflow")
            .add_filter("Aworkit workflow", &["aworkit.json"])
            .add_filter("JSON", &["json"]),
    );
    if let Some(name) = suggested_name {
        dialog = dialog.set_file_name(suggested_file_name(name));
    }
    let chosen = dialog.blocking_save_file();
    if let Some(path) = chosen.as_ref() {
        crate::dialog_session::remember_chosen_file(path);
    }
    chosen.map(path_text).transpose()
}

/// Reads one workflow document from `path` as bounded UTF-8 text.
///
/// A directory, a missing file, an unreadable file, a document larger than
/// [`MAXIMUM_WORKFLOW_FILE_BYTES`], and a file that is not UTF-8 text are each
/// reported as their own error; no caller can be handed a partial document.
pub fn read_document(path: &str) -> Result<String, String> {
    let path = Path::new(path);
    require_absolute(path)?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if metadata.len() > MAXIMUM_WORKFLOW_FILE_BYTES as u64 {
        return Err(oversize_message(path, metadata.len()));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    // The file may have grown between the metadata check and the read.
    if bytes.len() > MAXIMUM_WORKFLOW_FILE_BYTES {
        return Err(oversize_message(path, bytes.len() as u64));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "{} is not UTF-8 text, so it is not a workflow document",
            path.display()
        )
    })
}

/// Writes one complete workflow document to `path`.
///
/// The document is written to a temporary file in the same directory and then
/// renamed over the target, so an interrupted or failed write cannot leave a
/// half-written workflow behind. An existing file is only replaced when the
/// caller passes `overwrite`, which the designer does after the user confirmed.
pub fn write_document(
    path: &str,
    contents: &str,
    overwrite: bool,
) -> Result<WorkflowFileWriteOutcomeV1, String> {
    let path = Path::new(path);
    require_absolute(path)?;
    if contents.len() > MAXIMUM_WORKFLOW_FILE_BYTES {
        return Err(oversize_message(path, contents.len() as u64));
    }
    if let Some(existing) = existing_file(path)? {
        if !overwrite {
            return Ok(WorkflowFileWriteOutcomeV1::Exists);
        }
        if !existing.is_file() {
            return Err(format!(
                "{} exists and is not a regular file",
                path.display()
            ));
        }
    }
    let parent = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no containing folder", path.display()))?;
    if !parent.is_dir() {
        return Err(format!(
            "the folder {} does not exist",
            parent.display()
        ));
    }
    let temporary = temporary_path(parent, path.file_name());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot write {}: {error}", temporary.display()));
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("cannot replace {}: {error}", path.display())
    })?;
    Ok(WorkflowFileWriteOutcomeV1::Written)
}

/// Makes the file name a caller proposes a safe leaf name for the save dialog.
///
/// Only the dialog's suggestion is affected: the user may type anything, so what
/// is chosen wins, and a workflow name and a file name may always differ. Path
/// separators, control characters, and other non-portable characters are
/// dropped or replaced, so a workflow name can never turn the suggestion into a
/// path, and an unusable suggestion falls back to the Aworkit suffix.
pub fn suggested_file_name(workflow_name: &str) -> String {
    let mut safe = String::new();
    for character in workflow_name.trim().chars() {
        let keep = character.is_alphanumeric() || matches!(character, '-' | '_' | '.');
        if keep {
            safe.push(character);
        } else if character.is_whitespace() && !safe.ends_with('-') && !safe.is_empty() {
            safe.push('-');
        }
        if safe.len() >= MAXIMUM_SUGGESTED_FILE_NAME_BYTES {
            break;
        }
    }
    let trimmed = safe.trim_matches(|character: char| character == '.' || character == '-');
    if trimmed.is_empty() {
        return format!("workflow{WORKFLOW_FILE_SUFFIX}");
    }
    trimmed.to_owned()
}

fn require_absolute(path: &Path) -> Result<(), String> {
    if path.is_absolute() && path.file_name().is_some() {
        return Ok(());
    }
    Err(format!(
        "{} is not an absolute file path",
        path.display()
    ))
}

fn oversize_message(path: &Path, bytes: u64) -> String {
    format!(
        "{} is {bytes} bytes, larger than the {MAXIMUM_WORKFLOW_FILE_BYTES}-byte workflow document bound",
        path.display()
    )
}

/// Reports whether `path` already holds something, without following a broken link.
fn existing_file(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

/// A same-directory temporary name that no other writer can already hold.
fn temporary_path(parent: &Path, file_name: Option<&OsStr>) -> PathBuf {
    let stem = file_name
        .and_then(OsStr::to_str)
        .unwrap_or("workflow")
        .replace(['/', '\\'], "-");
    let process = std::process::id();
    let mut ordinal = 0_u32;
    loop {
        let candidate = parent.join(format!(".{stem}.aworkit-{process}-{ordinal}.tmp"));
        if !candidate.exists() {
            return candidate;
        }
        ordinal += 1;
    }
}

/// Converts a chosen dialog path into the UTF-8 text the webview receives.
fn path_text(file: FilePath) -> Result<String, String> {
    let path = file
        .into_path()
        .map_err(|error| format!("the chosen location is not a local file path: {error}"))?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "the chosen path is not valid UTF-8 text".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_is_atomic_and_refuses_an_unconfirmed_overwrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("draft.aworkit.json");
        let first = write_document(
            target.to_str().expect("utf-8 path"),
            "{\"schemaVersion\":1}",
            false,
        )
        .expect("first write");
        assert_eq!(first, WorkflowFileWriteOutcomeV1::Written);
        assert_eq!(
            fs::read_to_string(&target).expect("written document"),
            "{\"schemaVersion\":1}"
        );

        // An unconfirmed write reports the existing file and changes nothing.
        let refused = write_document(
            target.to_str().expect("utf-8 path"),
            "{\"schemaVersion\":9}",
            false,
        )
        .expect("refused write");
        assert_eq!(refused, WorkflowFileWriteOutcomeV1::Exists);
        assert_eq!(
            fs::read_to_string(&target).expect("untouched document"),
            "{\"schemaVersion\":1}"
        );

        // A confirmed write replaces the document and leaves no temporary file.
        let replaced = write_document(
            target.to_str().expect("utf-8 path"),
            "{\"schemaVersion\":2}",
            true,
        )
        .expect("confirmed write");
        assert_eq!(replaced, WorkflowFileWriteOutcomeV1::Written);
        assert_eq!(
            fs::read_to_string(&target).expect("replaced document"),
            "{\"schemaVersion\":2}"
        );
        let leftovers = fs::read_dir(directory.path())
            .expect("directory listing")
            .filter(|entry| {
                entry
                    .as_ref()
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn read_reports_each_way_a_file_cannot_be_a_workflow_document() {
        assert!(read_document("relative/path.json").is_err());
        assert!(read_document("/definitely/missing.aworkit.json").is_err());

        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(read_document(directory.path().to_str().expect("utf-8 path")).is_err());

        let binary = directory.path().join("image.aworkit.json");
        fs::write(&binary, [0xff, 0xfe, 0x00]).expect("fixture bytes");
        assert!(read_document(binary.to_str().expect("utf-8 path")).is_err());

        let oversize = directory.path().join("huge.aworkit.json");
        fs::write(&oversize, vec![b' '; MAXIMUM_WORKFLOW_FILE_BYTES + 1]).expect("fixture bytes");
        assert!(read_document(oversize.to_str().expect("utf-8 path")).is_err());

        let document = directory.path().join("readable.aworkit.json");
        fs::write(&document, "{\"schemaVersion\":1}").expect("fixture document");
        assert_eq!(
            read_document(document.to_str().expect("utf-8 path")).expect("readable document"),
            "{\"schemaVersion\":1}"
        );
    }

    #[test]
    fn write_rejects_paths_and_documents_outside_its_bounds() {
        let directory = tempfile::tempdir().expect("temporary directory");
        assert!(write_document("relative.aworkit.json", "{}", false).is_err());
        assert!(
            write_document(
                directory
                    .path()
                    .join(format!("huge{WORKFLOW_FILE_SUFFIX}"))
                    .to_str()
                    .expect("utf-8 path"),
                &"x".repeat(MAXIMUM_WORKFLOW_FILE_BYTES + 1),
                false,
            )
            .is_err()
        );
        assert!(
            write_document(
                directory
                    .path()
                    .join("missing")
                    .join(format!("draft{WORKFLOW_FILE_SUFFIX}"))
                    .to_str()
                    .expect("utf-8 path"),
                "{}",
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn suggested_file_name_is_a_bounded_leaf_name() {
        assert_eq!(
            suggested_file_name("research-agent.aworkit.json"),
            "research-agent.aworkit.json"
        );
        assert_eq!(suggested_file_name("Repository Engineer"), "Repository-Engineer");
        assert_eq!(suggested_file_name("   "), "workflow.aworkit.json");
        assert_eq!(suggested_file_name("../../../etc/passwd"), "etcpasswd");
        let name = suggested_file_name(&"a".repeat(400));
        assert_eq!(name.len(), MAXIMUM_SUGGESTED_FILE_NAME_BYTES);
    }
}
