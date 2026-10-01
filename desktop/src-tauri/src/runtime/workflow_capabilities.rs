//! Portable authored MCP names and per-pass unavailable-capability notices.
use super::settings_v2::{McpServerConfigurationV2, SettingsConfigurationV2};
use serde_json::{Value, json};

pub(crate) fn server<'a>(
    settings: &'a SettingsConfigurationV2,
    name: &str,
    uri_encoded: bool,
) -> Result<&'a McpServerConfigurationV2, String> {
    let decoded = if uri_encoded { decode(name) } else { name.to_owned() };
    let matches: Vec<_> = settings
        .mcp_servers
        .iter()
        .filter(|s| s.name == decoded)
        .collect();
    match matches.as_slice() {
        [found] => Ok(found),
        [] => settings
            .mcp_servers
            .iter()
            .find(|s| s.id == name)
            .ok_or_else(|| format!("MCP server '{decoded}' is missing from Settings")),
        _ => Err(format!(
            "MCP server name '{decoded}' is ambiguous in Settings"
        )),
    }
}

fn encode(name: &str) -> String {
    name.as_bytes()
        .iter()
        .map(|b| {
            // Match the editor's encodeURIComponent representation exactly.
            if b.is_ascii_alphanumeric() || b"-_.~!*'()".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn decode(name: &str) -> String {
    let mut bytes = Vec::new();
    let source = name.as_bytes();
    let mut index = 0;
    while index < source.len() {
        if source[index] == b'%' && index + 2 < source.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&source[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                bytes.push(byte);
                index += 3;
                continue;
            }
        }
        bytes.push(source[index]);
        index += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|_| name.to_owned())
}

pub(crate) fn portable_reference(id: &str, settings: &SettingsConfigurationV2) -> String {
    if let Some(rest) = id.strip_prefix("mcp://") {
        if let Some((name, tool)) = rest.split_once('/') {
            if let Ok(server) = server(settings, name, true) {
                return format!("mcp://{}/{}", encode(&server.name), tool);
            }
        }
    } else if let Some(name) = id.strip_prefix("mcp:") {
        if let Ok(server) = server(settings, name, false) {
            return format!("mcp:{}", server.name);
        }
    }
    id.to_owned()
}

/// Only capability-bearing node fields are transformed; opaque extensions survive.
pub(crate) fn portable_document(document: &Value, settings: &SettingsConfigurationV2) -> Value {
    let mut document = document.clone();
    if let Some(nodes) = document.get_mut("nodes").and_then(Value::as_array_mut) {
        for node in nodes {
            let kind = node
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Some(config) = node.get_mut("configuration") {
                if kind == "agent" {
                    if let Some(ids) = config.get_mut("toolIds").and_then(Value::as_array_mut) {
                        for id in ids {
                            if let Some(text) = id.as_str() {
                                *id = json!(portable_reference(text, settings));
                            }
                        }
                    }
                } else if kind == "tool" {
                    if let Some(id) = config.get_mut("toolId") {
                        if let Some(text) = id.as_str() {
                            *id = json!(portable_reference(text, settings));
                        }
                    }
                }
            }
        }
    }
    document
}

pub(crate) fn warn(document: &mut Value, message: impl Into<String>) {
    let message = message.into();
    if document.get("capabilityWarnings").is_none() {
        document["capabilityWarnings"] = json!([]);
    }
    if let Some(warnings) = document["capabilityWarnings"].as_array_mut() {
        if !warnings.iter().any(|w| w.as_str() == Some(&message)) {
            warnings.push(json!(message));
        }
    }
}

/// Prune only Agent tool offers. Tool nodes retain their position and report an
/// unavailable result so their downstream edges can still execute.
pub(crate) fn retain_available(document: &mut Value, available: &[String]) {
    if let Some(nodes) = document.get_mut("nodes").and_then(Value::as_array_mut) {
        for node in nodes {
            let kind = node
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if kind == "agent" {
                if let Some(ids) = node["configuration"]["toolIds"].as_array_mut() {
                    ids.retain(|id| {
                        id.as_str()
                            .is_some_and(|id| available.iter().any(|a| a == id))
                    });
                }
            }
        }
    }
}
