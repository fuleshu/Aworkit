//! Explicit parallel, for-each, and join frame coordination.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_BRANCHES_PER_FRAME: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStateV1 {
    Ready,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchRecordV1 {
    pub branch_id: StableId,
    pub ordinal: u32,
    pub state: BranchStateV1,
    pub result: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchFrameV1 {
    pub frame_id: StableId,
    pub parent_token_id: StableId,
    pub join_node_id: StableId,
    pub branches: Vec<BranchRecordV1>,
    pub merge_policy: String,
    pub integrated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchCheckpointV1 {
    pub frames: Vec<BranchFrameV1>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BranchError {
    #[error("branch frame {0} already exists")]
    DuplicateFrame(String),
    #[error("unknown branch frame {0}")]
    UnknownFrame(String),
    #[error("unknown branch {0}")]
    UnknownBranch(String),
    #[error("branch {0} has already reached a terminal state")]
    BranchTerminal(String),
    #[error("join frame is not ready")]
    JoinNotReady,
    #[error("join frame was already integrated")]
    AlreadyIntegrated,
    #[error("branch count must be between 1 and {MAX_BRANCHES_PER_FRAME}")]
    InvalidBranchCount,
    #[error("duplicate declared branch id")]
    DuplicateBranch,
    #[error("unsupported merge policy {0}")]
    UnsupportedMergePolicy(String),
    #[error("object merge requires object branch results")]
    ObjectMergeType,
}

#[derive(Debug, Default)]
pub struct BranchJoinCoordinator {
    frames: BTreeMap<String, BranchFrameV1>,
}

impl BranchJoinCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_parallel(
        &mut self,
        frame_id: StableId,
        parent_token_id: StableId,
        join_node_id: StableId,
        declared_branch_ids: &[StableId],
        merge_policy: impl Into<String>,
    ) -> Result<Vec<BranchRecordV1>, BranchError> {
        if declared_branch_ids.is_empty() || declared_branch_ids.len() > MAX_BRANCHES_PER_FRAME {
            return Err(BranchError::InvalidBranchCount);
        }
        let merge_policy = merge_policy.into();
        if !matches!(merge_policy.as_str(), "ordered_array" | "object_overlay") {
            return Err(BranchError::UnsupportedMergePolicy(merge_policy));
        }
        if self.frames.contains_key(frame_id.as_str()) {
            return Err(BranchError::DuplicateFrame(frame_id.to_string()));
        }
        let unique: BTreeSet<_> = declared_branch_ids.iter().map(StableId::as_str).collect();
        if unique.len() != declared_branch_ids.len() {
            return Err(BranchError::DuplicateBranch);
        }
        let branches = declared_branch_ids
            .iter()
            .enumerate()
            .map(|(ordinal, id)| BranchRecordV1 {
                branch_id: id.clone(),
                ordinal: u32::try_from(ordinal).expect("branch bound fits u32"),
                state: BranchStateV1::Ready,
                result: None,
            })
            .collect::<Vec<_>>();
        self.frames.insert(
            frame_id.as_str().to_owned(),
            BranchFrameV1 {
                frame_id,
                parent_token_id,
                join_node_id,
                branches: branches.clone(),
                merge_policy,
                integrated: false,
            },
        );
        Ok(branches)
    }

    pub fn open_for_each(
        &mut self,
        frame_id: StableId,
        parent_token_id: StableId,
        join_node_id: StableId,
        items: &[Value],
        maximum_items: usize,
        merge_policy: impl Into<String>,
    ) -> Result<Vec<(BranchRecordV1, Value)>, BranchError> {
        if items.is_empty() || items.len() > maximum_items || items.len() > MAX_BRANCHES_PER_FRAME {
            return Err(BranchError::InvalidBranchCount);
        }
        let ids = (0..items.len())
            .map(|ordinal| stable_branch_id(&frame_id, ordinal))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| BranchError::InvalidBranchCount)?;
        let branches =
            self.open_parallel(frame_id, parent_token_id, join_node_id, &ids, merge_policy)?;
        Ok(branches.into_iter().zip(items.iter().cloned()).collect())
    }

    pub fn mark_running(
        &mut self,
        frame_id: &StableId,
        branch_id: &StableId,
    ) -> Result<(), BranchError> {
        let branch = self.branch_mut(frame_id, branch_id)?;
        if branch.state != BranchStateV1::Ready {
            return Err(BranchError::BranchTerminal(branch_id.to_string()));
        }
        branch.state = BranchStateV1::Running;
        Ok(())
    }

    pub fn complete(
        &mut self,
        frame_id: &StableId,
        branch_id: &StableId,
        result: Value,
    ) -> Result<(), BranchError> {
        let branch = self.branch_mut(frame_id, branch_id)?;
        if matches!(
            branch.state,
            BranchStateV1::Completed | BranchStateV1::Cancelled | BranchStateV1::Failed
        ) {
            return Err(BranchError::BranchTerminal(branch_id.to_string()));
        }
        branch.state = BranchStateV1::Completed;
        branch.result = Some(result);
        Ok(())
    }

    pub fn cancel_frame(&mut self, frame_id: &StableId) -> Result<(), BranchError> {
        let frame = self
            .frames
            .get_mut(frame_id.as_str())
            .ok_or_else(|| BranchError::UnknownFrame(frame_id.to_string()))?;
        for branch in &mut frame.branches {
            if !matches!(
                branch.state,
                BranchStateV1::Completed | BranchStateV1::Cancelled | BranchStateV1::Failed
            ) {
                branch.state = BranchStateV1::Cancelled;
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn is_join_ready(&self, frame_id: &StableId) -> bool {
        self.frames.get(frame_id.as_str()).is_some_and(|frame| {
            frame.branches.iter().all(|branch| {
                matches!(
                    branch.state,
                    BranchStateV1::Completed | BranchStateV1::Cancelled | BranchStateV1::Failed
                )
            })
        })
    }

    pub fn integrate(&mut self, frame_id: &StableId) -> Result<Value, BranchError> {
        let frame = self
            .frames
            .get_mut(frame_id.as_str())
            .ok_or_else(|| BranchError::UnknownFrame(frame_id.to_string()))?;
        if frame.integrated {
            return Err(BranchError::AlreadyIntegrated);
        }
        if !frame.branches.iter().all(|branch| {
            matches!(
                branch.state,
                BranchStateV1::Completed | BranchStateV1::Cancelled | BranchStateV1::Failed
            )
        }) {
            return Err(BranchError::JoinNotReady);
        }
        let value = match frame.merge_policy.as_str() {
            "ordered_array" => Value::Array(
                frame
                    .branches
                    .iter()
                    .map(|branch| branch.result.clone().unwrap_or(Value::Null))
                    .collect(),
            ),
            "object_overlay" => {
                let mut merged = Map::new();
                for branch in &frame.branches {
                    let Some(value) = &branch.result else {
                        continue;
                    };
                    let object = value.as_object().ok_or(BranchError::ObjectMergeType)?;
                    for (key, value) in object {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(merged)
            }
            other => return Err(BranchError::UnsupportedMergePolicy(other.to_owned())),
        };
        frame.integrated = true;
        Ok(value)
    }

    #[must_use]
    pub fn checkpoint(&self) -> BranchCheckpointV1 {
        BranchCheckpointV1 {
            frames: self.frames.values().cloned().collect(),
        }
    }

    pub fn restore(checkpoint: BranchCheckpointV1) -> Result<Self, BranchError> {
        let mut frames = BTreeMap::new();
        for frame in checkpoint.frames {
            if frame.branches.is_empty() || frame.branches.len() > MAX_BRANCHES_PER_FRAME {
                return Err(BranchError::InvalidBranchCount);
            }
            if !matches!(
                frame.merge_policy.as_str(),
                "ordered_array" | "object_overlay"
            ) {
                return Err(BranchError::UnsupportedMergePolicy(frame.merge_policy));
            }
            let ids: BTreeSet<_> = frame
                .branches
                .iter()
                .map(|branch| branch.branch_id.as_str())
                .collect();
            let ordinals: BTreeSet<_> =
                frame.branches.iter().map(|branch| branch.ordinal).collect();
            let expected_ordinals: BTreeSet<_> = (0..frame.branches.len())
                .map(|ordinal| u32::try_from(ordinal).expect("branch bound fits u32"))
                .collect();
            if ids.len() != frame.branches.len()
                || ordinals != expected_ordinals
                || frame.branches.iter().any(|branch| match branch.state {
                    BranchStateV1::Completed => branch.result.is_none(),
                    _ => branch.result.is_some(),
                })
                || (frame.integrated
                    && !frame.branches.iter().all(|branch| {
                        matches!(
                            branch.state,
                            BranchStateV1::Completed
                                | BranchStateV1::Cancelled
                                | BranchStateV1::Failed
                        )
                    }))
            {
                return Err(BranchError::DuplicateBranch);
            }
            if frames
                .insert(frame.frame_id.as_str().to_owned(), frame)
                .is_some()
            {
                return Err(BranchError::DuplicateBranch);
            }
        }
        Ok(Self { frames })
    }

    fn branch_mut(
        &mut self,
        frame_id: &StableId,
        branch_id: &StableId,
    ) -> Result<&mut BranchRecordV1, BranchError> {
        self.frames
            .get_mut(frame_id.as_str())
            .ok_or_else(|| BranchError::UnknownFrame(frame_id.to_string()))?
            .branches
            .iter_mut()
            .find(|branch| branch.branch_id == *branch_id)
            .ok_or_else(|| BranchError::UnknownBranch(branch_id.to_string()))
    }
}

fn stable_branch_id(frame_id: &StableId, ordinal: usize) -> Result<StableId, ()> {
    let digest = Sha256::digest(format!("{}:{ordinal}", frame_id.as_str()).as_bytes());
    StableId::parse(format!("branch.{ordinal}.{:x}", digest)[..64].to_owned()).map_err(|_| ())
}
