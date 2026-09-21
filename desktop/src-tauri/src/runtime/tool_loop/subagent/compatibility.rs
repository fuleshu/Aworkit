//! Explicit adapter compatibility for Chats frozen before readOnly was added.
//!
//! Preserve the saved manifest. Only this known schema extension is compatible;
//! changes to executor permissions, limits or any other descriptor field are not.
use super::*;

pub(crate) fn legacy_descriptor(
    current: &CapabilityDescriptor,
) -> Result<CapabilityDescriptor, WorkflowPipelineError> {
    let mut schema = subagent_schema();
    // Fields a later build added to the model-facing schema. The legacy
    // descriptor is the exact pre-extension contract, so every added field
    // must be removed before its historical hash is recomputed.
    if let Some(properties) = schema["properties"].as_object_mut() {
        properties.remove("readOnly");
        properties.remove("runInBackground");
    }
    let mut legacy = current.clone();
    legacy.input_schema_hash = Some(canonical_hash(&schema)?);
    legacy
        .rehash()
        .map_err(|e| WorkflowPipelineError::Host(e.to_string()))?;
    Ok(legacy)
}

/// The binding itself still selects the old child scope and argument validator.
/// Matching a historical hash alone must never enable the new delegation mode.
pub(crate) fn accepts_legacy(
    binding: &StoredFileToolBindingV1,
    current: &CapabilityDescriptor,
    frozen_hash: &str,
) -> bool {
    binding.capability_id == SUBAGENT_CAPABILITY_ID
        && current.capability_id == SUBAGENT_CAPABILITY_ID
        && matches!(
            binding.limit,
            StoredFileToolLimitV1::Subagent {
                inherit_parent_tools: false,
                ..
            }
        )
        && legacy_descriptor(current).is_ok_and(|legacy| legacy.version_hash == frozen_hash)
}
