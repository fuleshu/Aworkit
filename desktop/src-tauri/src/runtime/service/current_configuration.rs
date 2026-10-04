//! Document-derived configuration for a later pass of an existing Chat.
//!
//! The unit of configuration stability is one pass, never the Chat: the node and
//! edge definitions and the bound tool set come from the workflow document and
//! Settings as they are now, so a capability the user enabled in response to what
//! the model reported missing reaches the next pass of the same Chat. Only Chat
//! identity, the Run's authority ceiling, the workspace binding and committed
//! evidence carry over — see `DesktopRuntime::complete_workflow_input`.
use super::*;

use crate::runtime::documents::validate_executable_snapshot;

/// The workflow document and tool bindings one later pass runs with.
pub(super) struct CurrentPassConfigurationV1 {
    pub document: Value,
    pub tools: Vec<FrozenToolBindingV1>,
}

impl DesktopRuntime {
    /// Resolves the current documents for a later pass of the identified Chat.
    ///
    /// A saved workflow that still exists **and is still executable** is the
    /// current revision and feeds the pass, so a mid-Chat edit reaches the same
    /// Chat. A saved entry this build can no longer run — renamed, deleted,
    /// edited into a non-executable state, or saved from a read-only schema —
    /// does not end the Chat: the pass keeps the graph the Chat froze and
    /// reports a notice, so editing a saved workflow mid-Chat can never block
    /// every later message.
    ///
    /// Built-in capabilities resolve through the same path as a first-input
    /// freeze, so a tool the user enabled is bound and offered immediately. An
    /// MCP capability must already belong to the Run's frozen connections: a
    /// server the Chat never attested needs a New Chat, never a silent grant.
    pub(super) fn resolve_current_pass(
        &self,
        context: &FrozenChatExecutionContextV1,
    ) -> Result<CurrentPassConfigurationV1, String> {
        let stored = self.documents.workflow_snapshot_for(&context.workflow_id);
        let live_entry_usable = !stored.document.is_null()
            && stored.editable
            && self
                .documents
                .require_executable_workflow(&context.workflow_id)
                .is_ok();
        let mut workflow = if live_entry_usable {
            stored
        } else {
            // The frozen snapshot was validated when the Chat froze it, and it
            // is the Chat's own committed evidence, so a later pass runs it.
            // This is checked for a *usable* document, not as an upgrade gate.
            let mut fallback = WorkflowSnapshot {
                version: context.workflow_version,
                document: context.workflow_snapshot.clone(),
                editable: true,
                execution_verdict: None,
            };
            if let Err(error) = validate_executable_snapshot(&context.workflow_snapshot) {
                return Err(format!(
                    "frozen workflow '{}' is no longer executable: {error}",
                    context.workflow_id
                ));
            }
            let reason = if stored.document.is_null() {
                format!(
                    "saved workflow '{}' no longer exists; this pass runs the graph the Chat froze",
                    context.workflow_id
                )
            } else if !stored.editable {
                format!(
                    "saved workflow '{}' uses a read-only schema and cannot run; this pass runs the graph the Chat froze",
                    context.workflow_id
                )
            } else {
                format!(
                    "saved workflow '{}' is not executable in this build; this pass runs the graph the Chat froze",
                    context.workflow_id
                )
            };
            super::super::workflow_capabilities::warn(&mut fallback.document, reason);
            fallback
        };
        workflow.document = mcp_selection::expand_server_selections(
            &workflow.document,
            self.documents.settings(),
        )
        .map_err(|error| {
            format!(
                "workflow '{}' MCP selections could not be expanded: {error}",
                context.workflow_id
            )
        })?;
        let (tools, warnings) = freeze_current_tools(
            &workflow.document,
            self.documents.settings(),
            &context.tools,
        );
        for warning in warnings { super::super::workflow_capabilities::warn(&mut workflow.document, warning); }
        super::super::workflow_capabilities::retain_available(&mut workflow.document, &tools.iter().map(|t| t.tool_id.clone()).collect::<Vec<_>>());
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
///
/// Nothing here ends the pass. A binding shape this build no longer executes and
/// a capability that is unavailable, disabled or unknown all drop the binding
/// and report a bounded notice, exactly as the first-input freeze does.
pub(super) fn freeze_current_tools(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
    frozen: &[FrozenToolBindingV1],
) -> (Vec<FrozenToolBindingV1>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut tools = Vec::new();
    let mut warnings = Vec::new();
    let tool_ids = match document_tool_ids(workflow) {
        Ok(tool_ids) => tool_ids,
        Err(shape) => {
            warnings.push(format!(
                "workflow tool bindings could not be read this pass ({shape}); it runs without them"
            ));
            return (tools, warnings);
        }
    };
    for tool_id in tool_ids {
        if !seen.insert(tool_id.clone()) {
            continue;
        }
        if let Some(binding) = frozen.iter().find(|tool| tool.tool_id == tool_id) {
            if crate::runtime::history::frozen_tool_binding_is_executable(binding) {
                tools.push(binding.clone());
            } else {
                // The Chat froze this capability in a shape this build can no
                // longer dispatch. Dropping it and saying so keeps the message
                // alive; the binding stays in the record as evidence.
                warnings.push(format!(
                    "tool '{tool_id}' was frozen by this Chat in a shape this build cannot execute; the pass continues without it"
                ));
            }
            continue;
        }
        if tool_id.starts_with(MCP_CAPABILITY_PREFIX) {
            warnings.push(format!(
                "workflow binds MCP tool '{tool_id}', which this Chat never connected; start a New Chat to use it"
            ));
            continue;
        }
        match super::freeze_builtin_tool(&tool_id, settings) {
            Ok(tool) => tools.push(tool),
            Err(error) => warnings.push(format!("Tool '{tool_id}' is unavailable: {error}")),
        }
    }
    (tools, warnings)
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
            "tool" | "external_agent" => {
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
