//! Capability-rooted portable filesystem access.
//!
//! Paths remain relative after validation and all I/O is resolved by an open
//! directory handle.  A renamed or replaced project root is rejected rather
//! than silently changing the authority granted at open time.

use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use aworkit_process::filesystem::{AnchoredDirectory, FilesystemCapabilityReportV1};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions},
};
use thiserror::Error;

pub const MAX_PROJECT_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_REFERENCE_BYTES: usize = 4096;
const MAX_REFERENCE_COMPONENTS: usize = 128;

/// A validated project-relative reference; it cannot carry aliases which have
/// platform-dependent meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectReference(String);

impl ProjectReference {
    pub fn parse(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        let path = Path::new(&value);
        let valid = !value.is_empty()
            && value.len() <= MAX_REFERENCE_BYTES
            && !value.contains(['\\', ':', '\0'])
            && !path.is_absolute()
            && path.components().count() <= MAX_REFERENCE_COMPONENTS
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        if valid {
            Ok(Self(value))
        } else {
            Err(WorkspaceError::UnsafeReference)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn path(&self) -> &Path {
        Path::new(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitFactAvailabilityV1 {
    Complete,
    RepositoryAbsent,
    Partial { reason: String },
}

/// Descriptive read-only facts. No API in this adapter can stage, commit,
/// merge, fetch, push, or select a branch tip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitWorktreeFactsV1 {
    pub availability: GitFactAvailabilityV1,
    pub branch_reference: Option<String>,
    pub head_commit: Option<String>,
    pub dirty_state_digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootIdentity {
    canonical_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// An authority-bearing project root.  The ambient path is retained only for
/// identity diagnostics; reads and writes use `directory`.
#[derive(Clone)]
pub struct WorkspaceRoot {
    root: PathBuf,
    directory: Arc<Dir>,
    identity: RootIdentity,
    native: Arc<AnchoredDirectory>,
}

impl std::fmt::Debug for WorkspaceRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceRoot")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl WorkspaceRoot {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(WorkspaceError::NotDirectory);
        }
        let identity = root_identity(&root)?;
        let native = AnchoredDirectory::open(&root)
            .map_err(|error| WorkspaceError::Native(error.to_string()))?;
        let directory = Dir::open_ambient_dir(&root, ambient_authority())?;
        Ok(Self {
            root,
            directory: Arc::new(directory),
            identity,
            native: Arc::new(native),
        })
    }

    /// Diagnostic path only.  Repository operations remain capability rooted.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Fresh native guarantees used to decide whether portable writes are
    /// durable, local, and safe enough or must remain read-only/degraded.
    #[must_use]
    pub fn filesystem_capabilities(&self) -> &FilesystemCapabilityReportV1 {
        self.native.capability_report()
    }

    pub fn resolve_existing(
        &self,
        reference: &ProjectReference,
    ) -> Result<PathBuf, WorkspaceError> {
        self.revalidate()?;
        self.reject_symlinks(reference.path(), true)?;
        let path = fs::canonicalize(self.root.join(reference.as_str()))?;
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(WorkspaceError::Escape)
        }
    }

    pub fn resolve_new(&self, reference: &ProjectReference) -> Result<PathBuf, WorkspaceError> {
        self.revalidate()?;
        self.reject_symlinks(reference.path(), false)?;
        Ok(self.root.join(reference.as_str()))
    }

    pub fn read(&self, reference: &ProjectReference) -> Result<Vec<u8>, WorkspaceError> {
        self.read_bounded(reference, MAX_PROJECT_FILE_BYTES)
    }

    pub fn read_bounded(
        &self,
        reference: &ProjectReference,
        maximum: usize,
    ) -> Result<Vec<u8>, WorkspaceError> {
        self.revalidate()?;
        self.reject_symlinks(reference.path(), true)?;
        let file = self.directory.open(reference.path())?;
        let length = file.metadata()?.len();
        if length > maximum as u64 {
            return Err(WorkspaceError::FileTooLarge);
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take((maximum as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(WorkspaceError::FileTooLarge);
        }
        Ok(bytes)
    }

    pub fn inspect_git_read_only(&self) -> Result<GitWorktreeFactsV1, WorkspaceError> {
        let git_entry = ProjectReference::parse(".git")?;
        match self.directory.symlink_metadata(git_entry.path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GitWorktreeFactsV1 {
                    availability: GitFactAvailabilityV1::RepositoryAbsent,
                    branch_reference: None,
                    head_commit: None,
                    dirty_state_digest: None,
                });
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkspaceError::SymlinkDenied);
            }
            Ok(metadata) if metadata.is_file() => {
                // Linked worktrees commonly point outside the project root.
                // Following that path would broaden this adapter's authority.
                return Ok(GitWorktreeFactsV1 {
                    availability: GitFactAvailabilityV1::Partial {
                        reason: "linked_worktree_gitdir_is_outside_project_authority".into(),
                    },
                    branch_reference: None,
                    head_commit: None,
                    dirty_state_digest: None,
                });
            }
            Ok(_) => {}
        }
        let head =
            String::from_utf8(self.read_bounded(&ProjectReference::parse(".git/HEAD")?, 4096)?)
                .map_err(|_| WorkspaceError::InvalidGitFacts)?;
        let head = head.trim_end_matches(['\r', '\n']);
        let (branch_reference, head_commit) = if let Some(reference) = head.strip_prefix("ref: ") {
            let reference = ProjectReference::parse(format!(".git/{reference}"))?;
            let commit = match self.read_bounded(&reference, 4096) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map_err(|_| WorkspaceError::InvalidGitFacts)?
                    .trim()
                    .to_owned(),
                Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    packed_ref(self, &reference.as_str()[5..])?
                        .ok_or(WorkspaceError::InvalidGitFacts)?
                }
                Err(error) => return Err(error),
            };
            (Some(reference.as_str()[5..].to_owned()), Some(commit))
        } else {
            (None, Some(head.to_owned()))
        };
        if head_commit.as_deref().is_none_or(valid_git_object_id) {
            Ok(GitWorktreeFactsV1 {
                availability: GitFactAvailabilityV1::Partial {
                    reason: "dirty_state_not_computed_without_mutating_or_trusting_git_index"
                        .into(),
                },
                branch_reference,
                head_commit,
                dirty_state_digest: None,
            })
        } else {
            Err(WorkspaceError::InvalidGitFacts)
        }
    }

    pub(crate) fn create_dir_all(&self, path: &Path) -> Result<(), WorkspaceError> {
        self.revalidate()?;
        validate_relative_path(path)?;
        self.reject_symlinks(path, false)?;
        self.directory.create_dir_all(path)?;
        self.reject_symlinks(path, true)
    }

    pub(crate) fn open_with(
        &self,
        reference: &ProjectReference,
        options: &OpenOptions,
        require_final: bool,
    ) -> Result<File, WorkspaceError> {
        self.revalidate()?;
        self.reject_symlinks(reference.path(), require_final)?;
        Ok(self.directory.open_with(reference.path(), options)?)
    }

    pub(crate) fn publish_create_new(
        &self,
        temporary: &ProjectReference,
        target: &ProjectReference,
        bytes: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.require_durable_write_capabilities()?;
        self.revalidate()?;
        self.reject_symlinks(temporary.path(), false)?;
        self.reject_symlinks(target.path(), false)?;
        let parent = temporary
            .path()
            .parent()
            .ok_or(WorkspaceError::UnsafeReference)?;
        self.create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.directory.open_with(temporary.path(), &options)?;
        let write_result = (|| -> Result<(), WorkspaceError> {
            file.write_all(bytes)?;
            file.sync_all()?;
            // A hard-link insertion is create-new at the destination. Unlike
            // rename it cannot replace an object won by a concurrent writer.
            self.directory
                .hard_link(temporary.path(), &self.directory, target.path())?;
            self.directory.remove_file(temporary.path())?;
            self.sync_directory(parent)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = self.directory.remove_file(temporary.path());
        }
        write_result
    }

    pub(crate) fn replace_atomically(
        &self,
        temporary: &ProjectReference,
        target: &ProjectReference,
        bytes: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.require_durable_write_capabilities()?;
        self.revalidate()?;
        self.reject_symlinks(temporary.path(), false)?;
        self.reject_symlinks(target.path(), false)?;
        let parent = temporary
            .path()
            .parent()
            .ok_or(WorkspaceError::UnsafeReference)?;
        self.create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.directory.open_with(temporary.path(), &options)?;
        let write_result = (|| -> Result<(), WorkspaceError> {
            file.write_all(bytes)?;
            file.sync_all()?;
            self.directory
                .rename(temporary.path(), &self.directory, target.path())?;
            self.sync_directory(parent)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = self.directory.remove_file(temporary.path());
        }
        write_result
    }

    pub(crate) fn remove_file(&self, reference: &ProjectReference) -> Result<(), WorkspaceError> {
        self.revalidate()?;
        self.reject_symlinks(reference.path(), true)?;
        Ok(self.directory.remove_file(reference.path())?)
    }

    pub(crate) fn read_dir_names(&self, path: &Path) -> Result<Vec<String>, WorkspaceError> {
        self.revalidate()?;
        validate_relative_path(path)?;
        self.reject_symlinks(path, true)?;
        let mut names = Vec::new();
        let mut folded = std::collections::BTreeSet::new();
        for entry in self.directory.read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(WorkspaceError::SymlinkDenied);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| WorkspaceError::UnsafeReference)?;
            if !folded.insert(name.to_ascii_lowercase()) {
                return Err(WorkspaceError::CaseAliasDenied);
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    fn sync_directory(&self, path: &Path) -> Result<(), WorkspaceError> {
        // `open_dir` may use an O_PATH descriptor on Linux, which cannot be
        // fsynced. Opening the directory read-only yields an fsync-capable
        // handle while remaining rooted in the capability.
        let path = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };
        #[cfg(unix)]
        {
            self.directory.open(path)?.sync_all()?;
        }
        // On Windows `FlushFileBuffers` requires a directory handle opened with
        // write access and FILE_FLAG_BACKUP_SEMANTICS, so the read-only
        // capability handle cannot be flushed. The caller has already
        // revalidated the pinned root, so resolving the validated relative
        // path against it is safe.
        #[cfg(not(unix))]
        {
            sync_directory_native(&self.root.join(path))?;
        }
        Ok(())
    }

    fn revalidate(&self) -> Result<(), WorkspaceError> {
        self.native
            .revalidate()
            .map_err(|error| WorkspaceError::Native(error.to_string()))?;
        if root_identity(&self.root)? == self.identity {
            Ok(())
        } else {
            Err(WorkspaceError::RootChanged)
        }
    }

    fn require_durable_write_capabilities(&self) -> Result<(), WorkspaceError> {
        let capability = self.native.capability_report();
        if capability.anchored_identity
            && capability.no_follow_components
            && capability.local_volume_proven
            && capability.atomic_file_replace
            && capability.file_sync
            && capability.directory_sync
        {
            Ok(())
        } else {
            Err(WorkspaceError::DurableWriteUnavailable)
        }
    }

    fn reject_symlinks(&self, path: &Path, require_final: bool) -> Result<(), WorkspaceError> {
        validate_relative_path(path)?;
        let mut current = PathBuf::new();
        let count = path.components().count();
        for (index, component) in path.components().enumerate() {
            let Component::Normal(component) = component else {
                return Err(WorkspaceError::UnsafeReference);
            };
            current.push(component);
            match self.directory.symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(WorkspaceError::SymlinkDenied);
                }
                Ok(_) => self.verify_exact_component_case(&current, component)?,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && (!require_final || index + 1 == count) =>
                {
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn verify_exact_component_case(
        &self,
        current: &Path,
        expected_name: &std::ffi::OsStr,
    ) -> Result<(), WorkspaceError> {
        let parent = current.parent().unwrap_or_else(|| Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let mut exact = false;
        for entry in self.directory.read_dir(parent)? {
            let entry = entry?;
            if entry.file_name() == expected_name {
                exact = true;
                break;
            }
        }
        if exact {
            Ok(())
        } else {
            Err(WorkspaceError::CaseAliasDenied)
        }
    }
}

fn packed_ref(root: &WorkspaceRoot, reference: &str) -> Result<Option<String>, WorkspaceError> {
    let packed = match root.read_bounded(&ProjectReference::parse(".git/packed-refs")?, 1024 * 1024)
    {
        Ok(bytes) => bytes,
        Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let text = String::from_utf8(packed).map_err(|_| WorkspaceError::InvalidGitFacts)?;
    for line in text.lines() {
        if line.starts_with(['#', '^']) {
            continue;
        }
        if let Some((object, name)) = line.split_once(' ') {
            if name == reference && valid_git_object_id(object) {
                return Ok(Some(object.to_owned()));
            }
        }
    }
    Ok(None)
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_relative_path(path: &Path) -> Result<(), WorkspaceError> {
    let text = path.to_string_lossy();
    if path.is_absolute()
        || text.contains(['\\', ':', '\0'])
        || path.components().count() > MAX_REFERENCE_COMPONENTS
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(WorkspaceError::UnsafeReference)
    } else {
        Ok(())
    }
}

/// Flushes a directory's entries so renames and creates survive a crash.
///
/// A capability-rooted directory handle is read-only on Windows, which
/// `FlushFileBuffers` rejects. The directory is therefore reopened with write
/// access and `FILE_FLAG_BACKUP_SEMANTICS`, mirroring the platform helper in
/// `aworkit-process`.
#[cfg(not(unix))]
fn sync_directory_native(path: &Path) -> Result<(), WorkspaceError> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    directory.sync_all()?;
    Ok(())
}

fn root_identity(path: &Path) -> Result<RootIdentity, std::io::Error> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(RootIdentity {
            canonical_path: fs::canonicalize(path)?,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(RootIdentity {
            canonical_path: fs::canonicalize(path)?,
        })
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("project reference is not a safe relative path")]
    UnsafeReference,
    #[error("project reference escapes its configured root")]
    Escape,
    #[error("symbolic links and reparse-like aliases are denied")]
    SymlinkDenied,
    #[error("configured project root changed identity")]
    RootChanged,
    #[error("configured project root is not a directory")]
    NotDirectory,
    #[error("project reference casing does not match the opened directory entry")]
    CaseAliasDenied,
    #[error("descriptive Git metadata is malformed")]
    InvalidGitFacts,
    #[error("project file exceeds its bounded size")]
    FileTooLarge,
    #[error("native filesystem guarantee failed: {0}")]
    Native(String),
    #[error("portable workspace is read-only because native durability is unavailable")]
    DurableWriteUnavailable,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
