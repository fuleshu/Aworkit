//! Native tool descriptions are part of the frozen model context, not live catalog data.
use super::*;

/// New Chats freeze the requested bindings without a previous definition.
/// Upgrade fixtures can retain historical native descriptions when materializing
/// an old snapshot. Live continuation reuses its saved record in continuation.rs
/// directly and never resolves or re-freezes tool bindings through this helper.
pub(super) fn freeze(
    requested: &[WorkflowToolBindingV1],
    previous: Option<&[StoredFileToolBindingV1]>,
) -> Result<Vec<StoredFileToolBindingV1>, WorkflowPipelineError> {
    let mut bindings = freeze_file_tool_bindings(requested)?;
    if let Some(previous) = previous {
        for binding in &mut bindings {
            if !binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX)
                && let Some(saved) = previous
                    .iter()
                    .find(|saved| saved.capability_id == binding.capability_id)
            {
                binding.description.clone_from(&saved.description);
            }
        }
    }
    Ok(bindings)
}
