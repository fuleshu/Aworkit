//! The standard skills that ship inside Aworkit.
//!
//! They live in `desktop/skills`, are copied into the installed application by
//! the desktop bundle, and are always the **last** discovery root, so a project
//! or user skill of the same name still wins and an upgrade replaces them
//! without touching anything the user wrote.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Folder name inside the installed application resource directory.
const INSTALLED_FOLDER: &str = "skills";

/// Development and CI override for the installed copy.
const OVERRIDE_ENV: &str = "AWORKIT_BUNDLED_SKILLS_DIR";

/// The standard skills every Aworkit installation ships. Read by the tests that
/// hold this folder, the workflow node catalog and the history schema in step.
#[cfg(test)]
pub(crate) const STANDARD_SKILLS: &[&str] = &[
    "aworkit-skills",
    "aworkit-plugins",
    "aworkit-workflows",
    "aworkit-chat-storage",
];

static INSTALLED_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Records the installed resource directory once, during desktop startup.
///
/// A development run has no resources yet, so the source tree stays the
/// fallback and the app demonstrates the same skills it ships.
pub fn register_installed_root(resource_dir: impl AsRef<Path>) {
    let _ = INSTALLED_ROOT.set(resource_dir.as_ref().join(INSTALLED_FOLDER));
}

/// Resolves the bundled skill folder for one frozen Chat configuration.
///
/// A non-blank `bundledSkillDir` setting is used as given: it replaces the
/// bundled default. A blank setting resolves the override, then the installed
/// resources, then this repository's own `desktop/skills` folder. `None` means
/// no bundled folder exists, which only leaves those skills out - every user
/// and project skill is discovered either way.
pub(crate) fn resolve_root(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(configured) = configured.filter(|value| !value.trim().is_empty()) {
        return Some(PathBuf::from(configured));
    }
    select_default(
        std::env::var_os(OVERRIDE_ENV).map(PathBuf::from),
        INSTALLED_ROOT.get().cloned(),
        source_tree(),
    )
}

/// The folder in this repository; present in a development build only.
pub(crate) fn source_tree() -> Option<PathBuf> {
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../skills"))
}

/// The ordered candidates as pure inputs, so the choice itself is testable.
pub(super) fn select_default(
    environment: Option<PathBuf>,
    installed: Option<PathBuf>,
    source: Option<PathBuf>,
) -> Option<PathBuf> {
    [environment, installed, source]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.is_absolute() && candidate.is_dir())
}
