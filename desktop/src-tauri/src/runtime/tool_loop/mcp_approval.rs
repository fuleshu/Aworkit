//! MCP approval routing uses the user's override or both explicit server hints.
//! This controls review only; it does not change execution/effect classification.
use super::*;
use aworkit_capability_host::McpToolAnnotationsV1;

pub(super) fn annotations(
    configuration: &Value,
) -> Result<Option<McpToolAnnotationsV1>, WorkflowPipelineError> {
    configuration.get("annotations")
        .map(|value| serde_json::from_value(value.clone())
            .map_err(|_| invalid_tool("MCP annotations must contain optional boolean readOnlyHint and destructiveHint fields")))
        .transpose()
}

pub(super) fn requires_approval(
    binding: &WorkflowToolBindingV1,
) -> Result<bool, WorkflowPipelineError> {
    let hints = annotations(&binding.configuration)?;
    Ok(!binding.options.auto_approve
        && !hints.is_some_and(|hints| hints.permits_approval_free_call()))
}
