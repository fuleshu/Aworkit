//! Freeze only available workflow tools and explain omitted capabilities.
use super::*;

pub(super) struct FrozenWorkflowAgentV1 {
    pub(super) run_deadline_millis: u64,
    pub(super) tools: Vec<FrozenToolBindingV1>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn freeze_graph_bindings(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
    mcp_definitions: &BTreeMap<String, DiscoveredMcpDefinition>,
) -> Result<FrozenWorkflowAgentV1, String> {
    let nodes = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "workflow nodes are missing".to_owned())?;
    let mut seen = BTreeSet::new();
    let mut tools = Vec::new();
    let mut warnings = Vec::new();
    for node in nodes {
        let node_id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow node id is missing".to_owned())?;
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let configuration = node.get("configuration").and_then(Value::as_object);
        let tool_ids: Vec<String> = match node_type {
            "agent" => configuration
                .and_then(|config| config.get("toolIds"))
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| format!("workflow node '{node_id}' toolIds must be an array"))?
                .unwrap_or_default(),
            "tool" | "external_agent" => configuration
                .and_then(|config| config.get("toolId"))
                .and_then(Value::as_str)
                .map(|tool_id| vec![tool_id.to_owned()])
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for tool_id in tool_ids {
            if !seen.insert(tool_id.clone()) {
                continue;
            }
            if tool_id.starts_with(MCP_CAPABILITY_PREFIX) {
                let (server_id, tool) = split_mcp_capability(&tool_id)?;
                let configured_server = settings
                    .mcp_servers
                    .iter()
                    .find(|server| server.id == server_id);
                let saved_tool = configured_server
                    .and_then(|server| server.tools.iter().find(|entry| entry.name == tool));
                // Settings decides whether a function may be called; discovery
                // only decides what its interface is. A disabled function is
                // never offered, whether or not its server is running.
                if saved_tool.is_some_and(|entry| !entry.enabled) {
                    warnings.push(format!("MCP tool '{tool_id}' is disabled in Settings"));
                    continue;
                }
                let Some(discovered) = mcp_definitions.get(&tool_id) else {
                    // The live catalog and the saved catalog both failed to
                    // describe it, so the Chat has no connection to offer. A
                    // server that is merely closed is described from Settings
                    // and never reaches this branch.
                    warnings.push(format!(
                        "MCP tool '{tool_id}' could not be prepared from Settings → MCP; the pass continues without it"
                    ));
                    continue;
                };
                let definition = discovered.definition.clone();
                // Descriptions already travel in the frozen tool definition. Only
                // explicitly configured instructions belong in the system prompt.
                let options = saved_tool
                    .map(|entry| entry.options.clone())
                    .unwrap_or_default();
                // The alias leads with the configured server name so the model can
                // re-identify its tools; a server without a usable name falls back
                // to its own id.
                let label = configured_server
                    .map(|server| server.name.clone())
                    .unwrap_or_else(|| mcp_fallback_label(server_id));
                let snapshot = BuiltInToolConfigurationV2 {
                    options,
                    id: tool_id.clone(),
                    name: mcp_provider_name(server_id, &label, tool),
                    enabled: true,
                    requires_project: false,
                    credential_bindings: Vec::new(),
                    configuration: discovered.configuration(server_id, tool)?,
                };
                let tool_hash = canonical_hash(&snapshot)?;
                tools.push(FrozenToolBindingV1 {
                    tool_id,
                    tool_hash,
                    tool_snapshot: snapshot,
                    credentials: Vec::new(),
                    definition: Some(definition),
                });
                continue;
            }
            if crate::runtime::comfyui::is_comfyui_authoring_capability(&tool_id) {
                // The read-only authoring helpers freeze the endpoint and, for
                // the workflow reader, the workflow table resolved from
                // Settings now, so a later Settings edit cannot change what a
                // running Chat inspects. They share the ComfyUI prefix but are
                // not Settings-derived workflow tools, so this exact-id test
                // must come first: the workflow-tool branch below would report
                // them as unavailable and drop them.
                let provider_name =
                    crate::runtime::comfyui::comfyui_authoring_provider_name(&tool_id)
                        .unwrap_or_default();
                let snapshot = BuiltInToolConfigurationV2 {
                    options: Default::default(),
                    id: tool_id.clone(),
                    name: provider_name.to_owned(),
                    enabled: true,
                    requires_project: false,
                    credential_bindings: Vec::new(),
                    configuration: crate::runtime::comfyui::comfyui_authoring_configuration(
                        &tool_id,
                        &settings.comfyui.endpoint,
                        &settings.comfyui.workflow_tools,
                    ),
                };
                let tool_hash = canonical_hash(&snapshot)?;
                tools.push(FrozenToolBindingV1 {
                    tool_id: tool_id.clone(),
                    tool_hash,
                    tool_snapshot: snapshot,
                    credentials: Vec::new(),
                    definition: Some(aworkit_capability_host::ModelToolDefinitionV1 {
                        capability_id: tool_id.clone(),
                        name: provider_name.to_owned(),
                        description: crate::runtime::comfyui::comfyui_authoring_description(
                            &tool_id,
                        )
                        .unwrap_or_default()
                        .to_owned(),
                        input_schema: crate::runtime::comfyui::comfyui_authoring_schema(&tool_id),
                    }),
                });
                continue;
            }
            if tool_id.starts_with(crate::runtime::comfyui::COMFYUI_CAPABILITY_PREFIX) {
                // A ComfyUI workflow tool is a native tool whose model-facing
                // definition is generated from the saved workflow tool. Settings
                // is the single enable decision, exactly like an MCP function.
                let Some(native) =
                    crate::runtime::comfyui::comfyui_native_tool(&settings.comfyui, &tool_id)
                else {
                    warnings.push(comfyui_tool_warning(
                        &settings.comfyui.workflow_tools,
                        &tool_id,
                    ));
                    continue;
                };
                let snapshot = BuiltInToolConfigurationV2 {
                    options: Default::default(),
                    id: tool_id.clone(),
                    name: native.name.clone(),
                    enabled: true,
                    requires_project: false,
                    credential_bindings: Vec::new(),
                    configuration: crate::runtime::comfyui::comfyui_frozen_configuration(
                        &native.tool,
                        &settings.comfyui.endpoint,
                    ),
                };
                let tool_hash = canonical_hash(&snapshot)?;
                tools.push(FrozenToolBindingV1 {
                    tool_id,
                    tool_hash,
                    tool_snapshot: snapshot,
                    credentials: Vec::new(),
                    definition: Some(aworkit_capability_host::ModelToolDefinitionV1 {
                        capability_id: native.capability_id.clone(),
                        name: native.name.clone(),
                        description: native.description.clone(),
                        input_schema: native.input_schema.clone(),
                    }),
                });
                continue;
            }
            match freeze_builtin_tool(&tool_id, settings) {
                Ok(tool) => tools.push(tool),
                Err(error) => warnings.push(format!("Tool '{tool_id}' is unavailable: {error}")),
            }
        }
    }
    Ok(FrozenWorkflowAgentV1 {
        // Preserve the durable field for old Chat records without deriving
        // execution behavior from the removed Agent timeoutSeconds setting.
        run_deadline_millis: DEFAULT_MODEL_CALL_TIMEOUT_SECONDS.saturating_mul(1_000),
        tools,
        warnings,
    })
}

/// Explains why a ComfyUI workflow tool id produced no binding.
///
/// Settings is the single enable decision and a Chat freezes its tools at the
/// first input, so three different causes look identical to a model otherwise:
/// the tool is disabled, it no longer exists because it was renamed, replaced or
/// removed, or the id was never a workflow tool. Naming the cause and the next
/// step keeps a model from hunting for a tool it cannot call.
fn comfyui_tool_warning(
    tools: &[crate::runtime::comfyui::ComfyUiWorkflowToolV2],
    tool_id: &str,
) -> String {
    let id = crate::runtime::comfyui::comfyui_tool_id(tool_id).unwrap_or(tool_id);
    match tools.iter().find(|tool| tool.id == id) {
        Some(tool) if !tool.enabled => format!(
            "ComfyUI tool '{tool_id}' is disabled in Settings; enable it and start a new Chat, because a Chat keeps the tools it started with"
        ),
        Some(_) => format!(
            "ComfyUI tool '{tool_id}' is enabled in Settings but was not frozen into this Chat; start a new Chat"
        ),
        None => format!(
            "ComfyUI tool '{tool_id}' is not configured in Settings; it may have been renamed, replaced or removed. A Chat keeps the tools it started with, so a tool configured now appears in the next Chat. Call comfyui_get_workflow without an argument to list the configured workflow tools"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, enabled: bool) -> crate::runtime::comfyui::ComfyUiWorkflowToolV2 {
        crate::runtime::comfyui::ComfyUiWorkflowToolV2 {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            workflow_path: format!("D:\\workflows\\{id}.json"),
            enabled,
            parameters: Vec::new(),
        }
    }

    #[test]
    fn a_missing_comfyui_tool_names_the_cause_and_the_next_step() {
        let tools = vec![tool("krea", true), tool("off", false)];
        let disabled = comfyui_tool_warning(&tools, "comfyui.off");
        assert!(disabled.contains("disabled in Settings"), "{disabled}");
        assert!(disabled.contains("new Chat"), "{disabled}");

        // The failing Chat asked for a tool id the user had replaced; the
        // message must say so instead of claiming it is unavailable.
        let absent = comfyui_tool_warning(&tools, "comfyui.qwen-image-2-1-turbo-text-to-image");
        assert!(absent.contains("not configured in Settings"), "{absent}");
        assert!(absent.contains("renamed"), "{absent}");
        assert!(absent.contains("comfyui_get_workflow"), "{absent}");

        // An id outside the ComfyUI prefix is described too, never panicking.
        assert!(comfyui_tool_warning(&tools, "comfyui.missing").contains("not configured"));
    }
}
