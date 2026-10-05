//! Native agent tools derived from the saved ComfyUI settings section.
//!
//! Each enabled workflow tool in Settings becomes one native Aworkit tool with a
//! capability id of the form `comfyui.<tool id>`. The model-facing definition —
//! display name, description and JSON input schema — is generated here from the
//! typed parameter list the Settings tab edits, so a workflow tool is a
//! first-class named tool rather than a function of a bridge server.
//!
//! The schema and the parameter bindings are produced from one source, so what
//! the model is offered and what execution writes cannot drift apart.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::settings::{ComfyUiConfigurationV2, ComfyUiWorkflowToolV2};

/// Prefix that marks a capability id as a ComfyUI capability.
pub const COMFYUI_CAPABILITY_PREFIX: &str = "comfyui.";
/// Scope every ComfyUI workflow tool is admitted under.
pub const COMFYUI_RUN_SCOPE: &str = "comfyui.run";
/// Scope the read-only ComfyUI authoring helpers are admitted under.
pub const COMFYUI_AUTHORING_SCOPE: &str = "comfyui.author";
/// Adapter identity every native ComfyUI capability is brokered under.
pub const COMFYUI_ADAPTER_ID: &str = "adapter.comfyui.v1";
/// Adapter version of the native ComfyUI tool surface.
pub const COMFYUI_ADAPTER_VERSION: &str = "1.0.0";

/// Read-only node-type catalog helper: searches the live `/object_info` catalog.
pub const COMFYUI_NODE_TYPES_CAPABILITY_ID: &str = "comfyui.list_node_types";
/// Read-only workflow reader helper: returns a configured tool's API graph.
pub const COMFYUI_GET_WORKFLOW_CAPABILITY_ID: &str = "comfyui.get_workflow";
/// Model-facing name of the node-type catalog helper.
pub const COMFYUI_NODE_TYPES_PROVIDER_NAME: &str = "comfyui_list_node_types";
/// Model-facing name of the workflow reader helper.
pub const COMFYUI_GET_WORKFLOW_PROVIDER_NAME: &str = "comfyui_get_workflow";
/// Workflow tool ids reserved by the authoring helpers.
pub const COMFYUI_RESERVED_TOOL_IDS: [&str; 2] = ["list_node_types", "get_workflow"];

/// The capability id of one configured workflow tool.
#[must_use]
pub fn comfyui_capability_id(tool_id: &str) -> String {
    format!("{COMFYUI_CAPABILITY_PREFIX}{tool_id}")
}

/// The model-facing provider name of one configured workflow tool.
///
/// A workflow tool's display name is authored in Settings and is meant for the
/// user, so it may contain spaces, parentheses and punctuation. The provider
/// contract for a function name is a non-empty name of at most 64 bytes drawn
/// from `[A-Za-z0-9_-]`, so the model-facing name is generated from the stable
/// tool id through the same sanitiser the MCP function names use. Handing the
/// display name to a provider invalidates every request that carries it.
#[must_use]
pub fn comfyui_provider_name(tool_id: &str) -> String {
    super::super::mcp_tools::mcp_provider_name("comfyui", "comfyui", tool_id)
}

/// The workflow tool id inside a ComfyUI capability id.
///
/// Returns `None` for any id that is not a ComfyUI workflow tool, so callers can
/// route by prefix without guessing. The two authoring helper ids share the
/// prefix but are not workflow tools, so they are refused here too.
#[must_use]
pub fn comfyui_tool_id(capability_id: &str) -> Option<&str> {
    if is_comfyui_authoring_capability(capability_id) {
        return None;
    }
    capability_id
        .strip_prefix(COMFYUI_CAPABILITY_PREFIX)
        .filter(|tool_id| !tool_id.is_empty())
}

/// Whether a capability id belongs to a ComfyUI workflow tool.
#[must_use]
pub fn is_comfyui_capability(capability_id: &str) -> bool {
    comfyui_tool_id(capability_id).is_some()
}

/// Whether a capability id is one of the read-only authoring helpers.
#[must_use]
pub fn is_comfyui_authoring_capability(capability_id: &str) -> bool {
    matches!(
        capability_id,
        COMFYUI_NODE_TYPES_CAPABILITY_ID | COMFYUI_GET_WORKFLOW_CAPABILITY_ID
    )
}

/// Whether a capability id belongs to one Settings-derived workflow tool.
#[must_use]
pub fn is_comfyui_workflow_capability(capability_id: &str) -> bool {
    is_comfyui_capability(capability_id)
}

/// The model-facing name of one authoring helper.
#[must_use]
pub fn comfyui_authoring_provider_name(capability_id: &str) -> Option<&'static str> {
    match capability_id {
        COMFYUI_NODE_TYPES_CAPABILITY_ID => Some(COMFYUI_NODE_TYPES_PROVIDER_NAME),
        COMFYUI_GET_WORKFLOW_CAPABILITY_ID => Some(COMFYUI_GET_WORKFLOW_PROVIDER_NAME),
        _ => None,
    }
}

/// The model-facing description of one authoring helper.
#[must_use]
pub fn comfyui_authoring_description(capability_id: &str) -> Option<&'static str> {
    match capability_id {
        COMFYUI_NODE_TYPES_CAPABILITY_ID => Some(
            "Search the live ComfyUI node type catalog and return a bounded compact list of node types, categories and input names. Use it while authoring workflow parameter bindings.",
        ),
        COMFYUI_GET_WORKFLOW_CAPABILITY_ID => Some(
            "Read the API graph of one configured ComfyUI workflow tool, so its node ids and input names can be inspected before a parameter binding is proposed. Read-only.",
        ),
        _ => None,
    }
}

/// The JSON input schema of one authoring helper.
#[must_use]
pub fn comfyui_authoring_schema(capability_id: &str) -> Value {
    match capability_id {
        COMFYUI_NODE_TYPES_CAPABILITY_ID => json!({
            "type": "object",
            "properties": {
                "search": {
                    "type": "string",
                    "description": "Optional text matched across node type, category and input names."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 50
                }
            },
            "additionalProperties": false
        }),
        COMFYUI_GET_WORKFLOW_CAPABILITY_ID => json!({
            "type": "object",
            "properties": {
                "workflow": {
                    "type": "string",
                    "description": "Id of a configured ComfyUI workflow tool."
                }
            },
            "required": ["workflow"],
            "additionalProperties": false
        }),
        _ => json!({"type": "object", "properties": {}, "additionalProperties": false}),
    }
}

/// The frozen configuration of one authoring helper: the endpoint it reads and,
/// for the workflow reader, the exact `(workflow tool id -> workflow path)`
/// table resolved from Settings.
#[must_use]
pub fn comfyui_authoring_configuration(
    capability_id: &str,
    endpoint: &str,
    workflow_tools: &[ComfyUiWorkflowToolV2],
) -> BTreeMap<String, Value> {
    let mut configuration = BTreeMap::from([("endpoint".to_owned(), json!(endpoint))]);
    if capability_id == COMFYUI_GET_WORKFLOW_CAPABILITY_ID {
        let workflows = workflow_tools
            .iter()
            .map(|tool| (tool.id.clone(), json!(tool.workflow_path)))
            .collect::<Map<String, Value>>();
        configuration.insert("workflows".to_owned(), Value::Object(workflows));
    }
    configuration
}

/// One ComfyUI workflow tool resolved for execution.
///
/// It carries the generated model-facing definition and the exact parameter
/// bindings execution needs, so freeze and dispatch never re-derive them
/// differently.
#[derive(Clone, Debug, PartialEq)]
pub struct ComfyUiNativeTool {
    /// Capability id, `comfyui.<tool id>`.
    pub capability_id: String,
    /// Model-facing tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// Generated JSON input schema for the parameter list.
    pub input_schema: Value,
    /// The configured workflow tool this capability executes.
    pub tool: ComfyUiWorkflowToolV2,
}

/// Every enabled workflow tool as a native tool, in Settings order.
#[must_use]
pub fn comfyui_native_tools(configuration: &ComfyUiConfigurationV2) -> Vec<ComfyUiNativeTool> {
    configuration
        .workflow_tools
        .iter()
        .filter(|tool| tool.enabled)
        .map(|tool| ComfyUiNativeTool {
            capability_id: comfyui_capability_id(&tool.id),
            name: comfyui_provider_name(&tool.id),
            description: tool.description.clone(),
            input_schema: comfyui_input_schema(tool),
            tool: tool.clone(),
        })
        .collect()
}

/// Resolves one capability id against the configured workflow tools.
///
/// A disabled tool resolves to `None`: Settings is the single enable decision.
#[must_use]
pub fn comfyui_native_tool(
    configuration: &ComfyUiConfigurationV2,
    capability_id: &str,
) -> Option<ComfyUiNativeTool> {
    let tool_id = comfyui_tool_id(capability_id)?;
    configuration
        .workflow_tools
        .iter()
        .find(|tool| tool.id == tool_id && tool.enabled)
        .map(|tool| ComfyUiNativeTool {
            capability_id: comfyui_capability_id(&tool.id),
            name: comfyui_provider_name(&tool.id),
            description: tool.description.clone(),
            input_schema: comfyui_input_schema(tool),
            tool: tool.clone(),
        })
}

/// Builds the JSON input schema for one workflow tool.
///
/// Every declared parameter becomes one property with its observed type, and an
/// optional default or closed choice set when the workflow reported one. The
/// schema refuses undeclared properties, so a model cannot smuggle a value into
/// an input the workflow does not expose.
#[must_use]
pub fn comfyui_input_schema(tool: &ComfyUiWorkflowToolV2) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for parameter in &tool.parameters {
        let mut property = Map::new();
        property.insert("type".into(), json!(parameter.value_kind.json_schema_type()));
        if !parameter.description.trim().is_empty() {
            property.insert("description".into(), json!(parameter.description));
        }
        if let Some(default) = &parameter.default_value {
            property.insert("default".into(), default.clone());
        }
        if !parameter.choices.is_empty() {
            property.insert("enum".into(), json!(parameter.choices));
        }
        if parameter.required {
            required.push(json!(parameter.name));
        }
        properties.insert(parameter.name.clone(), Value::Object(property));
    }
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert("required".into(), json!(required));
    schema.insert("additionalProperties".into(), json!(false));
    Value::Object(schema)
}

/// The frozen configuration one ComfyUI workflow tool carries into a Chat.
///
/// It records the endpoint, the workflow file and every `(node id, input name)`
/// binding, so a later Settings edit cannot change what a running Chat executes.
#[must_use]
pub fn comfyui_frozen_configuration(
    tool: &ComfyUiWorkflowToolV2,
    endpoint: &str,
) -> BTreeMap<String, Value> {
    let parameters = tool
        .parameters
        .iter()
        .map(|parameter| {
            json!({
                "name": parameter.name,
                "nodeId": parameter.node_id,
                "inputName": parameter.input_name,
                "valueKind": parameter.value_kind,
                "required": parameter.required,
                "choices": parameter.choices,
            })
        })
        .collect::<Vec<_>>();
    BTreeMap::from([
        ("endpoint".to_owned(), json!(endpoint)),
        ("workflowPath".to_owned(), json!(tool.workflow_path)),
        ("parameters".to_owned(), Value::Array(parameters)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::comfyui::settings::{
        ComfyUiParameterKindV2, ComfyUiToolParameterV2,
    };

    fn parameter(name: &str, kind: ComfyUiParameterKindV2, required: bool) -> ComfyUiToolParameterV2 {
        ComfyUiToolParameterV2 {
            name: name.into(),
            description: format!("{name} description"),
            value_kind: kind,
            required,
            default_value: None,
            node_id: "6".into(),
            input_name: name.into(),
            choices: Vec::new(),
        }
    }

    fn workflow_tool(id: &str, enabled: bool) -> ComfyUiWorkflowToolV2 {
        ComfyUiWorkflowToolV2 {
            id: id.into(),
            name: format!("{id} tool"),
            description: "Generates an image".into(),
            workflow_path: "D:\\workflows\\image.json".into(),
            enabled,
            parameters: vec![parameter("prompt", ComfyUiParameterKindV2::String, true)],
        }
    }

    fn configuration() -> ComfyUiConfigurationV2 {
        ComfyUiConfigurationV2 {
            workflow_tools: vec![workflow_tool("krea", true), workflow_tool("off", false)],
            ..ComfyUiConfigurationV2::default()
        }
    }

    #[test]
    fn a_workflow_tools_model_facing_name_satisfies_the_provider_contract() {
        // The display name is authored in Settings and may contain anything.
        // Handing it to a provider invalidated every request that carried the
        // tool, so the pass never reached its first model call.
        let mut tool = workflow_tool("krea-2-turbo-text-to-image-upscaled", true);
        tool.name = "Krea-2 Turbo Text to Image (Upscaled)".into();
        let configuration = ComfyUiConfigurationV2 {
            workflow_tools: vec![tool.clone()],
            ..ComfyUiConfigurationV2::default()
        };

        let resolved = comfyui_native_tool(&configuration, &comfyui_capability_id(&tool.id))
            .expect("the enabled tool resolves");
        assert!(
            aworkit_capability_host::provider_name_is_valid(&resolved.name),
            "model-facing name '{}' violates the provider contract",
            resolved.name
        );
        assert_eq!(resolved.name, "comfyui_krea-2-turbo-text-to-image-upscaled");

        // Every generated name holds the contract, whatever Settings authored,
        // including a name long enough that the alias must fold.
        for id in [
            "krea",
            "krea-2-turbo-text-to-image-upscaled",
            "an-extremely-long-workflow-tool-identifier-that-cannot-fit-in-64-bytes-at-all",
            "dots.and-dashes_and_underscores",
        ] {
            let name = comfyui_provider_name(id);
            assert!(
                aworkit_capability_host::provider_name_is_valid(&name),
                "'{id}' produced an invalid provider name '{name}'"
            );
        }
        // Distinct tool ids never share one provider name.
        assert_ne!(
            comfyui_provider_name("a.b"),
            comfyui_provider_name("a_b"),
            "folded ids must stay distinct"
        );
    }

    #[test]
    fn capability_ids_round_trip_and_reject_foreign_ids() {
        assert_eq!(comfyui_capability_id("krea"), "comfyui.krea");
        assert_eq!(comfyui_tool_id("comfyui.krea"), Some("krea"));
        assert!(is_comfyui_capability("comfyui.krea"));
        // A bare prefix, an MCP id and a built-in id are all refused.
        assert_eq!(comfyui_tool_id("comfyui."), None);
        assert_eq!(comfyui_tool_id("mcp://server/tool"), None);
        assert_eq!(comfyui_tool_id("tool.files.read"), None);
        assert!(!is_comfyui_capability("tool.context"));
    }

    #[test]
    fn the_authoring_helpers_share_the_prefix_but_are_not_workflow_tools() {
        // Both helpers are ComfyUI capabilities...
        assert!(is_comfyui_authoring_capability(
            COMFYUI_NODE_TYPES_CAPABILITY_ID
        ));
        assert!(is_comfyui_authoring_capability(
            COMFYUI_GET_WORKFLOW_CAPABILITY_ID
        ));
        // ...but neither resolves as a Settings-derived workflow tool, so a
        // user workflow tool can never shadow one.
        assert_eq!(comfyui_tool_id(COMFYUI_NODE_TYPES_CAPABILITY_ID), None);
        assert_eq!(comfyui_tool_id(COMFYUI_GET_WORKFLOW_CAPABILITY_ID), None);
        assert!(!is_comfyui_workflow_capability(
            COMFYUI_NODE_TYPES_CAPABILITY_ID
        ));
        assert!(!is_comfyui_capability(COMFYUI_GET_WORKFLOW_CAPABILITY_ID));
        assert_eq!(
            comfyui_authoring_provider_name(COMFYUI_NODE_TYPES_CAPABILITY_ID),
            Some(COMFYUI_NODE_TYPES_PROVIDER_NAME)
        );
        assert_eq!(
            comfyui_authoring_schema(COMFYUI_GET_WORKFLOW_CAPABILITY_ID)["required"],
            json!(["workflow"])
        );
        assert_eq!(
            comfyui_authoring_schema(COMFYUI_NODE_TYPES_CAPABILITY_ID)["properties"]["limit"]
                ["maximum"],
            json!(200)
        );
    }

    #[test]
    fn only_enabled_tools_become_native_tools() {
        let tools = comfyui_native_tools(&configuration());
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].capability_id, "comfyui.krea");
        // The model-facing name is the provider name generated from the tool id,
        // never the Settings display name.
        assert_eq!(tools[0].name, "comfyui_krea");
        assert_eq!(tools[0].name, comfyui_provider_name("krea"));
        // The human-authored display name stays on the frozen tool record.
        assert_eq!(tools[0].tool.name, "krea tool");
        assert_eq!(tools[0].tool.id, "krea");
    }

    #[test]
    fn a_disabled_tool_does_not_resolve() {
        let configuration = configuration();
        assert!(comfyui_native_tool(&configuration, "comfyui.krea").is_some());
        assert!(comfyui_native_tool(&configuration, "comfyui.off").is_none());
        assert!(comfyui_native_tool(&configuration, "comfyui.missing").is_none());
        assert!(comfyui_native_tool(&configuration, "tool.context").is_none());
    }

    #[test]
    fn the_schema_carries_type_description_required_choices_and_defaults() {
        let mut tool = workflow_tool("krea", true);
        let mut steps = parameter("steps", ComfyUiParameterKindV2::Integer, false);
        steps.default_value = Some(json!(20));
        let mut sampler = parameter("sampler", ComfyUiParameterKindV2::String, false);
        sampler.choices = vec![json!("euler"), json!("dpmpp_2m")];
        let mut upscale = parameter("upscale", ComfyUiParameterKindV2::Boolean, false);
        upscale.default_value = Some(json!(false));
        tool.parameters = vec![
            parameter("prompt", ComfyUiParameterKindV2::String, true),
            steps,
            sampler,
            upscale,
        ];

        let schema = comfyui_input_schema(&tool);
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["prompt"]));
        assert_eq!(schema["properties"]["prompt"]["type"], json!("string"));
        assert_eq!(
            schema["properties"]["prompt"]["description"],
            json!("prompt description")
        );
        assert_eq!(schema["properties"]["steps"]["type"], json!("integer"));
        assert_eq!(schema["properties"]["steps"]["default"], json!(20));
        assert_eq!(
            schema["properties"]["sampler"]["enum"],
            json!(["euler", "dpmpp_2m"])
        );
        assert_eq!(schema["properties"]["upscale"]["type"], json!("boolean"));
        assert_eq!(schema["properties"]["upscale"]["default"], json!(false));
    }

    #[test]
    fn a_tool_without_parameters_still_declares_a_closed_object() {
        let mut tool = workflow_tool("krea", true);
        tool.parameters = Vec::new();
        let schema = comfyui_input_schema(&tool);
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn the_frozen_configuration_carries_the_endpoint_path_and_bindings() {
        let tool = workflow_tool("krea", true);
        let frozen = comfyui_frozen_configuration(&tool, "http://127.0.0.1:8188/");
        assert_eq!(frozen["endpoint"], json!("http://127.0.0.1:8188/"));
        assert_eq!(frozen["workflowPath"], json!("D:\\workflows\\image.json"));
        assert_eq!(frozen["parameters"][0]["name"], json!("prompt"));
        assert_eq!(frozen["parameters"][0]["nodeId"], json!("6"));
        assert_eq!(frozen["parameters"][0]["inputName"], json!("prompt"));
        assert_eq!(frozen["parameters"][0]["valueKind"], json!("string"));
        assert_eq!(frozen["parameters"][0]["required"], json!(true));
    }
}
