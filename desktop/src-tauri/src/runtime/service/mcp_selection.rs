//! Resolve portable MCP names to exact local tool identities for one pass.
use super::*;
use crate::runtime::workflow_capabilities::{server, warn};

pub(super) fn expand_server_selections(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
) -> Result<Value, String> {
    let mut resolved = workflow.clone();
    let mut warnings = Vec::new();
    let nodes = resolved
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .ok_or("workflow nodes are missing")?;
    for node in nodes {
        let kind = node
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if kind == "agent" {
            if let Some(ids) = node
                .get_mut("configuration")
                .and_then(|c| c.get_mut("toolIds"))
                .and_then(Value::as_array_mut)
            {
                let mut expanded = Vec::new();
                for id in ids.iter() {
                    // A selection this build cannot read is dropped with a
                    // notice, never a reason to end the pass.
                    let Some(id) = id.as_str() else {
                        warnings.push(
                            "an Agent tool selection is not a string; the pass continues without it"
                                .to_owned(),
                        );
                        continue;
                    };
                    match resolve(id, settings) {
                        Ok(candidates) => {
                            for candidate in candidates {
                                if !expanded.contains(&json!(candidate)) {
                                    expanded.push(json!(candidate));
                                }
                            }
                        }
                        Err(warning) => warnings.push(warning),
                    }
                }
                *ids = expanded;
            }
        } else if kind == "tool" {
            if let Some(id) = node
                .get_mut("configuration")
                .and_then(|c| c.get_mut("toolId"))
            {
                if let Some(text) = id.as_str() {
                    match resolve(text, settings) {
                        Ok(ids) if ids.len() == 1 => *id = json!(ids[0]),
                        Ok(_) => warnings
                            .push(format!("Tool node requires an individual tool: '{text}'")),
                        Err(warning) => warnings.push(warning),
                    }
                }
            }
        }
    }
    for warning in warnings {
        warn(&mut resolved, warning);
    }
    Ok(resolved)
}

fn resolve(id: &str, settings: &SettingsConfigurationV2) -> Result<Vec<String>, String> {
    if !id.starts_with("mcp:") {
        return Ok(vec![id.into()]);
    }
    let (name, tool) = if id.starts_with(MCP_CAPABILITY_PREFIX) {
        let (name, tool) = split_mcp_capability(id)?;
        (name, Some(tool))
    } else {
        (&id[4..], None)
    };
    let server = server(settings, name, tool.is_some())?;
    if !server.enabled {
        return Err(format!(
            "MCP server '{}' is disabled in Settings → MCP",
            server.name
        ));
    }
    if let Some(tool) = tool {
        if !server.tools.iter().any(|t| t.name == tool && t.enabled) {
            return Err(format!(
                "MCP tool '{}/{tool}' is missing or disabled in Settings → MCP",
                server.name
            ));
        }
        return Ok(vec![format!("mcp://{}/{tool}", server.id)]);
    }
    let ids: Vec<_> = server
        .tools
        .iter()
        .filter(|t| t.enabled)
        .map(|t| format!("mcp://{}/{}", server.id, t.name))
        .collect();
    if ids.is_empty() {
        return Err(format!(
            "MCP server '{}' has no enabled functions; connect and enable it in Settings → MCP",
            server.name
        ));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests;
