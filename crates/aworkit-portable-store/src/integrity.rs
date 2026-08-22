//! Branch/object integrity inspection and guarded non-destructive repair decisions.

use std::collections::{BTreeMap, BTreeSet};

use fs2::FileExt;

use crate::{
    ArtifactStore, CanonicalCodec, CommitError, PortableCheckpoint, PortableError,
    PortableRepository, ProjectionFeed, ReachabilityScanV1, RetentionError,
    retention_plan_two_phase, validate_checkpoint_record,
};

const MAX_BRANCH_DEPTH: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntegrityIssueV1 {
    InvalidBranchRef { file: String, diagnostic: String },
    MissingOrCorruptSegment { branch_id: String, identity: String },
    ParentOrdinalMismatch { branch_id: String, identity: String },
    MissingCheckpoint { identity: String },
    MissingArtifact { identity: String },
    ParentCycleOrDepth { branch_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityReportV1 {
    pub branch_heads: BTreeMap<String, String>,
    pub reachable_segments: BTreeSet<String>,
    pub reachable_checkpoints: BTreeSet<String>,
    pub reachable_artifacts: BTreeSet<String>,
    pub issues: Vec<IntegrityIssueV1>,
    pub continuation_blocked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NonDestructiveRepairProposalV1 {
    CreatePointerOnlyRepair {
        branch_id: String,
        expected_generation: u64,
        expected_current_head: Option<String>,
        candidate_verified_head: String,
    },
    CreateRepairBranch {
        source_branch_id: String,
        new_branch_id: String,
        parent_verified_head: String,
    },
    QuarantineForInspection {
        branch_id: String,
        diagnostic: String,
    },
}

#[derive(Clone, Debug)]
pub struct PortableIntegrityEngine {
    repository: PortableRepository,
    codec: CanonicalCodec,
}

impl PortableIntegrityEngine {
    #[must_use]
    pub fn new(repository: PortableRepository) -> Self {
        Self {
            repository,
            codec: CanonicalCodec,
        }
    }

    pub fn inspect(&self) -> Result<IntegrityReportV1, PortableError> {
        let mut report = IntegrityReportV1 {
            branch_heads: BTreeMap::new(),
            reachable_segments: BTreeSet::new(),
            reachable_checkpoints: BTreeSet::new(),
            reachable_artifacts: BTreeSet::new(),
            issues: Vec::new(),
            continuation_blocked: false,
        };
        for file in self.repository.paths().list_ref_files()? {
            let Some(branch_id) = file.strip_suffix(".json") else {
                report.issues.push(IntegrityIssueV1::InvalidBranchRef {
                    file,
                    diagnostic: "unexpected file in refs directory".into(),
                });
                continue;
            };
            let branch = match self.repository.read_branch(branch_id) {
                Ok(branch) => branch,
                Err(error) => {
                    report.issues.push(IntegrityIssueV1::InvalidBranchRef {
                        file,
                        diagnostic: error.to_string(),
                    });
                    continue;
                }
            };
            let Some(head) = branch.head_segment_hash else {
                continue;
            };
            if let Some(checkpoint) = branch.checkpoint_hash {
                if self.read_checkpoint(&checkpoint).is_err() {
                    report.issues.push(IntegrityIssueV1::MissingCheckpoint {
                        identity: checkpoint.clone(),
                    });
                }
                report.reachable_checkpoints.insert(checkpoint);
            }
            report
                .branch_heads
                .insert(branch_id.to_owned(), head.clone());
            self.inspect_lineage(branch_id, &head, &mut report);
        }
        report.continuation_blocked = !report.issues.is_empty();
        Ok(report)
    }

    fn inspect_lineage(&self, branch_id: &str, head: &str, report: &mut IntegrityReportV1) {
        let mut seen = BTreeSet::new();
        let mut cursor = Some(head.to_owned());
        let mut child_first = None;
        while let Some(identity) = cursor {
            if seen.len() >= MAX_BRANCH_DEPTH || !seen.insert(identity.clone()) {
                report.issues.push(IntegrityIssueV1::ParentCycleOrDepth {
                    branch_id: branch_id.to_owned(),
                });
                return;
            }
            let bytes = match self.repository.paths().read("segments", &identity) {
                Ok(bytes) => bytes,
                Err(_) => {
                    report
                        .issues
                        .push(IntegrityIssueV1::MissingOrCorruptSegment {
                            branch_id: branch_id.to_owned(),
                            identity,
                        });
                    return;
                }
            };
            let segment = match self.codec.decode_segment(&bytes) {
                Ok(segment) => segment,
                Err(_) => {
                    report
                        .issues
                        .push(IntegrityIssueV1::MissingOrCorruptSegment {
                            branch_id: branch_id.to_owned(),
                            identity,
                        });
                    return;
                }
            };
            let end = match segment
                .first_ordinal
                .checked_add(u64::try_from(segment.events.len()).expect("bounded"))
            {
                Some(end) => end,
                None => {
                    report.issues.push(IntegrityIssueV1::ParentOrdinalMismatch {
                        branch_id: branch_id.to_owned(),
                        identity,
                    });
                    return;
                }
            };
            if child_first.is_some_and(|first| first != end) {
                report.issues.push(IntegrityIssueV1::ParentOrdinalMismatch {
                    branch_id: branch_id.to_owned(),
                    identity: identity.clone(),
                });
            }
            child_first = Some(segment.first_ordinal);
            report.reachable_segments.insert(identity);
            if let Some(checkpoint) = segment.base_checkpoint_hash {
                if self.read_checkpoint(&checkpoint).is_err() {
                    report.issues.push(IntegrityIssueV1::MissingCheckpoint {
                        identity: checkpoint.clone(),
                    });
                }
                report.reachable_checkpoints.insert(checkpoint);
            }
            if let Some(context) = segment.context {
                let artifacts = ArtifactStore::new(self.repository.paths().clone());
                for descriptor in context.provenance.artifact_metadata {
                    let artifact = descriptor.digest.clone();
                    if artifacts.read_verified(&descriptor).is_err() {
                        report.issues.push(IntegrityIssueV1::MissingArtifact {
                            identity: artifact.clone(),
                        });
                    }
                    report.reachable_artifacts.insert(artifact);
                }
            }
            cursor = segment.parent_segment_hash;
        }
    }

    fn read_checkpoint(&self, identity: &str) -> Result<PortableCheckpoint, PortableError> {
        let bytes = self.repository.paths().read("checkpoints", identity)?;
        let checkpoint = self
            .codec
            .decode(&bytes)
            .map_err(|_| PortableError::CorruptObject)?;
        validate_checkpoint_record(&checkpoint).map_err(|_| PortableError::CorruptObject)?;
        Ok(checkpoint)
    }

    /// Offers an exact repair without mutating an existing published tip. An
    /// empty pointer may be initialized in place; otherwise the only writable
    /// repair target is a fresh branch selected by the caller.
    pub fn propose_non_destructive_repair(
        &self,
        branch_id: &str,
        candidate_verified_head: &str,
        fresh_repair_branch_id: Option<&str>,
    ) -> Result<NonDestructiveRepairProposalV1, IntegrityError> {
        let lineage = ProjectionFeed::new(self.repository.paths().clone())
            .validate_lineage(candidate_verified_head);
        if !lineage.accepted {
            return Ok(NonDestructiveRepairProposalV1::QuarantineForInspection {
                branch_id: branch_id.to_owned(),
                diagnostic: lineage
                    .diagnostics
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| "candidate lineage did not verify".into()),
            });
        }
        let current = self.repository.read_branch(branch_id)?;
        if current.generation == 0 && current.head_segment_hash.is_none() {
            return Ok(NonDestructiveRepairProposalV1::CreatePointerOnlyRepair {
                branch_id: branch_id.to_owned(),
                expected_generation: 0,
                expected_current_head: None,
                candidate_verified_head: candidate_verified_head.to_owned(),
            });
        }
        let new_branch_id = fresh_repair_branch_id.ok_or(IntegrityError::FreshBranchRequired)?;
        if new_branch_id == branch_id {
            return Err(IntegrityError::FreshBranchRequired);
        }
        let repair = self.repository.read_branch(new_branch_id)?;
        if repair.generation != 0 || repair.head_segment_hash.is_some() {
            return Err(IntegrityError::RepairBranchExists);
        }
        Ok(NonDestructiveRepairProposalV1::CreateRepairBranch {
            source_branch_id: branch_id.to_owned(),
            new_branch_id: new_branch_id.to_owned(),
            parent_verified_head: candidate_verified_head.to_owned(),
        })
    }

    /// Deletes only objects absent in two scans and still absent after an
    /// exclusive current-head recheck. Published/reachable history is never rewritten.
    pub fn collect_verified_orphans(
        &self,
        namespace: &str,
        first: &ReachabilityScanV1,
        final_scan: &ReachabilityScanV1,
        first_unreachable_epoch_millis: &BTreeMap<String, u64>,
        grace_millis: u64,
    ) -> Result<Vec<String>, IntegrityError> {
        if !matches!(namespace, "segments" | "checkpoints" | "artifacts") {
            return Err(IntegrityError::UnsafeNamespace);
        }
        let lock = self.repository.paths().open_lock()?;
        lock.lock_exclusive()?;
        let result = (|| -> Result<Vec<String>, IntegrityError> {
            let current = self.inspect()?;
            if current.branch_heads != final_scan.branch_heads {
                return Err(IntegrityError::HeadChanged);
            }
            let current_reachable = match namespace {
                "segments" => current.reachable_segments,
                "checkpoints" => current.reachable_checkpoints,
                "artifacts" => current.reachable_artifacts,
                _ => unreachable!(),
            };
            if current_reachable != final_scan.reachable {
                return Err(IntegrityError::HeadChanged);
            }
            let all: BTreeSet<_> = self
                .repository
                .paths()
                .list_object_ids(namespace)?
                .into_iter()
                .collect();
            let plan = retention_plan_two_phase(
                &all,
                first,
                final_scan,
                first_unreachable_epoch_millis,
                grace_millis,
            )?;
            for identity in &plan.collectable {
                self.repository
                    .paths()
                    .remove_verified(namespace, identity)?;
            }
            Ok(plan.collectable)
        })();
        let _ = lock.unlock();
        result
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    #[error("only segment, checkpoint, and artifact orphans may be collected")]
    UnsafeNamespace,
    #[error("branch heads changed during guarded collection")]
    HeadChanged,
    #[error("repair of a published branch requires a distinct fresh branch")]
    FreshBranchRequired,
    #[error("the selected repair branch already has published history")]
    RepairBranchExists,
    #[error(transparent)]
    Retention(#[from] RetentionError),
    #[error(transparent)]
    Portable(#[from] PortableError),
    #[error(transparent)]
    Commit(#[from] CommitError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
