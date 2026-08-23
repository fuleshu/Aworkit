//! Stable executable and live-process identity observations.

use std::{
    collections::hash_map::DefaultHasher,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
};

use same_file::Handle as SameFileHandle;
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_EXECUTABLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Content and object identity for one exact executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableIdentityV1 {
    pub canonical_path: PathBuf,
    pub content_hash: String,
    pub byte_size: u64,
    pub object_identity_hash: String,
}

impl ExecutableIdentityV1 {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let supplied = path.as_ref();
        let link_metadata = fs::symlink_metadata(supplied)?;
        if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
            return Err(IdentityError::ExecutableAliasOrKind);
        }
        if link_metadata.len() == 0 || link_metadata.len() > MAX_EXECUTABLE_BYTES {
            return Err(IdentityError::ExecutableBound);
        }
        let canonical_path = fs::canonicalize(supplied)?;
        let before = SameFileHandle::from_path(&canonical_path)?;
        let mut file = File::open(&canonical_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        let mut count = 0_u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .ok_or(IdentityError::ExecutableBound)?;
            if count > MAX_EXECUTABLE_BYTES {
                return Err(IdentityError::ExecutableBound);
            }
            hasher.update(&buffer[..read]);
        }
        let after = SameFileHandle::from_path(&canonical_path)?;
        if before != after || fs::metadata(&canonical_path)?.len() != count {
            return Err(IdentityError::ExecutableChanged);
        }
        let mut object_hasher = DefaultHasher::new();
        after.hash(&mut object_hasher);
        Ok(Self {
            canonical_path,
            content_hash: format!("sha256:{:x}", hasher.finalize()),
            byte_size: count,
            object_identity_hash: format!(
                "sha256:{:x}",
                Sha256::digest(format!("{}:{count}", object_hasher.finish()))
            ),
        })
    }
}

/// OS peer facts bound to the lifetime of an accepted local connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerProcessIdentityV1 {
    pub process_id: Option<u32>,
    pub effective_user_id: Option<u32>,
    pub executable: Option<ExecutableIdentityV1>,
    pub strong_executable_identity: bool,
}

/// Resolves a peer PID without trusting a caller-supplied executable path.
pub fn executable_for_process(process_id: u32) -> Result<ExecutableIdentityV1, IdentityError> {
    let system = sysinfo::System::new_all();
    let process = system
        .process(sysinfo::Pid::from_u32(process_id))
        .ok_or(IdentityError::ProcessExecutableUnavailable)?;
    let executable = process
        .exe()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(IdentityError::ProcessExecutableUnavailable)?;
    ExecutableIdentityV1::open(executable)
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("executable path is an alias or not a regular file")]
    ExecutableAliasOrKind,
    #[error("executable exceeds its bounded size")]
    ExecutableBound,
    #[error("executable object changed while it was hashed")]
    ExecutableChanged,
    #[error("the platform cannot prove the peer executable from a live process handle")]
    ProcessExecutableUnavailable,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
