//! Explicit local rebinding and conservative two-scan retention planning.

use std::collections::{BTreeMap, BTreeSet};

/// A requirement can be inspected even when the local machine cannot satisfy it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequirement {
    pub logical_id: String,
    pub version: String,
}

/// A continuation proposal creates a new child branch; it never imports authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebindPlan {
    pub missing: Vec<CapabilityRequirement>,
    pub child_branch_required: bool,
    pub fresh_authority_required: bool,
}

/// Builds a local-only compatibility plan without consulting credentials,
/// approvals, or imported bindings.
#[must_use]
pub fn plan_rebind(
    required: &[CapabilityRequirement],
    available: &BTreeSet<(String, String)>,
) -> RebindPlan {
    let missing = required
        .iter()
        .filter(|requirement| {
            !available.contains(&(requirement.logical_id.clone(), requirement.version.clone()))
        })
        .cloned()
        .collect();
    RebindPlan {
        missing,
        child_branch_required: true,
        fresh_authority_required: true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCapabilityV1 {
    pub logical_id: String,
    pub portable_version: String,
    pub local_binding_id: String,
    pub version_hash: String,
    pub compatible: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebindResolutionV1 {
    Resolved {
        requirement: CapabilityRequirement,
        local_binding_id: String,
        version_hash: String,
    },
    Missing(CapabilityRequirement),
    Ambiguous {
        requirement: CapabilityRequirement,
        candidates: Vec<String>,
    },
    Incompatible(CapabilityRequirement),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinuationRebindPlanV1 {
    pub parent_branch_id: String,
    pub proposed_child_branch_id: String,
    pub resolutions: Vec<RebindResolutionV1>,
    pub can_continue_after_fresh_approval: bool,
    pub imported_approvals_accepted: bool,
    pub imported_secret_handles_accepted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildContinuationPlanV1 {
    pub parent_chat_id: String,
    pub parent_branch_id: String,
    pub child_chat_id: String,
    pub child_branch_id: String,
    pub resolutions: Vec<RebindResolutionV1>,
    pub fresh_snapshot_required: bool,
    pub fresh_authority_required: bool,
    pub fresh_approvals_required: bool,
    pub imported_runtime_resumable: bool,
    pub can_create_after_user_confirmation: bool,
}

#[must_use]
pub fn plan_continuation_rebind(
    parent_branch_id: &str,
    proposed_child_branch_id: &str,
    required: &[CapabilityRequirement],
    local: &[LocalCapabilityV1],
) -> ContinuationRebindPlanV1 {
    let resolutions: Vec<_> = required
        .iter()
        .map(|requirement| {
            let mut candidates: Vec<_> = local
                .iter()
                .filter(|candidate| {
                    candidate.logical_id == requirement.logical_id
                        && candidate.portable_version == requirement.version
                })
                .collect();
            candidates.sort_by(|left, right| left.local_binding_id.cmp(&right.local_binding_id));
            let compatible: Vec<_> = candidates
                .iter()
                .filter(|candidate| candidate.compatible)
                .collect();
            match compatible.as_slice() {
                [candidate] => RebindResolutionV1::Resolved {
                    requirement: requirement.clone(),
                    local_binding_id: candidate.local_binding_id.clone(),
                    version_hash: candidate.version_hash.clone(),
                },
                [] if candidates.is_empty() => RebindResolutionV1::Missing(requirement.clone()),
                [] => RebindResolutionV1::Incompatible(requirement.clone()),
                values => RebindResolutionV1::Ambiguous {
                    requirement: requirement.clone(),
                    candidates: values
                        .iter()
                        .map(|candidate| candidate.local_binding_id.clone())
                        .collect(),
                },
            }
        })
        .collect();
    let can_continue_after_fresh_approval = resolutions
        .iter()
        .all(|resolution| matches!(resolution, RebindResolutionV1::Resolved { .. }));
    ContinuationRebindPlanV1 {
        parent_branch_id: parent_branch_id.to_owned(),
        proposed_child_branch_id: proposed_child_branch_id.to_owned(),
        resolutions,
        can_continue_after_fresh_approval,
        imported_approvals_accepted: false,
        imported_secret_handles_accepted: false,
    }
}

#[must_use]
pub fn plan_child_continuation(
    parent_chat_id: &str,
    parent_branch_id: &str,
    child_chat_id: &str,
    child_branch_id: &str,
    required: &[CapabilityRequirement],
    local: &[LocalCapabilityV1],
) -> ChildContinuationPlanV1 {
    let base = plan_continuation_rebind(parent_branch_id, child_branch_id, required, local);
    ChildContinuationPlanV1 {
        parent_chat_id: parent_chat_id.to_owned(),
        parent_branch_id: base.parent_branch_id,
        child_chat_id: child_chat_id.to_owned(),
        child_branch_id: base.proposed_child_branch_id,
        resolutions: base.resolutions,
        fresh_snapshot_required: true,
        fresh_authority_required: true,
        fresh_approvals_required: true,
        imported_runtime_resumable: false,
        can_create_after_user_confirmation: base.can_continue_after_fresh_approval
            && parent_chat_id != child_chat_id
            && parent_branch_id != child_branch_id
            && valid_identity(parent_chat_id)
            && valid_identity(parent_branch_id)
            && valid_identity(child_chat_id)
            && valid_identity(child_branch_id),
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Conservative retention result: only externally-proven unreachable identities may be collected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionPlan {
    pub collectable: Vec<String>,
    pub retained: Vec<String>,
}

/// Compatibility one-scan plan. The caller-provided `grace_expired` set is an
/// external proof and this function never deletes anything itself.
#[must_use]
pub fn retention_plan(
    all: &BTreeSet<String>,
    reachable: &BTreeSet<String>,
    grace_expired: &BTreeSet<String>,
) -> RetentionPlan {
    let collectable: Vec<String> = all
        .iter()
        .filter(|value| !reachable.contains(*value) && grace_expired.contains(*value))
        .cloned()
        .collect();
    let retained = all
        .iter()
        .filter(|value| !collectable.contains(*value))
        .cloned()
        .collect();
    RetentionPlan {
        collectable,
        retained,
    }
}

/// Reachability facts collected while holding the repository's head snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReachabilityScanV1 {
    pub generation: u64,
    pub observed_epoch_millis: u64,
    pub branch_heads: BTreeMap<String, String>,
    pub reachable: BTreeSet<String>,
}

/// Two-phase collection requires absence in both scans, elapsed grace, and a
/// strictly newer final generation. A newly reachable object is retained.
pub fn retention_plan_two_phase(
    all: &BTreeSet<String>,
    first: &ReachabilityScanV1,
    final_scan: &ReachabilityScanV1,
    first_unreachable_epoch_millis: &BTreeMap<String, u64>,
    grace_millis: u64,
) -> Result<RetentionPlan, RetentionError> {
    if final_scan.generation <= first.generation
        || final_scan.observed_epoch_millis < first.observed_epoch_millis
    {
        return Err(RetentionError::StaleFinalScan);
    }
    let collectable: Vec<_> = all
        .iter()
        .filter(|identity| {
            !first.reachable.contains(*identity)
                && !final_scan.reachable.contains(*identity)
                && first_unreachable_epoch_millis
                    .get(*identity)
                    .and_then(|started| started.checked_add(grace_millis))
                    .is_some_and(|deadline| deadline <= final_scan.observed_epoch_millis)
        })
        .cloned()
        .collect();
    let retained = all
        .iter()
        .filter(|identity| !collectable.contains(*identity))
        .cloned()
        .collect();
    Ok(RetentionPlan {
        collectable,
        retained,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RetentionError {
    #[error("retention final scan is not strictly newer than its candidate scan")]
    StaleFinalScan,
}
