//! Document-derived configuration for a later pass of an existing Chat.
//!
//! The unit of configuration stability is one pass, never the Chat: the node and
//! edge definitions and the bound tool set come from the workflow document and
//! Settings as they are now, so a capability the user enabled in response to what
//! the model reported missing reaches the next pass of the same Chat. Only Chat
//! identity, the Run's authority ceiling, the workspace binding and committed
//! evidence carry over — see `DesktopRuntime::complete_workflow_input`.
use super::*;

/// The workflow document and tool bindings one later pass runs with.
pub(super) struct CurrentPassConfigurationV1 {
    pub document: Value,
    pub tools: Vec<FrozenToolBindingV1>,
}

impl DesktopRuntime {
    /// Resolves the current documents for a later pass of the identified Chat.
    ///
    /// Built-in capabilities resolve through the same path as a first-input
    /// freeze, so a tool the user enabled is bound and offered immediately. An
    /// MCP capability must already belong to the Run's frozen connections: a
    /// server the Chat never attested needs a New Chat, never a silent grant.
    pub(super) fn resolve_current_pass(
        &self,
        context: &FrozenChatExecutionContextV1,
    ) -> Result<CurrentPassConfigurationV1, String> {
        let mut workflow = self.documents.workflow_snapshot_for(&context.workflow_id);
        if workflow.document.is_null() {
            return Err(format!(
                "workflow '{}' no longer exists in the workflow library",
                context.workflow_id
            ));
        }
        if !workflow.editable {
            return Err(format!(
                "workflow '{}' uses a read-only schema and cannot run",
                context.workflow_id
            ));
        }
        self.documents
            .require_executable_workflow(&context.workflow_id)
            .map_err(|error| format!("workflow '{}' is not executable: {error}", context.workflow_id))?;
        workflow.document = mcp_selection::expand_server_selections(
            &workflow.document,
            self.documents.settings(),
        )?;
        let tools = freeze_current_tools(
            &workflow.document,
            self.documents.settings(),
            &context.tools,
        )?;
        Ok(CurrentPassConfigurationV1 {
            document: workflow.document,
            tools,
        })
    }
}

/// Freezes the graph's bound tool set for a later pass of an existing Chat.
///
/// A capability the Chat already holds keeps the binding it was frozen with: its
/// configuration is that capability's authority contract, and disabling or
/// reconfiguring it in Settings feeds a New Chat rather than revoking the
/// authority a running Chat already holds. A capability the Chat never held
/// resolves through the same path as a first-input freeze, so it must be
/// installed and enabled in saved Settings to reach this pass.
fn freeze_current_tools(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
    frozen: &[FrozenToolBindingV1],
) -> Result<Vec<FrozenToolBindingV1>, String> {
    let mut seen = BTreeSet::new();
    let mut tools = Vec::new();
    for tool_id in document_tool_ids(workflow)? {
        if !seen.insert(tool_id.clone()) {
            continue;
        }
        if let Some(binding) = frozen.iter().find(|tool| tool.tool_id == tool_id) {
            tools.push(binding.clone());
            continue;
        }
        if tool_id.starts_with(MCP_CAPABILITY_PREFIX) {
            return Err(format!(
                "workflow binds MCP tool '{tool_id}', which this Chat never connected; start a New Chat to use it"
            ));
        }
        tools.push(super::freeze_builtin_tool(&tool_id, settings)?);
    }
    Ok(tools)
}

/// The distinct capability ids an agent or tool node binds, in document order.
fn document_tool_ids(workflow: &Value) -> Result<Vec<String>, String> {
    let nodes = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "workflow nodes are missing".to_owned())?;
    let mut ids = Vec::new();
    for node in nodes {
        let node_id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow node id is missing".to_owned())?;
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let configuration = node.get("configuration").and_then(Value::as_object);
        match node_type {
            "agent" => {
                let bound = configuration
                    .and_then(|config| config.get("toolIds"))
                    .and_then(Value::as_array)
                    .map(|bound| {
                        bound
                            .iter()
                            .map(|id| id.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| format!("workflow node '{node_id}' toolIds must be an array"))?
                    .unwrap_or_default();
                ids.extend(bound);
            }
            "tool" => {
                if let Some(tool_id) = configuration
                    .and_then(|config| config.get("toolId"))
                    .and_then(Value::as_str)
                {
                    ids.push(tool_id.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(ids)
}
