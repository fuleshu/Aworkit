//! Resolve Agent-level MCP server selections before freezing a Chat.
//! Saved workflows keep `mcp:<server>`; execution snapshots contain exact tools.
use super::*;

pub(super) fn expand_server_selections(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
) -> Result<Value, String> {
    let mut resolved = workflow.clone();
    let Some(nodes) = resolved.get_mut("nodes").and_then(Value::as_array_mut) else {
        return Err("workflow nodes are missing".into());
    };
    for node in nodes {
        if node.get("type").and_then(Value::as_str) != Some("agent") {
            continue;
        }
        let Some(ids) = node
            .get_mut("configuration")
            .and_then(|config| config.get_mut("toolIds"))
            .and_then(Value::as_array_mut)
        else {
            continue; // The executable catalog validator reports malformed nodes.
        };
        let mut expanded = Vec::new();
        let mut seen = BTreeSet::new();
        for id in ids.iter() {
            let id = id.as_str().ok_or("Agent tool selections must be strings")?;
            let candidates = if id.starts_with("mcp:") && !id.starts_with(MCP_CAPABILITY_PREFIX) {
                let server_id = &id[4..];
                let server = settings
                    .mcp_servers
                    .iter()
                    .find(|server| server.id == server_id)
                    .ok_or_else(|| format!("MCP server '{server_id}' is missing from Settings"))?;
                if !server.enabled {
                    return Err(format!(
                        "MCP server '{}' is disabled in Settings → MCP",
                        server.name
                    ));
                }
                if server.tools.is_empty() {
                    return Err(format!(
                        "MCP server '{}' needs setup. Connect and enable it in Settings → MCP",
                        server.name
                    ));
                }
                let tools: Vec<_> = server
                    .tools
                    .iter()
                    .filter(|tool| tool.enabled)
                    .map(|tool| format!("mcp://{server_id}/{}", tool.name))
                    .collect();
                if tools.is_empty() {
                    return Err(format!(
                        "MCP server '{}' has no enabled functions in Settings → MCP",
                        server.name
                    ));
                }
                tools
            } else {
                vec![id.to_owned()]
            };
            for candidate in candidates {
                if seen.insert(candidate.clone()) {
                    expanded.push(Value::String(candidate));
                }
            }
        }
        *ids = expanded;
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests;
