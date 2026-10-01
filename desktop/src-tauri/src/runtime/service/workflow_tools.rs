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
                let Some(discovered) = mcp_definitions.get(&tool_id) else {
                    warnings.push(format!("MCP tool '{tool_id}' is unavailable for this pass"));
                    continue;
                };
                let definition = discovered.definition.clone();
                let (server_id, tool) = split_mcp_capability(&tool_id).map_err(|error| error)?;
                let configured_server = settings
                    .mcp_servers
                    .iter()
                    .find(|server| server.id == server_id);
                let saved_tool = configured_server
                    .and_then(|server| server.tools.iter().find(|entry| entry.name == tool));
                if saved_tool.is_some_and(|entry| !entry.enabled) {
                    warnings.push(format!("MCP tool '{tool_id}' is disabled in Settings"));
                    continue;
                }
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
