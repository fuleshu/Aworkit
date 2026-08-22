//! Capability-rooted project file read, search, and atomic edit tools.

use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::CancellationToken;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 1024;

#[derive(Clone, Debug)]
pub struct FileAuthority {
    pub root: PathBuf,
    pub allow_write: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReadRequestV1 {
    pub path: PathBuf,
    pub maximum_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReadResultV1 {
    pub bytes: Vec<u8>,
    pub content_hash: String,
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSearchRequestV1 {
    pub path: PathBuf,
    pub needle: String,
    pub maximum_results: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileEffectKindV1 {
    Read,
    Search,
    Edit,
}

/// Durable callers can normalize this descriptor without inferring effects
/// from unstructured adapter output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEffectDescriptorV1 {
    pub kind: FileEffectKindV1,
    pub relative_path: PathBuf,
    pub before_content_hash: String,
    pub after_content_hash: String,
    pub bytes_observed_or_written: usize,
    pub write_committed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSearchResultV1 {
    pub offsets: Vec<usize>,
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEditRequestV1 {
    pub path: PathBuf,
    pub expected_content_hash: String,
    pub replacement: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEditResultV1 {
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootIdentity {
    canonical_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Project tool rooted in an open directory capability, not a working-directory claim.
#[derive(Clone)]
pub struct ProjectFiles {
    authority: FileAuthority,
    directory: Arc<Dir>,
    identity: RootIdentity,
    mutation_lock: Arc<Mutex<()>>,
}

impl ProjectFiles {
    pub fn new(authority: FileAuthority) -> Result<Self, FileToolError> {
        let canonical_root = fs::canonicalize(&authority.root)?;
        if !canonical_root.is_dir() {
            return Err(FileToolError::OutsideRoot);
        }
        let identity = root_identity(&canonical_root)?;
        let directory = Dir::open_ambient_dir(&canonical_root, ambient_authority())?;
        Ok(Self {
            authority: FileAuthority {
                root: canonical_root,
                ..authority
            },
            directory: Arc::new(directory),
            identity,
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn read(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, FileToolError> {
        Ok(self
            .read_v1(
                &FileReadRequestV1 {
                    path: path.as_ref().to_path_buf(),
                    maximum_bytes: MAX_FILE_BYTES,
                },
                &CancellationToken::default(),
            )?
            .bytes)
    }

    pub fn read_v1(
        &self,
        request: &FileReadRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileReadResultV1, FileToolError> {
        if request.maximum_bytes == 0 || request.maximum_bytes > MAX_FILE_BYTES {
            return Err(FileToolError::TooLarge);
        }
        check_cancelled(cancellation)?;
        self.revalidate_root()?;
        let path = validate_relative(&request.path)?;
        self.reject_symlinks(path, true)?;
        let mut file = self.directory.open(path)?;
        let length = file.metadata()?.len();
        if length > request.maximum_bytes as u64 {
            return Err(FileToolError::TooLarge);
        }
        let mut body = Vec::with_capacity(length as usize);
        let mut chunk = [0_u8; 8192];
        loop {
            check_cancelled(cancellation)?;
            let count = file.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            if body.len().saturating_add(count) > request.maximum_bytes {
                return Err(FileToolError::TooLarge);
            }
            body.extend_from_slice(&chunk[..count]);
        }
        Ok(FileReadResultV1 {
            content_hash: content_hash(&body),
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Read,
                relative_path: path.to_path_buf(),
                before_content_hash: content_hash(&body),
                after_content_hash: content_hash(&body),
                bytes_observed_or_written: body.len(),
                write_committed: false,
            },
            bytes: body,
        })
    }

    pub fn search(
        &self,
        path: impl AsRef<Path>,
        needle: &str,
    ) -> Result<Vec<usize>, FileToolError> {
        Ok(self
            .search_v1(
                &FileSearchRequestV1 {
                    path: path.as_ref().to_path_buf(),
                    needle: needle.to_owned(),
                    maximum_results: MAX_SEARCH_RESULTS,
                },
                &CancellationToken::default(),
            )?
            .offsets)
    }

    pub fn search_v1(
        &self,
        request: &FileSearchRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileSearchResultV1, FileToolError> {
        if request.needle.is_empty()
            || request.needle.len() > 64 * 1024
            || request.maximum_results == 0
            || request.maximum_results > MAX_SEARCH_RESULTS
        {
            return Err(FileToolError::InvalidSearch);
        }
        let read = self.read_v1(
            &FileReadRequestV1 {
                path: request.path.clone(),
                maximum_bytes: MAX_FILE_BYTES,
            },
            cancellation,
        )?;
        let text = String::from_utf8(read.bytes).map_err(|_| FileToolError::NotText)?;
        let mut results = Vec::new();
        for (index, _) in text.match_indices(&request.needle) {
            check_cancelled(cancellation)?;
            results.push(index);
            if results.len() == request.maximum_results {
                break;
            }
        }
        Ok(FileSearchResultV1 {
            offsets: results,
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Search,
                relative_path: request.path.clone(),
                before_content_hash: read.content_hash.clone(),
                after_content_hash: read.content_hash,
                bytes_observed_or_written: text.len(),
                write_committed: false,
            },
        })
    }

    /// Compatibility edit checks exact expected bytes before using the atomic hash path.
    pub fn edit(
        &self,
        path: impl AsRef<Path>,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<(), FileToolError> {
        if expected.len() > MAX_FILE_BYTES {
            return Err(FileToolError::TooLarge);
        }
        self.edit_hash(path, &content_hash(expected), replacement)
    }

    /// Replaces one file atomically only if its current content identity still matches.
    pub fn edit_hash(
        &self,
        path: impl AsRef<Path>,
        expected_hash: &str,
        replacement: &[u8],
    ) -> Result<(), FileToolError> {
        self.edit_v1(
            &FileEditRequestV1 {
                path: path.as_ref().to_path_buf(),
                expected_content_hash: expected_hash.to_owned(),
                replacement: replacement.to_vec(),
            },
            &CancellationToken::default(),
        )?;
        Ok(())
    }

    pub fn edit_v1(
        &self,
        request: &FileEditRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileEditResultV1, FileToolError> {
        if !self.authority.allow_write {
            return Err(FileToolError::WriteDenied);
        }
        if request.replacement.len() > MAX_FILE_BYTES {
            return Err(FileToolError::TooLarge);
        }
        check_cancelled(cancellation)?;
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| FileToolError::Poisoned)?;
        self.revalidate_root()?;
        let path = validate_relative(&request.path)?;
        self.reject_symlinks(path, true)?;
        let current = self.read(path)?;
        if content_hash(&current) != request.expected_content_hash {
            return Err(FileToolError::Conflict);
        }
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(FileToolError::OutsideRoot)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FileToolError::Clock)?
            .as_nanos();
        let temporary = parent.join(format!(".{file_name}.{nonce}.aworkit-tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut staged = self.directory.open_with(&temporary, &options)?;
        let staged_result = (|| -> Result<(), FileToolError> {
            staged.write_all(&request.replacement)?;
            staged.sync_all()?;
            check_cancelled(cancellation)?;
            if content_hash(&self.read(path)?) != request.expected_content_hash {
                return Err(FileToolError::Conflict);
            }
            self.directory.rename(&temporary, &self.directory, path)?;
            let sync_parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            self.directory.open(sync_parent)?.sync_all()?;
            Ok(())
        })();
        if staged_result.is_err() {
            let _ = self.directory.remove_file(&temporary);
        }
        staged_result?;
        Ok(FileEditResultV1 {
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Edit,
                relative_path: path.to_path_buf(),
                before_content_hash: request.expected_content_hash.clone(),
                after_content_hash: content_hash(&request.replacement),
                bytes_observed_or_written: request.replacement.len(),
                write_committed: true,
            },
        })
    }

    fn revalidate_root(&self) -> Result<(), FileToolError> {
        if root_identity(&self.authority.root)? == self.identity {
            Ok(())
        } else {
            Err(FileToolError::RootChanged)
        }
    }

    fn reject_symlinks(&self, path: &Path, require_final: bool) -> Result<(), FileToolError> {
        let mut current = PathBuf::new();
        let count = path.components().count();
        for (index, component) in path.components().enumerate() {
            let Component::Normal(component) = component else {
                return Err(FileToolError::OutsideRoot);
            };
            current.push(component);
            match self.directory.symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(FileToolError::SymlinkDenied);
                }
                Ok(_) => {}
                Err(error) if !require_final && index + 1 == count => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return Err(error.into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), FileToolError> {
    if cancellation.is_cancelled() {
        Err(FileToolError::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_relative(path: &Path) -> Result<&Path, FileToolError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_string_lossy().contains(['\\', ':', '\0'])
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.components().count() > 128
        || path.as_os_str().len() > 4096
    {
        return Err(FileToolError::OutsideRoot);
    }
    Ok(path)
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

fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Error)]
pub enum FileToolError {
    #[error("file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("path is outside the configured root")]
    OutsideRoot,
    #[error("symbolic links and reparse-like aliases are denied")]
    SymlinkDenied,
    #[error("configured root identity changed")]
    RootChanged,
    #[error("file exceeds its bound")]
    TooLarge,
    #[error("file is not UTF-8 text")]
    NotText,
    #[error("search query is invalid")]
    InvalidSearch,
    #[error("write authority denied")]
    WriteDenied,
    #[error("optimistic content identity conflict")]
    Conflict,
    #[error("file mutation lock is unavailable")]
    Poisoned,
    #[error("system clock is unavailable")]
    Clock,
    #[error("file operation was cancelled before its atomic effect")]
    Cancelled,
}
