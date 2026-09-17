//! Native tool descriptions are part of the frozen model context, not live catalog data.
use super::*;

/// Validate the requested configuration normally, then preserve native descriptions
/// from the original prepared Run. Rebuilding presentation text after an upgrade
/// changes the graph hash and falsely rejects an otherwise identical continuation.
/// All executable schemas, limits, options, secrets and MCP definitions still pass
/// through the existing strict frozen-run / exact-request equality checks.
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
