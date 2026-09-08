//! Provider boundary. Native reads reuse the existing rooted file authority;
//! a remote/provider-visible implementation can supply the same observations.

use crate::{CancellationToken, FileAuthority, ProjectFiles};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileObservation {
    Present(String),
    Absent,
    Unavailable(String),
}

pub trait InstructionFiles {
    /// Implementations must enforce the limit while streaming, not just at stat.
    fn read(
        &self,
        path: &Path,
        limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<FileObservation, String>;
    fn exists(&self, path: &Path, cancellation: &CancellationToken) -> Result<bool, String>;
}

/// Only these explicitly frozen roots are accessible. Failure never falls back
/// to ambient host reads. ProjectFiles also revalidates directory identity.
pub struct ProjectInstructionFiles {
    roots: Vec<(PathBuf, Result<Option<ProjectFiles>, String>)>,
}

impl ProjectInstructionFiles {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .map(|root| {
                    let files = match ProjectFiles::new(FileAuthority {
                        root: root.clone(),
                        allow_write: false,
                    }) {
                        Ok(files) => Ok(Some(files)),
                        Err(crate::FileToolError::Io(e))
                            if e.kind() == std::io::ErrorKind::NotFound =>
                        {
                            Ok(None)
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    (root, files)
                })
                .collect(),
        }
    }

    fn resolve(&self, path: &Path) -> Result<(Option<&ProjectFiles>, PathBuf), String> {
        let (root, files) = self
            .roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .ok_or("instruction path is outside frozen filesystem authority")?;
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        // ProjectFiles' portable authority contract uses forward slashes even
        // when the frozen Windows root is a verbatim canonical path.
        let relative = PathBuf::from(relative.to_string_lossy().replace('\\', "/"));
        Ok((files.as_ref().map_err(Clone::clone)?.as_ref(), relative))
    }
}

impl InstructionFiles for ProjectInstructionFiles {
    fn read(
        &self,
        path: &Path,
        limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<FileObservation, String> {
        super::check_cancelled(cancellation)?;
        let (files, relative) = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(error) => return Ok(FileObservation::Unavailable(error)),
        };
        match files {
            Some(files) => files.instruction_read(&relative, limit, cancellation),
            None => Ok(FileObservation::Absent),
        }
    }

    fn exists(&self, path: &Path, cancellation: &CancellationToken) -> Result<bool, String> {
        super::check_cancelled(cancellation)?;
        let (files, relative) = self.resolve(path)?;
        match files {
            Some(files) => files.instruction_exists(&relative, cancellation),
            None => Ok(false),
        }
    }
}
