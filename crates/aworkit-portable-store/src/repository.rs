//! Crash-consistent immutable object publication.

use crate::{
    codec::digest,
    workspace::{ProjectReference, WorkspaceError, WorkspaceRoot},
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub const MAX_PORTABLE_OBJECT_BYTES: usize = 16 * 1024 * 1024;
const NAMESPACES: &[&str] = &[
    "segments",
    "checkpoints",
    "artifacts",
    "manifests",
    "claims",
];

/// Fixed paths within the selected portable project root.
#[derive(Clone, Debug)]
pub struct PortablePaths {
    root: WorkspaceRoot,
}

impl PortablePaths {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PortableError> {
        Ok(Self {
            root: WorkspaceRoot::open(root)?,
        })
    }

    #[must_use]
    pub fn root(&self) -> &WorkspaceRoot {
        &self.root
    }

    /// Publishes bytes under their domain-separated content identity.  The
    /// destination insertion is create-new, so a concurrent writer can never
    /// replace an immutable object.
    pub fn publish(&self, namespace: &str, bytes: &[u8]) -> Result<String, PortableError> {
        validate_namespace(namespace)?;
        if bytes.is_empty() || bytes.len() > MAX_PORTABLE_OBJECT_BYTES {
            return Err(PortableError::ObjectSize);
        }
        let identity = digest(namespace, bytes);
        let target = object_reference(namespace, &identity)?;
        match self.root.read_bounded(&target, MAX_PORTABLE_OBJECT_BYTES) {
            Ok(existing) if existing == bytes => return Ok(identity),
            Ok(_) => return Err(PortableError::HashCollision),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let parent = target
            .path()
            .parent()
            .ok_or(PortableError::InvalidNamespace)?;
        self.root.create_dir_all(parent)?;
        let hex = identity
            .strip_prefix("sha256:")
            .ok_or(PortableError::InvalidHash)?;
        let temporary = ProjectReference::parse(format!(
            "{}/.{hex}.{}.{}.tmp",
            parent.to_string_lossy(),
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))?;
        match self.root.publish_create_new(&temporary, &target, bytes) {
            Ok(()) => Ok(identity),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self.root.read_bounded(&target, MAX_PORTABLE_OBJECT_BYTES)?;
                if existing == bytes {
                    Ok(identity)
                } else {
                    Err(PortableError::HashCollision)
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn read(&self, namespace: &str, identity: &str) -> Result<Vec<u8>, PortableError> {
        validate_namespace(namespace)?;
        let reference = object_reference(namespace, identity)?;
        let bytes = self
            .root
            .read_bounded(&reference, MAX_PORTABLE_OBJECT_BYTES)?;
        if digest(namespace, &bytes) != identity {
            return Err(PortableError::CorruptObject);
        }
        Ok(bytes)
    }

    pub fn contains(&self, namespace: &str, identity: &str) -> Result<bool, PortableError> {
        match self.read(namespace, identity) {
            Ok(_) => Ok(true),
            Err(PortableError::Workspace(WorkspaceError::Io(error)))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Removes a content object only after the caller has completed the
    /// conservative reachability protocol.  This API still revalidates the
    /// content identity before deletion.
    pub fn remove_verified(&self, namespace: &str, identity: &str) -> Result<(), PortableError> {
        let _ = self.read(namespace, identity)?;
        let reference = object_reference(namespace, identity)?;
        self.root.remove_file(&reference)?;
        Ok(())
    }

    pub(crate) fn read_relative(
        &self,
        reference: &ProjectReference,
        maximum: usize,
    ) -> Result<Vec<u8>, PortableError> {
        Ok(self.root.read_bounded(reference, maximum)?)
    }

    pub(crate) fn replace_relative(
        &self,
        temporary: &ProjectReference,
        target: &ProjectReference,
        bytes: &[u8],
    ) -> Result<(), PortableError> {
        Ok(self.root.replace_atomically(temporary, target, bytes)?)
    }

    pub(crate) fn publish_relative_immutable(
        &self,
        target: &ProjectReference,
        bytes: &[u8],
    ) -> Result<(), PortableError> {
        if bytes.is_empty() || bytes.len() > MAX_PORTABLE_OBJECT_BYTES {
            return Err(PortableError::ObjectSize);
        }
        match self.root.read_bounded(target, MAX_PORTABLE_OBJECT_BYTES) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => return Err(PortableError::HashCollision),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = target
            .path()
            .parent()
            .ok_or(PortableError::InvalidNamespace)?;
        self.root.create_dir_all(parent)?;
        let temporary = ProjectReference::parse(format!(
            "{}/.named.{}.{}.tmp",
            parent.to_string_lossy(),
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))?;
        match self.root.publish_create_new(&temporary, target, bytes) {
            Ok(()) => Ok(()),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self.root.read_bounded(target, MAX_PORTABLE_OBJECT_BYTES)?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(PortableError::HashCollision)
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn create_dir_all(&self, path: &Path) -> Result<(), PortableError> {
        Ok(self.root.create_dir_all(path)?)
    }

    pub(crate) fn open_lock(&self) -> Result<File, PortableError> {
        let directory = Path::new(".aworkit/portable");
        self.root.create_dir_all(directory)?;
        let reference = ProjectReference::parse(".aworkit/portable/.heads.lock")?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        Ok(self.root.open_with(&reference, &options, false)?.into_std())
    }

    pub(crate) fn list_ref_files(&self) -> Result<Vec<String>, PortableError> {
        match self
            .root
            .read_dir_names(Path::new(".aworkit/portable/refs"))
        {
            Ok(names) => Ok(names),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn list_object_ids(&self, namespace: &str) -> Result<Vec<String>, PortableError> {
        validate_namespace(namespace)?;
        let path = PathBuf::from(format!(".aworkit/portable/{namespace}/sha256"));
        match self.root.read_dir_names(&path) {
            Ok(names) => names
                .into_iter()
                .map(|name| {
                    let identity = format!("sha256:{name}");
                    let _ = object_reference(namespace, &identity)?;
                    Ok(identity)
                })
                .collect(),
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn validate_namespace(value: &str) -> Result<(), PortableError> {
    if NAMESPACES.contains(&value) {
        Ok(())
    } else {
        Err(PortableError::InvalidNamespace)
    }
}

fn object_reference(namespace: &str, identity: &str) -> Result<ProjectReference, PortableError> {
    validate_namespace(namespace)?;
    let hex = identity
        .strip_prefix("sha256:")
        .ok_or(PortableError::InvalidHash)?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(PortableError::InvalidHash);
    }
    Ok(ProjectReference::parse(format!(
        ".aworkit/portable/{namespace}/sha256/{hex}"
    ))?)
}

#[derive(Debug, Error)]
pub enum PortableError {
    #[error("portable namespace is invalid")]
    InvalidNamespace,
    #[error("portable identity is invalid")]
    InvalidHash,
    #[error("portable content hash collision")]
    HashCollision,
    #[error("portable object corruption detected")]
    CorruptObject,
    #[error("portable object violates its size bound")]
    ObjectSize,
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
