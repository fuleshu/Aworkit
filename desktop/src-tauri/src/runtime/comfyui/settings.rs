//! Persisted ComfyUI configuration: the Settings section and its validation.
//!
//! The section is deliberately data-only. It records which ComfyUI server
//! Aworkit talks to, where the local installation lives, which API workflows
//! become agent tools, and which workflow folder the authoring assistant may
//! read. Every workflow parameter carries the exact `(node id, input name)` it
//! writes, so a saved tool can only touch an input that the workflow contains.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use aworkit_capability_host::DEFAULT_COMFYUI_ENDPOINT;

use super::tools::COMFYUI_RESERVED_TOOL_IDS;

/// Bounds that keep one settings document reviewable and bounded.
const MAXIMUM_WORKFLOW_TOOLS: usize = 256;
const MAXIMUM_PARAMETERS_PER_TOOL: usize = 128;
const MAXIMUM_ID_BYTES: usize = 128;
const MAXIMUM_NAME_BYTES: usize = 256;
const MAXIMUM_DESCRIPTION_BYTES: usize = 2_000;
const MAXIMUM_PATH_BYTES: usize = 4_096;
const MAXIMUM_LAUNCH_ARGUMENTS: usize = 32;
const MAXIMUM_LAUNCH_ARGUMENT_BYTES: usize = 1_024;
const MAXIMUM_CHOICES: usize = 512;

/// ComfyUI settings section.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiConfigurationV2 {
    /// ComfyUI base URL. The default is the conventional local server.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Local ComfyUI installation folder, when one is available.
    #[serde(default)]
    pub install_path: Option<String>,
    /// Arguments appended to the launch command in that folder.
    #[serde(default = "default_launch_arguments")]
    pub launch_arguments: Vec<String>,
    /// Whether Aworkit may start the local server on request.
    #[serde(default)]
    pub auto_start: bool,
    /// Folder the authoring assistant reads and writes API workflows in.
    #[serde(default)]
    pub workflow_folder: Option<String>,
    /// One agent tool per configured ComfyUI API workflow.
    #[serde(default)]
    pub workflow_tools: Vec<ComfyUiWorkflowToolV2>,
    /// Reference to the retired Python bridge's MCP server.
    ///
    /// Documents written while the bridge existed still carry this key, and
    /// they must keep loading: the section refuses unknown keys, so dropping
    /// the field outright made every such profile fail to decode its Settings
    /// document and therefore fail to open at all. Native workflow tools own
    /// the server connection now, so the value is accepted and discarded, and
    /// the next write drops the key.
    #[serde(default, skip_serializing)]
    pub bridge_server_id: Option<String>,
}

fn default_endpoint() -> String {
    DEFAULT_COMFYUI_ENDPOINT.to_owned()
}

fn default_launch_arguments() -> Vec<String> {
    vec!["main.py".to_owned(), "--listen".to_owned(), "127.0.0.1".to_owned()]
}

impl Default for ComfyUiConfigurationV2 {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            install_path: None,
            launch_arguments: default_launch_arguments(),
            auto_start: false,
            workflow_folder: None,
            workflow_tools: Vec::new(),
            bridge_server_id: None,
        }
    }
}

impl ComfyUiConfigurationV2 {
    /// Validates the section and normalizes the endpoint and folder paths.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason for the first invalid field.
    pub fn validate(&mut self) -> Result<(), String> {
        self.endpoint =
            aworkit_capability_host::validate_comfyui_endpoint(&self.endpoint).map_err(|error| {
                format!("ComfyUI endpoint is invalid: {error}")
            })?;
        for (field, value) in [
            ("installPath", self.install_path.as_deref()),
            ("workflowFolder", self.workflow_folder.as_deref()),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() || value.len() > MAXIMUM_PATH_BYTES || value.contains('\0')
                {
                    return Err(format!("ComfyUI {field} is not a usable path"));
                }
            }
        }
        if self.launch_arguments.len() > MAXIMUM_LAUNCH_ARGUMENTS
            || self
                .launch_arguments
                .iter()
                .any(|argument| argument.len() > MAXIMUM_LAUNCH_ARGUMENT_BYTES || argument.contains('\0'))
        {
            return Err("ComfyUI launch arguments are outside the allowed bounds".into());
        }
        if self.workflow_tools.len() > MAXIMUM_WORKFLOW_TOOLS {
            return Err(format!(
                "at most {MAXIMUM_WORKFLOW_TOOLS} ComfyUI workflow tools may be configured"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for tool in &mut self.workflow_tools {
            tool.validate()?;
            if !seen.insert(tool.id.clone()) {
                return Err(format!("ComfyUI workflow tool id '{}' is duplicated", tool.id));
            }
        }
        Ok(())
    }
}

/// One ComfyUI API workflow exposed to the agent as a tool.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiWorkflowToolV2 {
    /// Stable tool id. The bridge derives the MCP tool name from it.
    pub id: String,
    /// Display name shown in Settings and to the model.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// Absolute path of the API-format workflow JSON.
    pub workflow_path: String,
    /// Whether the tool is exposed to the agent.
    pub enabled: bool,
    /// Ordered parameters the model may set.
    #[serde(default)]
    pub parameters: Vec<ComfyUiToolParameterV2>,
}

impl ComfyUiWorkflowToolV2 {
    /// Validates this tool.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason for the first invalid field.
    pub fn validate(&mut self) -> Result<(), String> {
        validate_identifier(&self.id, "workflow tool id")?;
        if COMFYUI_RESERVED_TOOL_IDS.contains(&self.id.as_str()) {
            return Err(format!(
                "ComfyUI workflow tool id '{}' is reserved by an authoring helper",
                self.id
            ));
        }
        if self.name.trim().is_empty() || self.name.len() > MAXIMUM_NAME_BYTES {
            return Err(format!(
                "ComfyUI workflow tool '{}' needs a bounded name",
                self.id
            ));
        }
        if self.description.len() > MAXIMUM_DESCRIPTION_BYTES {
            return Err(format!(
                "ComfyUI workflow tool '{}' description is too long",
                self.id
            ));
        }
        if self.workflow_path.trim().is_empty()
            || self.workflow_path.len() > MAXIMUM_PATH_BYTES
            || self.workflow_path.contains('\0')
        {
            return Err(format!(
                "ComfyUI workflow tool '{}' needs a workflow file path",
                self.id
            ));
        }
        if self.parameters.len() > MAXIMUM_PARAMETERS_PER_TOOL {
            return Err(format!(
                "ComfyUI workflow tool '{}' has too many parameters",
                self.id
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for parameter in &mut self.parameters {
            parameter.validate(&self.id)?;
            if !seen.insert(parameter.name.clone()) {
                return Err(format!(
                    "ComfyUI workflow tool '{}' declares parameter '{}' twice",
                    self.id, parameter.name
                ));
            }
        }
        Ok(())
    }
}

/// JSON types a workflow parameter may declare.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComfyUiParameterKindV2 {
    /// Free-form text.
    String,
    /// Whole number.
    Integer,
    /// Fractional number.
    Number,
    /// Boolean switch.
    Boolean,
}

impl ComfyUiParameterKindV2 {
    /// The JSON-schema type this parameter declares.
    #[must_use]
    pub fn json_schema_type(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
        }
    }
}

/// One typed parameter of a ComfyUI workflow tool.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiToolParameterV2 {
    /// Parameter name used by the model.
    pub name: String,
    /// What the parameter changes, written for the model.
    #[serde(default)]
    pub description: String,
    /// Declared JSON type.
    pub value_kind: ComfyUiParameterKindV2,
    /// Whether the model must supply it.
    #[serde(default)]
    pub required: bool,
    /// Optional default value.
    #[serde(default)]
    pub default_value: Option<Value>,
    /// Node id the value is written to.
    pub node_id: String,
    /// Input name inside that node.
    pub input_name: String,
    /// Closed choice set, when the workflow reports one.
    #[serde(default)]
    pub choices: Vec<Value>,
}

impl ComfyUiToolParameterV2 {
    /// Validates this parameter against its own bounds and its tool identity.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason for the first invalid field.
    pub fn validate(&mut self, tool_id: &str) -> Result<(), String> {
        if !valid_parameter_name(&self.name) {
            return Err(format!(
                "ComfyUI workflow tool '{tool_id}' has an invalid parameter name '{}'",
                self.name
            ));
        }
        if self.description.len() > MAXIMUM_DESCRIPTION_BYTES {
            return Err(format!(
                "ComfyUI parameter '{}' of '{tool_id}' has too long a description",
                self.name
            ));
        }
        if self.node_id.trim().is_empty()
            || self.input_name.trim().is_empty()
            || self.node_id.len() > MAXIMUM_NAME_BYTES
            || self.input_name.len() > MAXIMUM_NAME_BYTES
        {
            return Err(format!(
                "ComfyUI parameter '{}' of '{tool_id}' must bind a node id and an input name",
                self.name
            ));
        }
        if self.choices.len() > MAXIMUM_CHOICES {
            return Err(format!(
                "ComfyUI parameter '{}' of '{tool_id}' has too many allowed values",
                self.name
            ));
        }
        if let Some(default) = &self.default_value {
            if !value_matches_kind(default, self.value_kind) {
                return Err(format!(
                    "ComfyUI parameter '{}' of '{tool_id}' has a default that does not match its type",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

/// Whether a JSON value is a legal instance of a parameter kind.
#[must_use]
pub fn value_matches_kind(value: &Value, kind: ComfyUiParameterKindV2) -> bool {
    match kind {
        ComfyUiParameterKindV2::String => value.is_string(),
        ComfyUiParameterKindV2::Integer => {
            value.as_i64().is_some() || value.as_u64().is_some()
        }
        ComfyUiParameterKindV2::Number => value.is_number(),
        ComfyUiParameterKindV2::Boolean => value.is_boolean(),
    }
}

/// Whether a value is one of a parameter's declared choices when it has any.
#[must_use]
pub fn value_in_choices(value: &Value, choices: &[Value]) -> bool {
    choices.is_empty() || choices.contains(value)
}

/// Validates a stable identifier used by a tool id or server id.
///
/// # Errors
///
/// Returns a human-readable reason when the identifier is unusable.
pub fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let valid = chars
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        && value.len() <= MAXIMUM_ID_BYTES;
    if valid {
        Ok(())
    } else {
        Err(format!("ComfyUI {field} '{value}' is not a valid identifier"))
    }
}

/// Parameter names are model-facing JSON keys, so they use a strict rule.
#[must_use]
pub fn valid_parameter_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
        && value.len() <= 64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parameter(name: &str) -> ComfyUiToolParameterV2 {
        ComfyUiToolParameterV2 {
            name: name.into(),
            description: "What it changes".into(),
            value_kind: ComfyUiParameterKindV2::String,
            required: true,
            default_value: None,
            node_id: "6".into(),
            input_name: "text".into(),
            choices: Vec::new(),
        }
    }

    fn tool(id: &str) -> ComfyUiWorkflowToolV2 {
        ComfyUiWorkflowToolV2 {
            id: id.into(),
            name: "Image".into(),
            description: "Generates an image".into(),
            workflow_path: "D:\\workflows\\image.json".into(),
            enabled: true,
            parameters: vec![parameter("prompt")],
        }
    }

    #[test]
    fn defaults_are_valid_and_normalize_the_endpoint() {
        let mut configuration = ComfyUiConfigurationV2::default();
        assert_eq!(configuration.endpoint, DEFAULT_COMFYUI_ENDPOINT);
        configuration.validate().unwrap();
        assert_eq!(configuration.endpoint, "http://127.0.0.1:8188/");
        assert!(!configuration.auto_start);
    }

    #[test]
    fn a_document_from_the_python_bridge_build_still_loads_and_drops_the_retired_key() {
        // The exact section a build with the Python bridge persisted. The
        // section refuses unknown keys, so the retired reference must be
        // accepted rather than failing the whole Settings document.
        let persisted = serde_json::json!({
            "endpoint": "http://127.0.0.1:8188/",
            "installPath": null,
            "launchArguments": ["main.py", "--listen", "127.0.0.1"],
            "autoStart": false,
            "workflowFolder": null,
            "workflowTools": [],
            "bridgeServerId": "comfyui.bridge",
        });
        let mut configuration: ComfyUiConfigurationV2 = serde_json::from_value(persisted)
            .expect("a Settings document written with the bridge must keep loading");
        assert_eq!(
            configuration.bridge_server_id.as_deref(),
            Some("comfyui.bridge")
        );
        configuration.validate().unwrap();
        let written = serde_json::to_value(&configuration).unwrap();
        assert!(
            written.get("bridgeServerId").is_none(),
            "the retired key must not be written back"
        );

        // A document that recorded no bridge server is equally readable.
        let mut absent = serde_json::json!({ "endpoint": "http://127.0.0.1:8188/" });
        absent["bridgeServerId"] = serde_json::Value::Null;
        let configuration: ComfyUiConfigurationV2 = serde_json::from_value(absent).unwrap();
        assert_eq!(configuration.bridge_server_id, None);
    }

    #[test]
    fn duplicate_tool_ids_and_parameter_names_fail() {
        let mut configuration = ComfyUiConfigurationV2 {
            workflow_tools: vec![tool("image"), tool("image")],
            ..ComfyUiConfigurationV2::default()
        };
        assert!(configuration.validate().unwrap_err().contains("duplicated"));

        let mut duplicate_parameter = tool("image");
        duplicate_parameter.parameters.push(parameter("prompt"));
        let mut configuration = ComfyUiConfigurationV2 {
            workflow_tools: vec![duplicate_parameter],
            ..ComfyUiConfigurationV2::default()
        };
        assert!(configuration.validate().unwrap_err().contains("twice"));
    }

    #[test]
    fn parameter_names_and_defaults_are_checked() {
        let mut bad_name = tool("image");
        bad_name.parameters[0].name = "prompt text".into();
        let mut configuration = ComfyUiConfigurationV2 {
            workflow_tools: vec![bad_name],
            ..ComfyUiConfigurationV2::default()
        };
        assert!(configuration.validate().is_err());

        let mut bad_default = tool("image");
        bad_default.parameters[0].default_value = Some(json!(4));
        let mut configuration = ComfyUiConfigurationV2 {
            workflow_tools: vec![bad_default],
            ..ComfyUiConfigurationV2::default()
        };
        assert!(configuration.validate().unwrap_err().contains("default"));
    }

    #[test]
    fn choices_and_types_are_enforced() {
        assert!(value_in_choices(&json!("euler"), &[]));
        assert!(value_in_choices(&json!("euler"), &[json!("euler")]));
        assert!(!value_in_choices(&json!("other"), &[json!("euler")]));
        assert!(value_matches_kind(&json!(3), ComfyUiParameterKindV2::Integer));
        assert!(!value_matches_kind(&json!(3.5), ComfyUiParameterKindV2::Integer));
        assert!(value_matches_kind(&json!(3.5), ComfyUiParameterKindV2::Number));
    }
}
