//! Expected-head portable commits with prepared immutable objects and verified publication.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArtifactStore, BranchRef, CanonicalCodec, ExportPolicy, OmissionFact, PortableCheckpoint,
    PortableCommitContextV1, PortableError, PortableEvent, PortablePaths, PortableSegment,
    ProjectReference, canonical_json, validate_checkpoint_record, validate_context,
};

static REF_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A complete proposed portable transition. The caller must link the verified
/// receipt to its noncanonical runtime journal before acknowledging it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCommit {
    pub branch_id: String,
    pub expected_generation: u64,
    pub commit_id: String,
    pub context: Option<PortableCommitContextV1>,
    pub events: Vec<PortableEvent>,
    pub checkpoint: Option<PortableCheckpoint>,
}

/// Immutable preparation identity. Publishing this value never reserializes
/// caller input and therefore cannot drift between retries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedCommit {
    pub branch_id: String,
    pub commit_id: String,
    pub expected_generation: u64,
    pub previous_head_hash: Option<String>,
    pub segment_hash: String,
    pub checkpoint_hash: Option<String>,
    pub next_ordinal: u64,
    pub request_hash: String,
    pub claim_hash: String,
}

/// Freshly reread facts proving the branch pointer published this transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub branch_id: String,
    pub commit_id: String,
    pub previous_head_hash: Option<String>,
    pub head_segment_hash: String,
    pub checkpoint_hash: Option<String>,
    pub generation: u64,
    pub request_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFaultPoint {
    AfterPrepare,
    BeforeHeadPublication,
    AfterHeadPublication,
}

/// Portable object and branch-head coordinator.
#[derive(Clone, Debug)]
pub struct PortableRepository {
    paths: PortablePaths,
    codec: CanonicalCodec,
    policy: ExportPolicy,
    fault_once: Arc<Mutex<Option<CommitFaultPoint>>>,
}

impl PortableRepository {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            codec: CanonicalCodec,
            policy: ExportPolicy,
            fault_once: Arc::new(Mutex::new(None)),
        }
    }

    /// Hermetic crash-window injection used by QA. The next matching point
    /// fails exactly once.
    pub fn inject_fault_once(&self, point: CommitFaultPoint) -> Result<(), CommitError> {
        *self.fault_once.lock().map_err(|_| CommitError::Poisoned)? = Some(point);
        Ok(())
    }

    pub fn prepare_publish_verify(
        &self,
        commit: &PortableCommit,
    ) -> Result<CommitReceipt, CommitError> {
        let prepared = self.prepare(commit)?;
        let _ = self.publish(&prepared)?;
        self.verify(&prepared)
    }

    /// Writes and rereads immutable segment/checkpoint/claim objects without
    /// mutating the branch head.
    pub fn prepare(&self, commit: &PortableCommit) -> Result<PreparedCommit, CommitError> {
        validate_id(&commit.branch_id)?;
        validate_id(&commit.commit_id)?;
        if commit.events.is_empty() {
            return Err(CommitError::EmptyCommit);
        }
        let (sanitized, omissions) = self.sanitize_events(commit)?;
        let mut context = commit.context.clone();
        if !omissions.is_empty() {
            let context = context.as_mut().ok_or(CommitError::ExportRejected)?;
            context.provenance.omissions.extend(omissions);
            context
                .provenance
                .omissions
                .sort_by(|left, right| left.pointer.cmp(&right.pointer));
            context.provenance.omissions.dedup_by(|left, right| {
                left.pointer == right.pointer && left.reason == right.reason
            });
        }
        if let Some(context) = &context {
            validate_context(context)?;
            if let Some(snapshot) = &context.frozen_snapshot {
                let scrubbed = self
                    .policy
                    .scrub(&snapshot.portable_snapshot)
                    .map_err(|_| CommitError::ExportRejected)?;
                if scrubbed.value != snapshot.portable_snapshot || !scrubbed.omissions.is_empty() {
                    return Err(CommitError::ExportRejected);
                }
            }
        }
        let request_hash = hash_request(commit, &sanitized, context.as_ref())?;
        let current = self.read_branch(&commit.branch_id)?;
        self.validate_current_lineage(&current)?;

        // Exact retries after a successfully published head are reconstructible
        // from immutable data and the authenticated request hash in the ref.
        if current.commit_id.as_deref() == Some(&commit.commit_id) {
            if current.commit_request_hash.as_deref() != Some(&request_hash) {
                return Err(CommitError::CommitIdentityConflict);
            }
            let segment_hash = current
                .head_segment_hash
                .clone()
                .ok_or(CommitError::VerificationFailed)?;
            let segment = self
                .codec
                .decode_segment(&self.paths.read("segments", &segment_hash)?)?;
            let prepared = PreparedCommit {
                branch_id: commit.branch_id.clone(),
                commit_id: commit.commit_id.clone(),
                expected_generation: commit.expected_generation,
                previous_head_hash: segment.parent_segment_hash,
                segment_hash,
                checkpoint_hash: current.checkpoint_hash,
                next_ordinal: current.next_ordinal,
                request_hash,
                claim_hash: String::new(),
            };
            let prepared = self.publish_claim(prepared)?;
            self.fail_if(CommitFaultPoint::AfterPrepare)?;
            return Ok(prepared);
        }
        if current.generation != commit.expected_generation {
            return Err(CommitError::HeadConflict {
                expected: commit.expected_generation,
                actual: current.generation,
            });
        }
        validate_event_lineage(commit, current.next_ordinal)?;
        let segment = PortableSegment {
            parent_segment_hash: current.head_segment_hash.clone(),
            base_checkpoint_hash: current.checkpoint_hash.clone(),
            first_ordinal: current.next_ordinal,
            context,
            events: sanitized,
        };
        let segment_bytes = self.codec.encode_segment(&segment)?;
        let segment_hash = self.paths.publish("segments", &segment_bytes)?;
        let checkpoint_hash = match &commit.checkpoint {
            Some(checkpoint) => {
                validate_checkpoint(checkpoint, segment.events.last())?;
                Some(
                    self.paths
                        .publish("checkpoints", &self.codec.encode(checkpoint)?)?,
                )
            }
            None => current.checkpoint_hash.clone(),
        };
        let next_ordinal = current
            .next_ordinal
            .checked_add(u64::try_from(segment.events.len()).expect("bounded"))
            .ok_or(CommitError::OrdinalOverflow)?;
        let prepared = self.publish_claim(PreparedCommit {
            branch_id: commit.branch_id.clone(),
            commit_id: commit.commit_id.clone(),
            expected_generation: commit.expected_generation,
            previous_head_hash: current.head_segment_hash,
            segment_hash,
            checkpoint_hash,
            next_ordinal,
            request_hash,
            claim_hash: String::new(),
        })?;
        self.fail_if(CommitFaultPoint::AfterPrepare)?;
        Ok(prepared)
    }

    fn publish_claim(&self, mut prepared: PreparedCommit) -> Result<PreparedCommit, CommitError> {
        prepared.claim_hash.clear();
        let claim_hash = self
            .paths
            .publish("claims", &self.codec.encode(&prepared)?)?;
        prepared.claim_hash = claim_hash;
        Ok(prepared)
    }

    /// Performs the sole expected-generation mutable head transition.
    pub fn publish(&self, prepared: &PreparedCommit) -> Result<CommitReceipt, CommitError> {
        self.validate_prepared(prepared)?;
        let lock = self.paths.open_lock()?;
        lock.lock_exclusive()?;
        let result = self.publish_locked(prepared);
        let _ = lock.unlock();
        let receipt = result?;
        self.fail_if(CommitFaultPoint::AfterHeadPublication)?;
        Ok(receipt)
    }

    fn publish_locked(&self, prepared: &PreparedCommit) -> Result<CommitReceipt, CommitError> {
        let current = self.read_branch(&prepared.branch_id)?;
        if current.commit_id.as_deref() == Some(&prepared.commit_id) {
            if current.commit_request_hash.as_deref() != Some(&prepared.request_hash)
                || current.head_segment_hash.as_deref() != Some(&prepared.segment_hash)
                || current.checkpoint_hash != prepared.checkpoint_hash
            {
                return Err(CommitError::CommitIdentityConflict);
            }
            return self.receipt_from_ref(&current, prepared.previous_head_hash.clone());
        }
        if current.generation != prepared.expected_generation
            || current.head_segment_hash != prepared.previous_head_hash
        {
            return Err(CommitError::HeadConflict {
                expected: prepared.expected_generation,
                actual: current.generation,
            });
        }
        self.fail_if(CommitFaultPoint::BeforeHeadPublication)?;
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(CommitError::GenerationOverflow)?;
        let next = BranchRef {
            branch_id: prepared.branch_id.clone(),
            head_segment_hash: Some(prepared.segment_hash.clone()),
            checkpoint_hash: prepared.checkpoint_hash.clone(),
            next_ordinal: prepared.next_ordinal,
            generation,
            commit_id: Some(prepared.commit_id.clone()),
            commit_request_hash: Some(prepared.request_hash.clone()),
        };
        self.write_ref(&next)?;
        self.receipt_from_ref(&next, prepared.previous_head_hash.clone())
    }

    /// Rereads the pointer and every referenced immutable object. It never
    /// republishes, so callers can safely use it after an uncertain response.
    pub fn verify(&self, prepared: &PreparedCommit) -> Result<CommitReceipt, CommitError> {
        self.validate_prepared(prepared)?;
        let current = self.read_branch(&prepared.branch_id)?;
        if current.generation != prepared.expected_generation.saturating_add(1)
            || current.commit_id.as_deref() != Some(&prepared.commit_id)
            || current.commit_request_hash.as_deref() != Some(&prepared.request_hash)
            || current.head_segment_hash.as_deref() != Some(&prepared.segment_hash)
            || current.checkpoint_hash != prepared.checkpoint_hash
            || current.next_ordinal != prepared.next_ordinal
        {
            return Err(CommitError::VerificationFailed);
        }
        let segment = self
            .codec
            .decode_segment(&self.paths.read("segments", &prepared.segment_hash)?)?;
        if segment.parent_segment_hash != prepared.previous_head_hash {
            return Err(CommitError::VerificationFailed);
        }
        if let Some(hash) = &prepared.checkpoint_hash {
            self.read_checkpoint_validated(hash)?;
        }
        self.receipt_from_ref(&current, prepared.previous_head_hash.clone())
    }

    pub fn read_branch(&self, branch_id: &str) -> Result<BranchRef, CommitError> {
        validate_id(branch_id)?;
        let reference = ref_reference(branch_id)?;
        match self.paths.read_relative(&reference, 64 * 1024) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)?;
                if canonical_json(&value)? != bytes {
                    return Err(CommitError::VerificationFailed);
                }
                let branch: BranchRef = serde_json::from_value(value)?;
                validate_branch_ref(&branch, branch_id)?;
                Ok(branch)
            }
            Err(PortableError::Workspace(crate::WorkspaceError::Io(error)))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(BranchRef {
                    branch_id: branch_id.to_owned(),
                    head_segment_hash: None,
                    checkpoint_hash: None,
                    next_ordinal: 0,
                    generation: 0,
                    commit_id: None,
                    commit_request_hash: None,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    #[must_use]
    pub fn paths(&self) -> &PortablePaths {
        &self.paths
    }

    fn write_ref(&self, branch: &BranchRef) -> Result<(), CommitError> {
        let target = ref_reference(&branch.branch_id)?;
        let parent = target.path().parent().ok_or(CommitError::InvalidBranch)?;
        self.paths.create_dir_all(parent)?;
        let temporary = ProjectReference::parse(format!(
            "{}/.{}.{}.{}.tmp",
            parent.to_string_lossy(),
            branch.branch_id,
            std::process::id(),
            REF_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))?;
        Ok(self.paths.replace_relative(
            &temporary,
            &target,
            &canonical_json(&serde_json::to_value(branch)?)?,
        )?)
    }

    fn sanitize_events(
        &self,
        commit: &PortableCommit,
    ) -> Result<(Vec<PortableEvent>, Vec<OmissionFact>), CommitError> {
        let mut omissions = Vec::new();
        let events = commit
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let scrubbed = self
                    .policy
                    .scrub(&event.payload)
                    .map_err(|_| CommitError::ExportRejected)?;
                omissions.extend(scrubbed.omissions.into_iter().map(|fact| OmissionFact {
                    pointer: format!("/events/{index}/payload{}", fact.pointer),
                    reason: fact.reason,
                }));
                let mut event = event.clone();
                event.payload = scrubbed.value;
                Ok(event)
            })
            .collect::<Result<Vec<_>, CommitError>>()?;
        Ok((events, omissions))
    }

    fn validate_current_lineage(&self, branch: &BranchRef) -> Result<(), CommitError> {
        let Some(mut cursor) = branch.head_segment_hash.clone() else {
            if branch.generation == 0
                && branch.next_ordinal == 0
                && branch.checkpoint_hash.is_none()
            {
                return Ok(());
            }
            return Err(CommitError::VerificationFailed);
        };
        let mut expected_end = branch.next_ordinal;
        let mut seen = BTreeSet::new();
        for _ in 0..16_384 {
            if !seen.insert(cursor.clone()) {
                return Err(CommitError::VerificationFailed);
            }
            let segment = self
                .codec
                .decode_segment(&self.paths.read("segments", &cursor)?)?;
            let end = segment
                .first_ordinal
                .checked_add(u64::try_from(segment.events.len()).expect("bounded"))
                .ok_or(CommitError::OrdinalOverflow)?;
            if end != expected_end {
                return Err(CommitError::VerificationFailed);
            }
            if let Some(checkpoint) = &segment.base_checkpoint_hash {
                self.read_checkpoint_validated(checkpoint)?;
            }
            if let Some(context) = &segment.context {
                let artifacts = ArtifactStore::new(self.paths.clone());
                for descriptor in &context.provenance.artifact_metadata {
                    artifacts
                        .read_verified(descriptor)
                        .map_err(|_| CommitError::VerificationFailed)?;
                }
            }
            expected_end = segment.first_ordinal;
            match segment.parent_segment_hash {
                Some(parent) => cursor = parent,
                None if expected_end == 0 => break,
                None => return Err(CommitError::VerificationFailed),
            }
        }
        if seen.len() >= 16_384 {
            return Err(CommitError::VerificationFailed);
        }
        if let Some(checkpoint) = &branch.checkpoint_hash {
            self.read_checkpoint_validated(checkpoint)?;
        }
        Ok(())
    }

    fn read_checkpoint_validated(&self, identity: &str) -> Result<(), CommitError> {
        let checkpoint: PortableCheckpoint = self
            .codec
            .decode(&self.paths.read("checkpoints", identity)?)?;
        validate_checkpoint_record(&checkpoint)?;
        Ok(())
    }

    fn validate_prepared(&self, prepared: &PreparedCommit) -> Result<(), CommitError> {
        validate_id(&prepared.branch_id)?;
        validate_id(&prepared.commit_id)?;
        if prepared.claim_hash.is_empty() {
            return Err(CommitError::InvalidPreparedClaim);
        }
        let mut expected = prepared.clone();
        expected.claim_hash.clear();
        let bytes = self.paths.read("claims", &prepared.claim_hash)?;
        let stored: PreparedCommit = self.codec.decode(&bytes)?;
        if stored != expected {
            return Err(CommitError::InvalidPreparedClaim);
        }
        let _ = self
            .codec
            .decode_segment(&self.paths.read("segments", &prepared.segment_hash)?)?;
        if let Some(checkpoint) = &prepared.checkpoint_hash {
            self.read_checkpoint_validated(checkpoint)?;
        }
        Ok(())
    }

    fn receipt_from_ref(
        &self,
        branch: &BranchRef,
        previous_head_hash: Option<String>,
    ) -> Result<CommitReceipt, CommitError> {
        Ok(CommitReceipt {
            branch_id: branch.branch_id.clone(),
            commit_id: branch
                .commit_id
                .clone()
                .ok_or(CommitError::VerificationFailed)?,
            previous_head_hash,
            head_segment_hash: branch
                .head_segment_hash
                .clone()
                .ok_or(CommitError::VerificationFailed)?,
            checkpoint_hash: branch.checkpoint_hash.clone(),
            generation: branch.generation,
            request_hash: branch
                .commit_request_hash
                .clone()
                .ok_or(CommitError::VerificationFailed)?,
        })
    }

    fn fail_if(&self, point: CommitFaultPoint) -> Result<(), CommitError> {
        let mut fault = self.fault_once.lock().map_err(|_| CommitError::Poisoned)?;
        if fault.as_ref() == Some(&point) {
            *fault = None;
            if point == CommitFaultPoint::AfterHeadPublication {
                Err(CommitError::PublicationUncertain)
            } else {
                Err(CommitError::InjectedFault(point))
            }
        } else {
            Ok(())
        }
    }
}

fn hash_request(
    commit: &PortableCommit,
    sanitized: &[PortableEvent],
    context: Option<&PortableCommitContextV1>,
) -> Result<String, CommitError> {
    let value = serde_json::json!({
        "branchId": commit.branch_id,
        "expectedGeneration": commit.expected_generation,
        "commitId": commit.commit_id,
        "context": context,
        "events": sanitized,
        "checkpoint": commit.checkpoint,
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(&value)?)
    ))
}

fn validate_event_lineage(commit: &PortableCommit, first_ordinal: u64) -> Result<(), CommitError> {
    for (index, event) in commit.events.iter().enumerate() {
        let expected = first_ordinal
            .checked_add(u64::try_from(index).expect("bounded"))
            .ok_or(CommitError::OrdinalOverflow)?;
        if event.branch_id != commit.branch_id || event.ordinal != expected {
            return Err(CommitError::EventLineage);
        }
        validate_id(&event.event_id)?;
        validate_id(&event.chat_id)?;
    }
    if commit
        .events
        .windows(2)
        .any(|pair| pair[0].chat_id != pair[1].chat_id)
    {
        return Err(CommitError::EventLineage);
    }
    Ok(())
}

fn validate_checkpoint(
    checkpoint: &PortableCheckpoint,
    last_event: Option<&PortableEvent>,
) -> Result<(), CommitError> {
    if validate_checkpoint_record(checkpoint).is_err()
        || checkpoint.last_event_id.as_deref() != last_event.map(|event| event.event_id.as_str())
    {
        Err(CommitError::CheckpointMismatch)
    } else {
        Ok(())
    }
}

fn validate_branch_ref(branch: &BranchRef, expected_id: &str) -> Result<(), CommitError> {
    if branch.branch_id != expected_id
        || branch.generation == 0
            && (branch.head_segment_hash.is_some()
                || branch.checkpoint_hash.is_some()
                || branch.next_ordinal != 0
                || branch.commit_id.is_some()
                || branch.commit_request_hash.is_some())
        || branch.generation > 0
            && (branch.head_segment_hash.is_none()
                || branch.next_ordinal == 0
                || branch.commit_id.is_none()
                || branch.commit_request_hash.is_none())
        || branch
            .head_segment_hash
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
        || branch
            .checkpoint_hash
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
        || branch
            .commit_request_hash
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
    {
        Err(CommitError::VerificationFailed)
    } else {
        Ok(())
    }
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn ref_reference(branch_id: &str) -> Result<ProjectReference, CommitError> {
    validate_id(branch_id)?;
    Ok(ProjectReference::parse(format!(
        ".aworkit/portable/refs/{branch_id}.json"
    ))?)
}

fn validate_id(value: &str) -> Result<(), CommitError> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err(CommitError::InvalidBranch)
    }
}

#[derive(Debug, Error)]
pub enum CommitError {
    #[error("branch or commit identifier is invalid")]
    InvalidBranch,
    #[error("portable commits require at least one semantic event")]
    EmptyCommit,
    #[error("expected branch generation {expected}, found {actual}")]
    HeadConflict { expected: u64, actual: u64 },
    #[error("portable commit ID was reused with different content")]
    CommitIdentityConflict,
    #[error("portable events do not match branch or contiguous ordinal lineage")]
    EventLineage,
    #[error("portable checkpoint does not match the prepared transition")]
    CheckpointMismatch,
    #[error("portable ordinal overflow")]
    OrdinalOverflow,
    #[error("portable generation overflow")]
    GenerationOverflow,
    #[error("portable export policy rejected the commit")]
    ExportRejected,
    #[error("portable prepared claim is missing or was altered")]
    InvalidPreparedClaim,
    #[error("portable head reread did not verify")]
    VerificationFailed,
    #[error("portable publication may have succeeded; verify before retrying")]
    PublicationUncertain,
    #[error("portable commit fault injected at {0:?}")]
    InjectedFault(CommitFaultPoint),
    #[error("portable commit state is unavailable")]
    Poisoned,
    #[error(transparent)]
    Portable(#[from] PortableError),
    #[error(transparent)]
    Workspace(#[from] crate::WorkspaceError),
    #[error(transparent)]
    Codec(#[from] crate::CodecError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
