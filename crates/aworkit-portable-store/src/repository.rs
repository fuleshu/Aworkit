//! Crash-consistent immutable object publication.

use crate::{
    codec::digest,
    workspace::{ProjectReference, WorkspaceError, WorkspaceRoot},
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    pub fn publish(&self, namespace: &str, bytes: &[u8]) -> Result<String, PortableError> {
        if !valid_namespace(namespace) {
            return Err(PortableError::InvalidNamespace);
        }
        let identity = digest(namespace, bytes);
        let hex = identity.strip_prefix("sha256:").expect("format");
        let reference =
            ProjectReference::parse(format!(".aworkit/portable/{namespace}/sha256/{hex}"))?;
        let target = self.root.resolve_new(&reference)?;
        if target.exists() {
            let existing = fs::read(&target)?;
            if existing == bytes {
                return Ok(identity);
            }
            return Err(PortableError::HashCollision);
        }
        let parent = target.parent().ok_or(PortableError::InvalidNamespace)?;
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".{}.{}.tmp",
            hex,
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::rename(&temp, &target) {
            Ok(()) => {
                sync_parent(parent)?;
                Ok(identity)
            }
            Err(error) => {
                let _ = fs::remove_file(temp);
                Err(error.into())
            }
        }
    }
    pub fn read(&self, namespace: &str, identity: &str) -> Result<Vec<u8>, PortableError> {
        let hex = identity
            .strip_prefix("sha256:")
            .ok_or(PortableError::InvalidHash)?;
        let reference =
            ProjectReference::parse(format!(".aworkit/portable/{namespace}/sha256/{hex}"))?;
        let bytes = self.root.read(&reference)?;
        if digest(namespace, &bytes) != identity {
            return Err(PortableError::CorruptObject);
        }
        Ok(bytes)
    }
}
fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
}
#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), std::io::Error> {
    std::fs::File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_parent(_: &Path) -> Result<(), std::io::Error> {
    Ok(())
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
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
