//! Anchored, durable native filesystem operations.
//!
//! Callers retain a directory identity instead of treating an ambient path as
//! authority. Every operation revalidates that identity and rejects aliases,
//! symlinks, mount crossings, and case-only component substitutions before IO.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::FileExt;
use same_file::Handle as SameFileHandle;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_RELATIVE_COMPONENTS: usize = 128;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Operating-system family compiled into this adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformFamilyV1 {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl PlatformFamilyV1 {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Fresh capability facts. A caller must not infer a stronger guarantee than
/// the booleans reported here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemCapabilityReportV1 {
    pub platform: PlatformFamilyV1,
    pub anchored_identity: bool,
    pub no_follow_components: bool,
    pub local_volume_proven: bool,
    pub atomic_file_replace: bool,
    pub atomic_directory_publish: bool,
    pub file_sync: bool,
    pub directory_sync: bool,
    pub ownership_observed: bool,
    pub writable_without_elevation: bool,
}

impl FilesystemCapabilityReportV1 {
    /// Whole-build publication and selectors require every durability fact.
    #[must_use]
    pub fn supports_managed_publication(&self) -> bool {
        self.anchored_identity
            && self.no_follow_components
            && self.local_volume_proven
            && self.atomic_file_replace
            && self.atomic_directory_publish
            && self.file_sync
            && self.directory_sync
            && self.ownership_observed
            && self.writable_without_elevation
    }
}

/// Platform object identity captured from the opened root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemObjectIdentityV1 {
    pub canonical_path: PathBuf,
    pub volume_identity: String,
    pub object_identity: String,
}

/// Safe canonical relative path below an anchored directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchoredRelativePath(PathBuf);

impl AnchoredRelativePath {
    pub fn parse(value: impl AsRef<Path>) -> Result<Self, NativeFilesystemError> {
        let path = value.as_ref();
        let text = path.to_string_lossy();
        let valid = !text.is_empty()
            && text.len() <= MAX_RELATIVE_PATH_BYTES
            && !text.contains(['\\', ':', '\0'])
            && !path.is_absolute()
            && path.components().count() <= MAX_RELATIVE_COMPONENTS
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_)));
        if valid {
            Ok(Self(path.to_path_buf()))
        } else {
            Err(NativeFilesystemError::UnsafeRelativePath)
        }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Evidence returned after a durable file publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicFileReceiptV1 {
    pub relative_path: PathBuf,
    pub content_hash: String,
    pub byte_size: u64,
    pub file_synced: bool,
    pub parent_directory_synced: bool,
    pub root_identity: FilesystemObjectIdentityV1,
}

/// Advisory single-machine writer guard. Immutable parent/head claims are
/// still required for cross-machine correctness.
pub struct AnchoredWriterGuard {
    file: File,
}

impl Drop for AnchoredWriterGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Directory authority anchored to one exact filesystem object.
#[derive(Debug)]
pub struct AnchoredDirectory {
    root: PathBuf,
    root_handle: SameFileHandle,
    identity: FilesystemObjectIdentityV1,
    capability: FilesystemCapabilityReportV1,
}

impl AnchoredDirectory {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, NativeFilesystemError> {
        let supplied = root.as_ref();
        let link_metadata = fs::symlink_metadata(supplied)?;
        if link_metadata.file_type().is_symlink() {
            return Err(NativeFilesystemError::AliasDenied);
        }
        if !link_metadata.is_dir() {
            return Err(NativeFilesystemError::NotDirectory);
        }
        let root = fs::canonicalize(supplied)?;
        let root_handle = SameFileHandle::from_path(&root)?;
        let identity = identify(&root)?;
        let directory_sync = sync_directory(&root).is_ok();
        let writable_without_elevation = probe_writable(&root)?;
        let capability = FilesystemCapabilityReportV1 {
            platform: PlatformFamilyV1::current(),
            anchored_identity: true,
            no_follow_components: true,
            local_volume_proven: local_volume_proven(&root),
            atomic_file_replace: cfg!(any(
                target_os = "windows",
                target_os = "macos",
                target_os = "linux"
            )),
            atomic_directory_publish: cfg!(any(
                target_os = "windows",
                target_os = "macos",
                target_os = "linux"
            )),
            file_sync: true,
            directory_sync,
            ownership_observed: ownership_observed(&link_metadata),
            writable_without_elevation,
        };
        Ok(Self {
            root,
            root_handle,
            identity,
            capability,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn identity(&self) -> &FilesystemObjectIdentityV1 {
        &self.identity
    }

    #[must_use]
    pub fn capability_report(&self) -> &FilesystemCapabilityReportV1 {
        &self.capability
    }

    pub fn revalidate(&self) -> Result<(), NativeFilesystemError> {
        if SameFileHandle::from_path(&self.root)? == self.root_handle
            && identify(&self.root)? == self.identity
        {
            Ok(())
        } else {
            Err(NativeFilesystemError::RootIdentityChanged)
        }
    }

    pub fn resolve_existing(
        &self,
        relative: &AnchoredRelativePath,
    ) -> Result<PathBuf, NativeFilesystemError> {
        self.revalidate()?;
        self.validate_components(relative.as_path(), true)?;
        let resolved = fs::canonicalize(self.root.join(relative.as_path()))?;
        if resolved.starts_with(&self.root) && same_volume(&self.identity, &identify(&resolved)?) {
            Ok(resolved)
        } else {
            Err(NativeFilesystemError::ContainmentChanged)
        }
    }

    pub fn read_bounded(
        &self,
        relative: &AnchoredRelativePath,
        maximum: usize,
    ) -> Result<Vec<u8>, NativeFilesystemError> {
        if maximum > MAX_FILE_BYTES {
            return Err(NativeFilesystemError::BoundExceeded);
        }
        let path = self.resolve_existing(relative)?;
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
        if length > maximum_u64 {
            return Err(NativeFilesystemError::BoundExceeded);
        }
        let capacity = usize::try_from(length).map_err(|_| NativeFilesystemError::BoundExceeded)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(maximum_u64.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(NativeFilesystemError::BoundExceeded);
        }
        Ok(bytes)
    }

    /// Returns a fresh identity for an existing contained object.
    pub fn identify_existing(
        &self,
        relative: &AnchoredRelativePath,
    ) -> Result<FilesystemObjectIdentityV1, NativeFilesystemError> {
        let path = self.resolve_existing(relative)?;
        identify(&path)
    }

    /// Flushes metadata for an existing contained directory.
    pub fn sync_existing_directory(
        &self,
        relative: &AnchoredRelativePath,
    ) -> Result<(), NativeFilesystemError> {
        let path = self.resolve_existing(relative)?;
        if !path.is_dir() {
            return Err(NativeFilesystemError::NotDirectory);
        }
        sync_directory(&path)
    }

    /// Writes one strictly sequential chunk into an unreachable staging file.
    /// The file is synced after each chunk so a crash never produces evidence
    /// claiming more durable bytes than storage actually accepted.
    pub fn append_staged_chunk(
        &self,
        relative: &AnchoredRelativePath,
        expected_offset: u64,
        bytes: &[u8],
    ) -> Result<(), NativeFilesystemError> {
        self.revalidate()?;
        self.validate_components(relative.as_path(), false)?;
        let path = self.root.join(relative.as_path());
        let parent = path
            .parent()
            .ok_or(NativeFilesystemError::UnsafeRelativePath)?;
        self.create_directories(parent)?;
        let mut file = OpenOptions::new()
            .create(expected_offset == 0)
            .append(true)
            .open(&path)?;
        if file.metadata()?.len() != expected_offset {
            return Err(NativeFilesystemError::ExpectedContentChanged);
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }

    pub fn acquire_writer_guard(
        &self,
        relative: &AnchoredRelativePath,
    ) -> Result<AnchoredWriterGuard, NativeFilesystemError> {
        self.revalidate()?;
        self.validate_components(relative.as_path(), false)?;
        let path = self.root.join(relative.as_path());
        if let Some(parent) = path.parent() {
            self.create_directories(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()
            .map_err(|_| NativeFilesystemError::WriterBusy)?;
        Ok(AnchoredWriterGuard { file })
    }

    pub fn create_new_durable(
        &self,
        relative: &AnchoredRelativePath,
        bytes: &[u8],
    ) -> Result<AtomicFileReceiptV1, NativeFilesystemError> {
        self.revalidate()?;
        self.validate_components(relative.as_path(), false)?;
        let path = self.root.join(relative.as_path());
        let parent = path
            .parent()
            .ok_or(NativeFilesystemError::UnsafeRelativePath)?;
        self.create_directories(parent)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        sync_directory(parent)?;
        self.receipt(relative, bytes)
    }

    pub fn replace_durable_expected(
        &self,
        relative: &AnchoredRelativePath,
        expected_content_hash: Option<&str>,
        bytes: &[u8],
    ) -> Result<AtomicFileReceiptV1, NativeFilesystemError> {
        self.revalidate()?;
        self.validate_components(relative.as_path(), false)?;
        let target = self.root.join(relative.as_path());
        let parent = target
            .parent()
            .ok_or(NativeFilesystemError::UnsafeRelativePath)?;
        self.create_directories(parent)?;
        match expected_content_hash {
            Some(expected) => {
                let current = self.read_bounded(relative, MAX_FILE_BYTES)?;
                if content_hash(&current) != expected {
                    return Err(NativeFilesystemError::ExpectedContentChanged);
                }
            }
            None if target.exists() => return Err(NativeFilesystemError::ExpectedContentChanged),
            None => {}
        }
        let mut temporary = NamedTempFile::new_in(parent)?;
        let write_result = (|| -> Result<(), NativeFilesystemError> {
            let file = temporary.as_file_mut();
            file.write_all(bytes)?;
            file.sync_all()?;
            if let Some(expected) = expected_content_hash {
                let current = self.read_bounded(relative, MAX_FILE_BYTES)?;
                if content_hash(&current) != expected {
                    return Err(NativeFilesystemError::ExpectedContentChanged);
                }
            }
            temporary
                .persist(&target)
                .map_err(|error| NativeFilesystemError::Io(error.error))?;
            sync_directory(parent)?;
            Ok(())
        })();
        write_result?;
        self.receipt(relative, bytes)
    }

    /// Publishes one complete same-volume directory without replacing an
    /// existing target. The temporary tree must already be fully synced.
    pub fn publish_directory(
        &self,
        temporary: &AnchoredRelativePath,
        target: &AnchoredRelativePath,
    ) -> Result<(), NativeFilesystemError> {
        self.revalidate()?;
        let temporary_path = self.resolve_existing(temporary)?;
        if !temporary_path.is_dir() {
            return Err(NativeFilesystemError::NotDirectory);
        }
        self.validate_components(target.as_path(), false)?;
        let target_path = self.root.join(target.as_path());
        if target_path.exists() {
            return Err(NativeFilesystemError::ExpectedContentChanged);
        }
        let target_parent = target_path
            .parent()
            .ok_or(NativeFilesystemError::UnsafeRelativePath)?;
        self.create_directories(target_parent)?;
        if !same_volume(&self.identity, &identify(&temporary_path)?) {
            return Err(NativeFilesystemError::CrossVolume);
        }
        fs::rename(&temporary_path, &target_path)?;
        sync_directory(target_parent)?;
        Ok(())
    }

    pub fn list_controlled_directory(
        &self,
        relative: &AnchoredRelativePath,
    ) -> Result<Vec<String>, NativeFilesystemError> {
        let path = self.resolve_existing(relative)?;
        let mut names = Vec::new();
        let mut folded = BTreeSet::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(NativeFilesystemError::AliasDenied);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| NativeFilesystemError::UnsafeRelativePath)?;
            if !folded.insert(name.to_ascii_lowercase()) {
                return Err(NativeFilesystemError::CaseCollision);
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    fn receipt(
        &self,
        relative: &AnchoredRelativePath,
        bytes: &[u8],
    ) -> Result<AtomicFileReceiptV1, NativeFilesystemError> {
        self.revalidate()?;
        Ok(AtomicFileReceiptV1 {
            relative_path: relative.as_path().to_path_buf(),
            content_hash: content_hash(bytes),
            byte_size: bytes.len() as u64,
            file_synced: true,
            parent_directory_synced: true,
            root_identity: self.identity.clone(),
        })
    }

    fn create_directories(&self, absolute: &Path) -> Result<(), NativeFilesystemError> {
        let relative = absolute
            .strip_prefix(&self.root)
            .map_err(|_| NativeFilesystemError::ContainmentChanged)?;
        let mut current = self.root.clone();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(NativeFilesystemError::UnsafeRelativePath);
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(NativeFilesystemError::AliasDenied);
                }
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(NativeFilesystemError::NotDirectory),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current)?;
                    sync_directory(current.parent().unwrap_or(&self.root))?;
                }
                Err(error) => return Err(error.into()),
            }
            if !same_volume(&self.identity, &identify(&current)?) {
                return Err(NativeFilesystemError::CrossVolume);
            }
        }
        Ok(())
    }

    fn validate_components(
        &self,
        path: &Path,
        require_final: bool,
    ) -> Result<(), NativeFilesystemError> {
        let relative = AnchoredRelativePath::parse(path)?;
        let count = relative.as_path().components().count();
        let mut current = self.root.clone();
        for (index, component) in relative.as_path().components().enumerate() {
            let Component::Normal(component) = component else {
                return Err(NativeFilesystemError::UnsafeRelativePath);
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(NativeFilesystemError::AliasDenied);
                }
                Ok(_) => {
                    verify_exact_case(&current, component)?;
                    if !same_volume(&self.identity, &identify(&current)?) {
                        return Err(NativeFilesystemError::CrossVolume);
                    }
                }
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
}

fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn verify_exact_case(path: &Path, expected: &std::ffi::OsStr) -> Result<(), NativeFilesystemError> {
    let parent = path
        .parent()
        .ok_or(NativeFilesystemError::ContainmentChanged)?;
    if fs::read_dir(parent)?.any(|entry| entry.is_ok_and(|entry| entry.file_name() == expected)) {
        Ok(())
    } else {
        Err(NativeFilesystemError::CaseCollision)
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), NativeFilesystemError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(path: &Path) -> Result<(), NativeFilesystemError> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS makes CreateFileW open a real directory
    // handle, and write access lets FlushFileBuffers commit the directory
    // entries so renames and creates survive a crash.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    directory
        .sync_all()
        .map_err(|_| NativeFilesystemError::DirectorySyncUnavailable)
}

fn probe_writable(root: &Path) -> Result<bool, NativeFilesystemError> {
    let probe = root.join(format!(
        ".aworkit-write-probe-{}-{}",
        std::process::id(),
        TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    match OpenOptions::new().create_new(true).write(true).open(&probe) {
        Ok(file) => {
            file.sync_all()?;
            fs::remove_file(&probe)?;
            sync_directory(root)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn ownership_observed(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn ownership_observed(_metadata: &fs::Metadata) -> bool {
    // The helper-controlled Windows root is admitted only after its ACL has
    // made it writable to the current unelevated token; the write probe below
    // revalidates that effective access without inheriting a caller flag.
    !_metadata.permissions().readonly()
}

fn local_volume_proven(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        // Kernel pseudo/network mounts are rejected by their canonical mount
        // table entries. Unknown entries are conservatively treated as local;
        // cross-device traversal is still rejected by object identity.
        let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") else {
            return false;
        };
        let canonical = path.to_string_lossy();
        let mut best: Option<(&str, &str)> = None;
        for line in mounts.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let fields: Vec<_> = left.split_whitespace().collect();
            let right_fields: Vec<_> = right.split_whitespace().collect();
            if fields.len() < 5 || right_fields.is_empty() {
                continue;
            }
            let mount = fields[4];
            if canonical.starts_with(mount)
                && best.is_none_or(|(current, _)| mount.len() > current.len())
            {
                best = Some((mount, right_fields[0]));
            }
        }
        return best.is_some_and(|(_, kind)| {
            !matches!(
                kind,
                "9p" | "afs" | "cifs" | "fuse" | "fuseblk" | "nfs" | "nfs4" | "smb3"
            )
        });
    }
    #[cfg(target_os = "macos")]
    {
        return rustix::fs::statfs(path)
            .is_ok_and(|facts| (facts.f_flags as i32 & libc::MNT_LOCAL) == libc::MNT_LOCAL);
    }
    #[cfg(target_os = "windows")]
    {
        use std::path::Prefix;
        return path.components().next().is_some_and(|component| {
            matches!(
                component,
                Component::Prefix(prefix)
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            )
        });
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        false
    }
}

#[cfg(unix)]
fn identify(path: &Path) -> Result<FilesystemObjectIdentityV1, NativeFilesystemError> {
    use std::os::unix::fs::MetadataExt;
    let canonical_path = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical_path)?;
    Ok(FilesystemObjectIdentityV1 {
        canonical_path,
        volume_identity: format!("unix-dev:{}", metadata.dev()),
        object_identity: format!("unix-dev:{}-ino:{}", metadata.dev(), metadata.ino()),
    })
}

#[cfg(not(unix))]
fn identify(path: &Path) -> Result<FilesystemObjectIdentityV1, NativeFilesystemError> {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    let canonical_path = fs::canonicalize(path)?;
    let handle = SameFileHandle::from_path(&canonical_path)?;
    let volume = canonical_path
        .components()
        .next()
        .map_or_else(String::new, |value| format!("{value:?}"));
    let mut object_hasher = DefaultHasher::new();
    handle.hash(&mut object_hasher);
    let object = format!(
        "sha256:{:x}",
        Sha256::digest(object_hasher.finish().to_be_bytes())
    );
    Ok(FilesystemObjectIdentityV1 {
        canonical_path,
        volume_identity: volume,
        object_identity: object,
    })
}

fn same_volume(root: &FilesystemObjectIdentityV1, child: &FilesystemObjectIdentityV1) -> bool {
    root.volume_identity == child.volume_identity
}

#[derive(Debug, Error)]
pub enum NativeFilesystemError {
    #[error("path is not a safe canonical relative path")]
    UnsafeRelativePath,
    #[error("symbolic link, reparse point, or equivalent alias was denied")]
    AliasDenied,
    #[error("path is not a directory")]
    NotDirectory,
    #[error("anchored root identity changed")]
    RootIdentityChanged,
    #[error("path containment or object identity changed")]
    ContainmentChanged,
    #[error("path crossed onto another volume or mount")]
    CrossVolume,
    #[error("case-colliding or case-substituted path was denied")]
    CaseCollision,
    #[error("bounded filesystem operation exceeded its limit")]
    BoundExceeded,
    #[error("expected file contents changed before publication")]
    ExpectedContentChanged,
    #[error("another local writer owns the branch guard")]
    WriterBusy,
    #[error("filesystem object kind is unsupported")]
    UnsupportedObject,
    #[error("atomic replacement is unavailable on this filesystem")]
    AtomicReplaceUnavailable,
    #[error("directory durability is unavailable on this filesystem")]
    DirectorySyncUnavailable,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_create_replace_and_directory_publish_are_identity_bound() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let root = AnchoredDirectory::open(temporary.path()).expect("anchored root");
        let pointer = AnchoredRelativePath::parse("refs/main").expect("reference");
        let first = root
            .create_new_durable(&pointer, b"first")
            .expect("durable create");
        let second = root
            .replace_durable_expected(&pointer, Some(&first.content_hash), b"second")
            .expect("guarded replace");
        assert_eq!(root.read_bounded(&pointer, 16).expect("read"), b"second");
        assert_ne!(first.content_hash, second.content_hash);

        fs::create_dir(temporary.path().join("staging")).expect("staging");
        fs::write(temporary.path().join("staging/app"), b"bundle").expect("bundle");
        root.publish_directory(
            &AnchoredRelativePath::parse("staging").expect("staging reference"),
            &AnchoredRelativePath::parse("slots/build-a").expect("slot reference"),
        )
        .expect("publish directory");
        assert_eq!(
            root.read_bounded(
                &AnchoredRelativePath::parse("slots/build-a/app").expect("entry reference"),
                16
            )
            .expect("entry"),
            b"bundle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliases_and_mount_or_root_identity_changes_fail_closed() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary root");
        let outside = tempfile::tempdir().expect("outside root");
        let root = AnchoredDirectory::open(temporary.path()).expect("anchored root");
        symlink(outside.path(), temporary.path().join("alias")).expect("symlink");
        let error = root
            .resolve_existing(&AnchoredRelativePath::parse("alias").expect("reference"))
            .expect_err("alias denied");
        assert!(matches!(error, NativeFilesystemError::AliasDenied));
    }

    #[test]
    fn filesystem_capability_report_distinguishes_supported_and_degraded() {
        let root = tempfile::tempdir().expect("capability root");
        let supported = AnchoredDirectory::open(root.path())
            .expect("open capability root")
            .capability_report()
            .clone();
        assert!(supported.supports_managed_publication());
        let degraded = FilesystemCapabilityReportV1 {
            local_volume_proven: false,
            ..supported
        };
        assert!(!degraded.supports_managed_publication());
    }
}
