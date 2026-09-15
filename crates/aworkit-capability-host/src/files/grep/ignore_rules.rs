//! Ignore rules for the regex walk: what the project itself says is not source.
//!
//! A project already declares what is not source — `.gitignore`, `.ignore` and
//! `.rgignore`, at any depth — and those declarations are authoritative, because a
//! hand-written list of build directories can only ever name the ones someone
//! thought of. The conventional generated directory names stay as a fallback for
//! trees that declare nothing at all.

use std::path::{Component, Path, PathBuf};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};

use super::super::ProjectFiles;

/// Ignore file names honored in every directory, in increasing precedence: a
/// `.rgignore` declaration outranks an `.ignore` one, which outranks `.gitignore`,
/// matching the order they are added to one directory's matcher.
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".rgignore"];

/// Directories inspected above the capability root. A repository marker stops the
/// climb earlier; the cap only bounds a search started in a tree that has none.
const MAXIMUM_ANCESTOR_DIRECTORIES: usize = 32;

/// Machine-generated dependency and build directories that are skipped even when
/// no ignore file names them. They are the fallback, not the policy: a repository
/// that declares its own build output is honored through its ignore files, and a
/// directory that follows none of these conventions is only skipped when the
/// project says so.
const GENERATED_DIRECTORIES: [&str; 25] = [
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".pnpm-store",
    ".yarn",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".angular",
    ".turbo",
    ".parcel-cache",
    ".cache",
    ".output",
    ".gradle",
    ".tox",
    "__pycache__",
    ".mypy_cache",
    ".venv",
    "venv",
];

/// True when a directory name is a conventional generated tree.
pub(super) fn is_generated_directory(name: &str) -> bool {
    GENERATED_DIRECTORIES
        .iter()
        .any(|generated| name.eq_ignore_ascii_case(generated))
}

/// One directory's parsed ignore declarations.
struct IgnoreFrame {
    matcher: Gitignore,
}

/// Every ignore declaration in force at one point of the walk: the directories
/// above the capability root (outermost first), the root itself, and the
/// directories between the root and the search start. A deeper declaration
/// outranks a shallower one, which is how git resolves the same files.
pub(super) struct IgnoreRules {
    frames: Vec<IgnoreFrame>,
}

impl IgnoreRules {
    /// Rules for a search that starts at `scope`, relative to `capability_root`
    /// (`""` when the search starts at the root itself).
    pub(super) fn for_search(
        files: &ProjectFiles,
        capability_root: &Path,
        scope: &Path,
    ) -> Self {
        let mut frames = Vec::new();
        // Above the capability the walk cannot go, so these declarations are read
        // through the ambient filesystem: they are configuration, never search
        // results, and an unreadable one is simply absent.
        for directory in ancestors(capability_root) {
            if let Some(frame) = ambient_frame(&directory) {
                frames.push(frame);
            }
        }
        // Inside the capability every declaration is read through the capability:
        // the root itself, then each directory on the way down to the scope.
        let mut inside: Vec<PathBuf> = vec![PathBuf::new()];
        for component in scope.components() {
            if let Component::Normal(name) = component {
                let mut nested = inside.last().cloned().unwrap_or_default();
                nested.push(name);
                inside.push(nested);
            }
        }
        for directory in inside {
            if let Some(frame) = capability_frame(files, capability_root, &directory) {
                frames.push(frame);
            }
        }
        Self { frames }
    }

    /// Adds the declarations of one directory the walk has just entered.
    pub(super) fn enter(
        &mut self,
        files: &ProjectFiles,
        capability_root: &Path,
        directory: &Path,
    ) -> usize {
        match capability_frame(files, capability_root, directory) {
            Some(frame) => {
                self.frames.push(frame);
                1
            }
            None => 0,
        }
    }

    /// Drops the declarations `enter` added for one directory.
    pub(super) fn leave(&mut self, entered: usize) {
        self.frames
            .truncate(self.frames.len().saturating_sub(entered));
    }

    /// True when the project's own declarations exclude this entry. The deepest
    /// declaration wins, so a nested `!keep` re-includes what a root pattern
    /// excluded, exactly as git resolves the same pair of files.
    pub(super) fn is_ignored(&self, capability_root: &Path, relative: &Path, is_dir: bool) -> bool {
        if self.frames.is_empty() {
            return false;
        }
        let absolute = capability_root.join(relative);
        for frame in self.frames.iter().rev() {
            match frame.matcher.matched(&absolute, is_dir) {
                Match::None => continue,
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
            }
        }
        false
    }
}

/// Directories above the capability root, outermost first, ending at the
/// repository root when the tree has one. The capability root itself is excluded:
/// it is read through the capability, not through the ambient filesystem.
fn ancestors(capability_root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = capability_root
        .ancestors()
        .take(MAXIMUM_ANCESTOR_DIRECTORIES)
        .map(Path::to_path_buf)
        .collect();
    if let Some(repository) = found
        .iter()
        .position(|directory| directory.join(".git").exists())
    {
        found.truncate(repository + 1);
    }
    found.remove(0);
    found.reverse();
    found
}

/// One directory's declarations read through the ambient filesystem. Only
/// directories above the capability root use it, because the capability cannot
/// name its own ancestors.
fn ambient_frame(directory: &Path) -> Option<IgnoreFrame> {
    let mut builder = GitignoreBuilder::new(directory);
    let mut declared = false;
    for name in IGNORE_FILE_NAMES {
        let path = directory.join(name);
        if path.is_file() {
            declared = true;
            // A malformed or unreadable declaration is treated as absent.
            let _ = builder.add(&path);
        }
    }
    declared
        .then(|| builder.build().ok())
        .flatten()
        .map(|matcher| IgnoreFrame { matcher })
}

/// One directory's declarations read through the capability, so a declaration
/// inside the search root is reached exactly like the content it filters.
fn capability_frame(
    files: &ProjectFiles,
    capability_root: &Path,
    directory: &Path,
) -> Option<IgnoreFrame> {
    let absolute = capability_root.join(directory);
    let mut builder = GitignoreBuilder::new(&absolute);
    let mut declared = false;
    for name in IGNORE_FILE_NAMES {
        let Some(content) = files.read_configuration(&directory.join(name)) else {
            continue;
        };
        declared = true;
        let source = absolute.join(name);
        for line in content.lines() {
            let _ = builder.add_line(Some(source.clone()), line);
        }
    }
    declared
        .then(|| builder.build().ok())
        .flatten()
        .map(|matcher| IgnoreFrame { matcher })
}
