//! Root-anchored portable filesystem access.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

/// A validated project-relative reference; it cannot carry an absolute path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectReference(String);
impl ProjectReference {
    pub fn parse(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        let path = Path::new(&value);
        if value.is_empty()
            || value.contains(['\\', ':', '\0'])
            || path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(WorkspaceError::UnsafeReference);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Project root that permits only symlink-safe relative reads and create-new writes.
#[derive(Clone, Debug)]
pub struct WorkspaceRoot {
    root: PathBuf,
}
impl WorkspaceRoot {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }
    pub fn resolve_existing(
        &self,
        reference: &ProjectReference,
    ) -> Result<PathBuf, WorkspaceError> {
        let path = fs::canonicalize(self.root.join(reference.as_str()))?;
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(WorkspaceError::Escape)
        }
    }
    pub fn resolve_new(&self, reference: &ProjectReference) -> Result<PathBuf, WorkspaceError> {
        let path = self.root.join(reference.as_str());
        let parent = path.parent().ok_or(WorkspaceError::UnsafeReference)?;
        let parent = nearest_existing(parent)?;
        if !parent.starts_with(&self.root) {
            return Err(WorkspaceError::Escape);
        }
        Ok(path)
    }
    pub fn read(&self, reference: &ProjectReference) -> Result<Vec<u8>, WorkspaceError> {
        Ok(fs::read(self.resolve_existing(reference)?)?)
    }
}
fn nearest_existing(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Ok(fs::canonicalize(candidate)?);
        }
        candidate = candidate.parent().ok_or(WorkspaceError::Escape)?;
    }
}
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("project reference is not a safe relative path")]
    UnsafeReference,
    #[error("project reference escapes its configured root")]
    Escape,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
