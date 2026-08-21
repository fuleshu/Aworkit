//! Expected-head portable commits with immutable objects and a verified receipt.

use crate::{
    BranchRef, CanonicalCodec, ExportPolicy, PortableCheckpoint, PortableError, PortableEvent,
    PortablePaths, PortableSegment, canonical_json,
};
use fs2::FileExt;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    path::Path,
};
use thiserror::Error;

/// A complete proposed portable transition. The caller must link the receipt
/// to its noncanonical runtime journal before acknowledgement or dispatch.
#[derive(Clone, Debug)]
pub struct PortableCommit {
    pub branch_id: String,
    pub expected_generation: u64,
    pub commit_id: String,
    pub events: Vec<PortableEvent>,
    pub checkpoint: Option<PortableCheckpoint>,
}
/// Freshly reread facts proving the branch pointer published this exact transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub branch_id: String,
    pub commit_id: String,
    pub head_segment_hash: String,
    pub checkpoint_hash: Option<String>,
    pub generation: u64,
}
/// Portable object and branch-head coordinator.
#[derive(Clone, Debug)]
pub struct PortableRepository {
    paths: PortablePaths,
    codec: CanonicalCodec,
    policy: ExportPolicy,
}
impl PortableRepository {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            codec: CanonicalCodec,
            policy: ExportPolicy,
        }
    }
    pub fn prepare_publish_verify(
        &self,
        commit: &PortableCommit,
    ) -> Result<CommitReceipt, CommitError> {
        let lock_path = self
            .paths
            .root()
            .path()
            .join(".aworkit/portable/.heads.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        let result = self.commit_locked(commit);
        let _ = lock.unlock();
        result
    }
    fn commit_locked(&self, commit: &PortableCommit) -> Result<CommitReceipt, CommitError> {
        let branch = checked_id(&commit.branch_id)?;
        let ref_path = self
            .paths
            .root()
            .path()
            .join(format!(".aworkit/portable/refs/{branch}.json"));
        let current = read_ref(&ref_path, &commit.branch_id)?;
        if current.generation != commit.expected_generation {
            return Err(CommitError::HeadConflict {
                expected: commit.expected_generation,
                actual: current.generation,
            });
        }
        let mut sanitized = Vec::with_capacity(commit.events.len());
        for event in &commit.events {
            let scrubbed = self
                .policy
                .scrub(&event.payload)
                .map_err(|_| CommitError::ExportRejected)?;
            let mut copy = event.clone();
            copy.payload = scrubbed.value;
            sanitized.push(copy);
        }
        let segment = PortableSegment {
            parent_segment_hash: current.head_segment_hash.clone(),
            base_checkpoint_hash: current.checkpoint_hash.clone(),
            first_ordinal: current.next_ordinal,
            events: sanitized,
        };
        let bytes = self.codec.encode_segment(&segment)?;
        let segment_hash = self.paths.publish("segments", &bytes)?;
        let checkpoint_hash = match &commit.checkpoint {
            Some(checkpoint) => Some(
                self.paths
                    .publish("checkpoints", &self.codec.encode(checkpoint)?)?,
            ),
            None => current.checkpoint_hash.clone(),
        };
        let next = BranchRef {
            branch_id: commit.branch_id.clone(),
            head_segment_hash: Some(segment_hash.clone()),
            checkpoint_hash: checkpoint_hash.clone(),
            next_ordinal: current.next_ordinal
                + u64::try_from(commit.events.len()).expect("bounded"),
            generation: current.generation + 1,
            commit_id: Some(commit.commit_id.clone()),
        };
        let bytes = canonical_json(&serde_json::to_value(&next)?)?;
        write_ref(&ref_path, &bytes)?;
        let verified = read_ref(&ref_path, &commit.branch_id)?;
        if verified != next {
            return Err(CommitError::VerificationFailed);
        }
        Ok(CommitReceipt {
            branch_id: commit.branch_id.clone(),
            commit_id: commit.commit_id.clone(),
            head_segment_hash: segment_hash,
            checkpoint_hash,
            generation: next.generation,
        })
    }
    pub fn read_branch(&self, branch_id: &str) -> Result<BranchRef, CommitError> {
        read_ref(
            &self.paths.root().path().join(format!(
                ".aworkit/portable/refs/{}.json",
                checked_id(branch_id)?
            )),
            branch_id,
        )
    }
    #[must_use]
    pub fn paths(&self) -> &PortablePaths {
        &self.paths
    }
}
fn checked_id(value: &str) -> Result<&str, CommitError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        Ok(value)
    } else {
        Err(CommitError::InvalidBranch)
    }
}
fn read_ref(path: &Path, branch_id: &str) -> Result<BranchRef, CommitError> {
    if !path.exists() {
        return Ok(BranchRef {
            branch_id: branch_id.to_owned(),
            head_segment_hash: None,
            checkpoint_hash: None,
            next_ordinal: 0,
            generation: 0,
            commit_id: None,
        });
    }
    let bytes = fs::read(path)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if canonical_json(&value)? != bytes {
        return Err(CommitError::VerificationFailed);
    }
    Ok(serde_json::from_value(value)?)
}
fn write_ref(path: &Path, bytes: &[u8]) -> Result<(), CommitError> {
    let parent = path.parent().ok_or(CommitError::InvalidBranch)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ref")
    ));
    {
        use std::io::Write;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[derive(Debug, Error)]
pub enum CommitError {
    #[error("branch identifier is invalid")]
    InvalidBranch,
    #[error("expected branch generation {expected}, found {actual}")]
    HeadConflict { expected: u64, actual: u64 },
    #[error("portable export policy rejected the commit")]
    ExportRejected,
    #[error("portable head reread did not verify")]
    VerificationFailed,
    #[error(transparent)]
    Portable(#[from] PortableError),
    #[error(transparent)]
    Codec(#[from] crate::CodecError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
