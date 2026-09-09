//! Keep discovered approval hints beside model definitions until Chat freeze.
//! Only the model definition is exposed to the provider; hints enter the frozen
//! tool configuration, whose hash also covers the user's approval choices.
use super::*;
use aworkit_capability_host::{McpToolAnnotationsV1, McpToolDescriptorV1};

pub(super) struct DiscoveredMcpDefinition {
    pub definition: ModelToolDefinitionV1,
    pub annotations: Option<McpToolAnnotationsV1>,
}

impl DiscoveredMcpDefinition {
    pub fn from_descriptor(capability_id: &str, server: &str, tool: &McpToolDescriptorV1) -> Self {
        Self {
            definition: ModelToolDefinitionV1 {
                capability_id: capability_id.to_owned(),
                name: mcp_provider_name(server, &tool.name),
                description: if tool.description.is_empty() {
                    format!("Call MCP tool '{}' on server '{server}'.", tool.name)
                } else {
                    tool.description.clone()
                },
                input_schema: tool.input_schema.clone(),
            },
            annotations: tool.annotations.clone(),
        }
    }

    pub fn configuration(
        &self,
        server: &str,
        tool: &str,
    ) -> Result<BTreeMap<String, Value>, String> {
        let mut configuration = BTreeMap::from([
            ("serverId".to_owned(), Value::String(server.to_owned())),
            ("tool".to_owned(), Value::String(tool.to_owned())),
        ]);
        if let Some(annotations) = &self.annotations {
            configuration.insert(
                "annotations".into(),
                serde_json::to_value(annotations).map_err(|e| e.to_string())?,
            );
        }
        Ok(configuration)
    }
}

/// Readiness is inert; live discovery replaces saved hints when a Chat starts.
pub(super) fn preview_mcp_definitions(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
) -> Result<BTreeMap<String, DiscoveredMcpDefinition>, String> {
    graph_mcp_tool_ids(workflow).into_iter().map(|id| {
        let (server_id, name) = split_mcp_capability(&id).map_err(|error| error.to_string())?;
        let server = settings.mcp_servers.iter().find(|server| server.id == server_id && server.enabled)
            .ok_or_else(|| format!("MCP server '{server_id}' is missing or disabled in Settings"))?;
        let tool = server.tools.iter().find(|tool| tool.name == name && tool.enabled)
            .ok_or_else(|| format!("MCP tool '{id}' is missing or disabled; discover and enable it in Settings → MCP"))?;
        Ok((id.clone(), DiscoveredMcpDefinition {
            definition: ModelToolDefinitionV1 { capability_id: id.clone(), name: mcp_provider_name(server_id, name),
                description: if tool.description.is_empty() { format!("Call MCP tool '{name}'.") } else { tool.description.clone() },
                input_schema: tool.input_schema.clone() },
            annotations: tool.annotations.clone(),
        }))
    }).collect()
}
