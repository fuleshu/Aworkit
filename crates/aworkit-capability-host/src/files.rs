//! Capability-rooted project file read, search, and atomic edit tools.

use std::{
    collections::BTreeSet,
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
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::CancellationToken;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 1024;
const MAX_LIST_ENTRIES: usize = 1000;
const MAX_LIST_SCANNED_ENTRIES: u64 = 100_000;
const MAX_GREP_MATCHES: usize = 512;
const MAX_GREP_FILES: usize = 128;

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
    List,
    Grep,
    Edit,
    Write,
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

/// Bounded glob listing beneath the root. Patterns use `*`, `**`, and `?`
/// segments; results are relative paths sorted by modification time, newest
/// first, capped at `maximum_entries`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileListRequestV1 {
    pub pattern: String,
    pub maximum_entries: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileListResultV1 {
    pub entries: Vec<FileListEntryV1>,
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileListEntryV1 {
    pub path: String,
    pub size_bytes: u64,
    pub modified_epoch_millis: u64,
}

/// Bounded regex search across files beneath the root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileGrepRequestV1 {
    pub pattern: String,
    pub maximum_matches: usize,
    pub maximum_files: usize,
    pub maximum_file_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileGrepResultV1 {
    pub matches: Vec<FileGrepMatchV1>,
    pub files_scanned: usize,
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileGrepMatchV1 {
    pub path: String,
    pub line: u64,
    pub offset: usize,
    pub line_text: String,
}

/// Full-content create-or-replace write beneath the root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileWriteRequestV1 {
    pub path: PathBuf,
    pub content: Vec<u8>,
    pub expected_content_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileWriteResultV1 {
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootIdentity {
    canonical_path: PathBuf,
    handle: Arc<same_file::Handle>,
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
            // Directory-entry durability: fsync the parent on Unix. Windows
            // exposes no directory fsync through cap-std; the rename itself
            // is committed by the OS before it returns.
            #[cfg(unix)]
            {
                let sync_parent = if parent.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    parent
                };
                self.directory.open(sync_parent)?.sync_all()?;
            }
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

    /// Lists files matching a bounded glob beneath the root, newest first.
    pub fn list_v1(
        &self,
        request: &FileListRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileListResultV1, FileToolError> {
        if request.pattern.is_empty()
            || request.pattern.len() > 4096
            || request.pattern.contains('\0')
            || request.maximum_entries == 0
            || request.maximum_entries > MAX_LIST_ENTRIES
        {
            return Err(FileToolError::InvalidList);
        }
        let segments = glob_segments(&request.pattern)?;
        self.revalidate_root()?;
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        let mut scanned = 0_u64;
        self.collect_glob(
            &Path::new(""),
            &segments,
            0,
            request.maximum_entries,
            &mut entries,
            &mut seen,
            &mut scanned,
            cancellation,
        )?;
        entries.sort_by(|left, right| {
            right
                .modified_epoch_millis
                .cmp(&left.modified_epoch_millis)
                .then_with(|| left.path.cmp(&right.path))
        });
        entries.truncate(request.maximum_entries);
        Ok(FileListResultV1 {
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::List,
                relative_path: PathBuf::new(),
                before_content_hash: content_hash(&[]),
                after_content_hash: content_hash(&[]),
                bytes_observed_or_written: usize::try_from(scanned).unwrap_or(usize::MAX),
                write_committed: false,
            },
            entries,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_glob(
        &self,
        directory_path: &Path,
        segments: &[GlobSegment],
        segment_index: usize,
        maximum_entries: usize,
        entries: &mut Vec<FileListEntryV1>,
        seen: &mut BTreeSet<String>,
        scanned: &mut u64,
        cancellation: &CancellationToken,
    ) -> Result<(), FileToolError> {
        if entries.len() >= maximum_entries {
            return Ok(());
        }
        check_cancelled(cancellation)?;
        let Some(segment) = segments.get(segment_index) else {
            return Ok(());
        };
        let last = segment_index + 1 == segments.len();
        if segment.recursive && !last {
            // `**` consumes zero directories first, then the same segment is
            // retried in each child directory to consume one or more.
            self.collect_glob(
                directory_path,
                segments,
                segment_index + 1,
                maximum_entries,
                entries,
                seen,
                scanned,
                cancellation,
            )?;
            if entries.len() >= maximum_entries {
                return Ok(());
            }
        }
        let directory = self.open_glob_directory(directory_path)?;
        for entry in directory.entries().map_err(FileToolError::Io)? {
            if *scanned >= MAX_LIST_SCANNED_ENTRIES {
                return Err(FileToolError::ListScanLimit);
            }
            let entry = entry.map_err(FileToolError::Io)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type().map_err(FileToolError::Io)?;
            let matches = segment.recursive || segment.matches(&name);
            if !matches {
                continue;
            }
            let relative = directory_path.join(name.as_ref());
            *scanned = scanned.saturating_add(1);
            if file_type.is_dir() {
                if segment.recursive || !last {
                    self.collect_glob(
                        &relative,
                        segments,
                        if segment.recursive {
                            segment_index
                        } else {
                            segment_index + 1
                        },
                        maximum_entries,
                        entries,
                        seen,
                        scanned,
                        cancellation,
                    )?;
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if !last {
                continue;
            }
            let metadata = entry.metadata().map_err(FileToolError::Io)?;
            let path = relative.to_string_lossy().replace('\\', "/");
            if !seen.insert(path.clone()) {
                continue;
            }
            entries.push(FileListEntryV1 {
                path,
                size_bytes: metadata.len(),
                modified_epoch_millis: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.into_std().duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |duration| {
                        duration.as_millis().try_into().unwrap_or(u64::MAX)
                    }),
            });
            if entries.len() >= maximum_entries {
                return Ok(());
            }
        }
        Ok(())
    }

    fn open_glob_directory(&self, directory_path: &Path) -> Result<Arc<Dir>, FileToolError> {
        if directory_path.as_os_str().is_empty() {
            return Ok(self.directory.clone());
        }
        self.reject_symlinks(directory_path, false)?;
        self.directory
            .open_dir(directory_path)
            .map(Arc::new)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => FileToolError::NoMatch(format!(
                    "path {} does not exist",
                    directory_path.display()
                )),
                _ => error.into(),
            })
    }

    /// Regex search across text files beneath the root with line context.
    pub fn grep_v1(
        &self,
        request: &FileGrepRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileGrepResultV1, FileToolError> {
        if request.pattern.is_empty()
            || request.pattern.len() > 16 * 1024
            || request.maximum_matches == 0
            || request.maximum_matches > MAX_GREP_MATCHES
            || request.maximum_files == 0
            || request.maximum_files > MAX_GREP_FILES
            || request.maximum_file_bytes == 0
            || request.maximum_file_bytes > MAX_FILE_BYTES
        {
            return Err(FileToolError::InvalidGrep);
        }
        let pattern = Regex::new(&request.pattern).map_err(|_| FileToolError::InvalidGrep)?;
        self.revalidate_root()?;
        let mut matches = Vec::new();
        let mut files_scanned = 0_usize;
        self.collect_regex(
            &Path::new(""),
            &pattern,
            request,
            &mut matches,
            &mut files_scanned,
            cancellation,
        )?;
        Ok(FileGrepResultV1 {
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Grep,
                relative_path: PathBuf::new(),
                before_content_hash: content_hash(&[]),
                after_content_hash: content_hash(&[]),
                bytes_observed_or_written: files_scanned,
                write_committed: false,
            },
            matches,
            files_scanned,
        })
    }

    fn collect_regex(
        &self,
        directory_path: &Path,
        pattern: &Regex,
        request: &FileGrepRequestV1,
        matches: &mut Vec<FileGrepMatchV1>,
        files_scanned: &mut usize,
        cancellation: &CancellationToken,
    ) -> Result<(), FileToolError> {
        if matches.len() >= request.maximum_matches || *files_scanned >= request.maximum_files {
            return Ok(());
        }
        check_cancelled(cancellation)?;
        let directory: Arc<Dir> = if directory_path.as_os_str().is_empty() {
            self.directory.clone()
        } else {
            self.reject_symlinks(directory_path, false)?;
            match self.directory.open_dir(directory_path) {
                Ok(directory) => Arc::new(directory),
                Err(_) => return Ok(()),
            }
        };
        for entry in directory.entries().map_err(FileToolError::Io)? {
            let entry = entry.map_err(FileToolError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = directory_path.join(&name);
            let file_type = entry.file_type().map_err(FileToolError::Io)?;
            if file_type.is_dir() {
                self.collect_regex(
                    &relative,
                    pattern,
                    request,
                    matches,
                    files_scanned,
                    cancellation,
                )?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if *files_scanned >= request.maximum_files {
                return Ok(());
            }
            *files_scanned = files_scanned.saturating_add(1);
            let Ok(read) = self.read_v1(
                &FileReadRequestV1 {
                    path: relative.clone(),
                    maximum_bytes: request.maximum_file_bytes,
                },
                cancellation,
            ) else {
                continue;
            };
            let Ok(text) = String::from_utf8(read.bytes) else {
                continue;
            };
            let path_text = relative.to_string_lossy().replace('\\', "/");
            for (line_index, line) in text.lines().enumerate() {
                for found in pattern.find_iter(line) {
                    matches.push(FileGrepMatchV1 {
                        path: path_text.clone(),
                        line: line_index as u64 + 1,
                        offset: found.start(),
                        line_text: truncate_line(line, 256),
                    });
                    if matches.len() >= request.maximum_matches {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Creates or replaces one file with the exact content, optionally gated
    /// on the current content hash. Symlinks and reparse aliases are denied.
    pub fn write_v1(
        &self,
        request: &FileWriteRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileWriteResultV1, FileToolError> {
        if !self.authority.allow_write {
            return Err(FileToolError::WriteDenied);
        }
        if request.content.len() > MAX_FILE_BYTES {
            return Err(FileToolError::TooLarge);
        }
        check_cancelled(cancellation)?;
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| FileToolError::Poisoned)?;
        self.revalidate_root()?;
        let path = validate_relative(&request.path)?;
        self.reject_symlinks(path, false)?;
        let existing = self.directory.open(path).ok().map(|mut file| {
            let mut body = Vec::new();
            let _ = file.read_to_end(&mut body);
            body
        });
        if let (Some(expected), Some(current)) = (&request.expected_content_hash, &existing)
            && content_hash(current) != *expected
        {
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
            staged.write_all(&request.content)?;
            staged.sync_all()?;
            check_cancelled(cancellation)?;
            if let (Some(expected), Some(current)) = (&request.expected_content_hash, &existing)
                && content_hash(current) != *expected
            {
                return Err(FileToolError::Conflict);
            }
            if existing.is_some() {
                let _ = self.directory.remove_file(path);
            }
            self.directory.rename(&temporary, &self.directory, path)?;
            // Directory-entry durability: fsync the parent on Unix. Windows
            // exposes no directory fsync through cap-std; the rename itself
            // is committed by the OS before it returns.
            #[cfg(unix)]
            {
                let sync_parent = if parent.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    parent
                };
                self.directory.open(sync_parent)?.sync_all()?;
            }
            Ok(())
        })();
        if staged_result.is_err() {
            let _ = self.directory.remove_file(&temporary);
        }
        staged_result?;
        Ok(FileWriteResultV1 {
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Write,
                relative_path: path.to_path_buf(),
                before_content_hash: existing
                    .as_deref()
                    .map(content_hash)
                    .unwrap_or_else(|| content_hash(&[])),
                after_content_hash: content_hash(&request.content),
                bytes_observed_or_written: request.content.len(),
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

/// File-system object identity that survives a path-level directory swap:
/// device/inode on Unix, volume serial/file index on Windows.
fn root_identity(path: &Path) -> Result<RootIdentity, std::io::Error> {
    Ok(RootIdentity {
        canonical_path: fs::canonicalize(path)?,
        handle: Arc::new(same_file::Handle::from_path(path)?),
    })
}

fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GlobSegment {
    alternatives: Vec<Vec<GlobPart>>,
    recursive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum GlobPart {
    Literal(String),
    Any,
    AnySequence,
}

impl GlobSegment {
    fn matches(&self, name: &str) -> bool {
        self.alternatives
            .iter()
            .any(|parts| match_segments(parts, name))
    }
}

fn match_segments(parts: &[GlobPart], name: &str) -> bool {
    match parts.split_first() {
        None => name.is_empty(),
        Some((GlobPart::AnySequence, rest)) => {
            for index in 0..=name.len() {
                if name.is_char_boundary(index) && match_segments(rest, &name[index..]) {
                    return true;
                }
            }
            false
        }
        Some((GlobPart::Any, rest)) => name.chars().next().is_some_and(|_| {
            match_segments(rest, &name[name.chars().next().unwrap().len_utf8()..])
        }),
        Some((GlobPart::Literal(literal), rest)) => name
            .strip_prefix(literal.as_str())
            .is_some_and(|remaining| match_segments(rest, remaining)),
    }
}

/// Splits a bounded glob pattern into slash-separated segments. `**` marks a
/// recursive segment.
fn glob_segments(pattern: &str) -> Result<Vec<GlobSegment>, FileToolError> {
    let mut segments = Vec::new();
    for raw in pattern.split('/') {
        if raw.is_empty() {
            continue;
        }
        let recursive = raw == "**";
        let alternatives = expand_brace_alternatives(raw)?
            .into_iter()
            .map(|alternative| glob_parts(&alternative))
            .collect::<Result<Vec<_>, _>>()?;
        segments.push(GlobSegment {
            alternatives,
            recursive,
        });
    }
    if segments.is_empty() {
        return Err(FileToolError::InvalidList);
    }
    Ok(segments)
}

fn glob_parts(raw: &str) -> Result<Vec<GlobPart>, FileToolError> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    for character in raw.chars() {
        match character {
            '*' => {
                if !literal.is_empty() {
                    parts.push(GlobPart::Literal(std::mem::take(&mut literal)));
                }
                match parts.last() {
                    Some(GlobPart::AnySequence) => {}
                    _ => parts.push(GlobPart::AnySequence),
                }
            }
            '?' => {
                if !literal.is_empty() {
                    parts.push(GlobPart::Literal(std::mem::take(&mut literal)));
                }
                parts.push(GlobPart::Any);
            }
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        parts.push(GlobPart::Literal(literal));
    }
    if parts.is_empty() {
        return Err(FileToolError::InvalidList);
    }
    Ok(parts)
}

fn expand_brace_alternatives(raw: &str) -> Result<Vec<String>, FileToolError> {
    let Some(open) = raw.find('{') else {
        return if raw.contains('}') {
            Err(FileToolError::InvalidList)
        } else {
            Ok(vec![raw.to_owned()])
        };
    };
    let close = raw[open + 1..]
        .find('}')
        .map(|index| open + 1 + index)
        .ok_or(FileToolError::InvalidList)?;
    let choices = raw[open + 1..close].split(',').collect::<Vec<_>>();
    if choices.len() < 2
        || choices.iter().any(|choice| choice.is_empty())
        || raw[open + 1..close]
            .chars()
            .any(|character| matches!(character, '{' | '}'))
    {
        return Err(FileToolError::InvalidList);
    }
    let suffixes = expand_brace_alternatives(&raw[close + 1..])?;
    let mut expanded = Vec::new();
    for choice in choices {
        for suffix in &suffixes {
            if expanded.len() >= 32 {
                return Err(FileToolError::InvalidList);
            }
            expanded.push(format!("{}{}{}", &raw[..open], choice, suffix));
        }
    }
    Ok(expanded)
}

fn truncate_line(line: &str, maximum_chars: usize) -> String {
    if line.chars().count() <= maximum_chars {
        line.to_owned()
    } else {
        let mut truncated: String = line.chars().take(maximum_chars).collect();
        truncated.push('…');
        truncated
    }
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
    #[error("glob pattern is invalid")]
    InvalidList,
    #[error("glob traversal exceeded its bounded scan limit")]
    ListScanLimit,
    #[error("regex pattern or grep bounds are invalid")]
    InvalidGrep,
    #[error("glob matched no files: {0}")]
    NoMatch(String),
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
