//! Explicit, non-authoritative rebinding and non-destructive integrity facts.

use std::collections::BTreeSet;

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
/// Builds a local-only plan without consulting credentials, approvals, or imported bindings.
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
/// Conservative retention result: only externally-proven unreachable identities may be collected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionPlan {
    pub collectable: Vec<String>,
    pub retained: Vec<String>,
}
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
