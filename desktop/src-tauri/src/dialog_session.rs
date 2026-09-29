//! Session memory of the folder each native chooser should open in.
//!
//! Every browse starts where the user last chose, so a run of the application
//! does not reopen its own installation directory for each dialog. The memory
//! is process-wide and deliberately not persisted: it lasts for one run, which
//! is what the file managers' own recent-folder behaviour gives the user.
//!
//! The remembered folder is a convenience, never an authority: a chooser still
//! returns whatever absolute path the user picks.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::Runtime;
use tauri_plugin_dialog::{FileDialogBuilder, FilePath};

static LAST_DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The folder a chooser opens in, once one has been used this session.
pub fn starting_directory() -> Option<PathBuf> {
    LAST_DIRECTORY
        .lock()
        .ok()
        .and_then(|current| current.clone())
}

/// Seeds a chooser with the folder this session last browsed to.
pub fn seed<R: Runtime>(dialog: FileDialogBuilder<R>) -> FileDialogBuilder<R> {
    match starting_directory() {
        Some(directory) => dialog.set_directory(directory),
        None => dialog,
    }
}

/// Remembers the folder holding a chosen file; the next chooser opens there.
pub fn remember_chosen_file(chosen: &FilePath) {
    if let Some(path) = chosen_path(chosen) {
        if let Some(parent) = path.parent() {
            remember(parent);
        }
    }
}

/// Remembers a chosen folder itself; the next chooser opens inside it.
pub fn remember_chosen_folder(chosen: &FilePath) {
    if let Some(path) = chosen_path(chosen) {
        remember(&path);
    }
}

/// `FilePath` is a plain path on desktop and a URI on the mobile shells; both
/// resolve to the same local path here, and an unresolvable one is simply not
/// remembered.
fn chosen_path(chosen: &FilePath) -> Option<PathBuf> {
    if let Some(path) = chosen.as_path() {
        return Some(path.to_path_buf());
    }
    chosen.clone().into_path().ok()
}

fn remember(directory: &Path) {
    if directory.as_os_str().is_empty() {
        return;
    }
    if let Ok(mut current) = LAST_DIRECTORY.lock() {
        *current = Some(directory.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_dialog::FilePath;

    #[test]
    fn remembers_the_folder_each_kind_of_choice_implies() {
        // One test owns the process-wide memory, so its sequence is explicit.
        *LAST_DIRECTORY.lock().unwrap() = None;
        assert_eq!(starting_directory(), None);

        // A chosen file seeds the folder that holds it.
        remember_chosen_file(&FilePath::Path(PathBuf::from("/work/repo/notes.txt")));
        assert_eq!(starting_directory(), Some(PathBuf::from("/work/repo")));

        // A chosen folder seeds that folder itself, which is where the next
        // browse of the same kind should open.
        remember_chosen_folder(&FilePath::Path(PathBuf::from("/work/repo/assets")));
        assert_eq!(starting_directory(), Some(PathBuf::from("/work/repo/assets")));

        // A path without a parent is not remembered as an empty folder.
        remember_chosen_file(&FilePath::Path(PathBuf::from("notes.txt")));
        assert_eq!(starting_directory(), Some(PathBuf::from("/work/repo/assets")));
    }
}
