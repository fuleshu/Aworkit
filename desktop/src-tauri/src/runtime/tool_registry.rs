//! Declarative tool discovery and frozen, user-editable tool behavior.
//!
//! Manifests describe plugins; native executors remain the authority boundary.
//! External executable/server tools use the existing MCP transport and broker.

use super::{approvals::ApprovalMode, settings_v2::BuiltInToolConfigurationV2};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const BUNDLED: &str = include_str!("../../../tool-plugins/aworkit-native/tool-plugin.json");

pub mod discovery;
mod persona;
#[cfg(test)]
mod tests;
pub(crate) use persona::migrate_persona;

/// User overrides, frozen along with the tool rather than read during a Run.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<ApprovalMode>,
}

/// Saved discovery plus user choices. Live execution revalidates the server schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolConfiguration {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "ToolOptions::is_default")]
    pub options: ToolOptions,
}

pub fn validate_mcp_catalog(tools: &[McpToolConfiguration]) -> Result<(), String> {
    if tools.len() > 2048 {
        return Err("MCP catalog has more than 2048 tools".into());
    }
    let mut names = BTreeSet::new();
    for tool in tools {
        if tool.name.is_empty()
            || tool.name.len() > 256
            || tool.name.contains('/')
            || tool.name.contains('\0')
            || !names.insert(&tool.name)
            || tool.description.len() > 32768
            || !tool.input_schema.is_object()
            || tool.input_schema.to_string().len() > 512 * 1024
        {
            return Err("Invalid MCP tool discovery catalog".into());
        }
        tool.options.validate("mcp")?;
    }
    Ok(())
}

impl ToolOptions {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self, execution: &str) -> Result<(), String> {
        if self
            .instructions
            .as_ref()
            .is_some_and(|s| s.len() > 32 * 1024 || s.contains('\0'))
        {
            return Err(
                "Tool instructions must be at most 32 KiB and contain no NUL characters".into(),
            );
        }
        if let Some(path) = &self.executable {
            if !matches!(execution, "shell" | "python")
                || path.len() > 4096
                || path.contains('\0')
                || !std::path::Path::new(path).is_absolute()
            {
                return Err(
                    "Tool executable must be an absolute path for a shell or Python executor"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

/// A field's form metadata. Values are validated again by its executor.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolField {
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: String,
    pub read_only: bool,
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTool {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub executor: String,
    pub provider_name: String,
    pub input_schema: Value,
    pub execution: String,
    pub requires_project: bool,
    pub configuration: BTreeMap<String, Value>,
    pub fields: Vec<ToolField>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeToolPlugin {
    pub schema_version: u16,
    pub id: String,
    pub name: String,
    pub version: String,
    pub tools: Vec<NativeTool>,
}

impl NativeToolPlugin {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 1024 * 1024 {
            return Err("Tool manifest exceeds 1 MiB".into());
        }
        let manifest: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if manifest.schema_version != 1
            || manifest.tools.is_empty()
            || manifest.tools.len() > 256
            || manifest.id.is_empty()
            || manifest.name.is_empty()
            || manifest.version.is_empty()
        {
            return Err("Invalid tool plugin identity, version or tool count".into());
        }
        let mut ids = BTreeSet::new();
        for tool in &manifest.tools {
            if !ids.insert(&tool.id)
                || tool.id != format!("tool.{}", tool.executor)
                || tool.name.is_empty()
                || tool.description.is_empty()
                || tool.provider_name.is_empty()
                || !tool.input_schema.is_object()
                || !matches!(tool.execution.as_str(), "native" | "shell" | "python")
            {
                return Err(format!("Invalid or duplicate tool '{}'", tool.id));
            }
            ToolOptions {
                instructions: Some(tool.instructions.clone()),
                ..Default::default()
            }
            .validate(&tool.execution)?;
            let mut fields = BTreeSet::new();
            for field in &tool.fields {
                if !fields.insert(&field.key)
                    || !tool.configuration.contains_key(&field.key)
                    || field.label.is_empty()
                    || field.help.is_empty()
                    || !matches!(
                        field.kind.as_str(),
                        "boolean" | "integer" | "string" | "list"
                    )
                    || field.minimum.zip(field.maximum).is_some_and(|(a, b)| a > b)
                {
                    return Err(format!("Invalid setting '{}' for '{}'", field.key, tool.id));
                }
                let _ = field.read_only;
            }
            if fields.len() != tool.configuration.len() {
                return Err(format!("Missing setting metadata for '{}'", tool.id));
            }
        }
        Ok(manifest)
    }
}

pub fn native_plugin() -> &'static NativeToolPlugin {
    static PLUGIN: OnceLock<NativeToolPlugin> = OnceLock::new();
    PLUGIN.get_or_init(|| NativeToolPlugin::parse(BUNDLED).expect("bundled tool plugin is valid"))
}

pub fn native_tool(id: &str) -> Option<&'static NativeTool> {
    native_plugin().tools.iter().find(|tool| tool.id == id)
}

pub fn native_defaults() -> Vec<BuiltInToolConfigurationV2> {
    native_plugin()
        .tools
        .iter()
        .map(|tool| BuiltInToolConfigurationV2 {
            id: tool.id.clone(),
            name: tool.name.clone(),
            enabled: false,
            requires_project: tool.requires_project,
            credential_bindings: Vec::new(),
            configuration: tool.configuration.clone(),
            options: ToolOptions::default(),
        })
        .collect()
}

/// Resolves current manifest defaults only when starting a new Run.
pub(crate) fn freeze_settings(
    tool: &BuiltInToolConfigurationV2,
) -> Result<BuiltInToolConfigurationV2, String> {
    let mut frozen = tool.clone();
    if frozen.options.instructions.is_none() {
        frozen.options.instructions = native_tool(&tool.id).map(|entry| entry.instructions.clone());
    }
    if matches!(tool.id.as_str(), "tool.shell.host" | "tool.python.host") {
        frozen.options.executable = Some(super::tool_loop::resolve_tool_executable(
            &tool.id,
            frozen.options.executable.as_deref(),
        )?);
    }
    Ok(frozen)
}

/// Adds only the selected tools' frozen instructions, in stable identifier order.
pub(crate) fn instruction_block<'a>(
    tools: impl IntoIterator<Item = (&'a str, &'a ToolOptions)>,
) -> String {
    let sections: BTreeMap<_, _> = tools
        .into_iter()
        .filter_map(|(id, options)| {
            options
                .instructions
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(|text| (id, text))
        })
        .collect();
    sections
        .into_iter()
        .map(|(id, text)| format!("Tool instructions for {id}:\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}
