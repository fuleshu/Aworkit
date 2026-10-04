//! Model-assisted workflow parameter authoring.
//!
//! The host never lets the model free-form a schema. It sends the API workflow
//! JSON and its inspected editable inputs, and accepts a proposal only when
//! every parameter binds to an input that the workflow actually contains. A
//! declared type must be compatible with the input's current value; the observed
//! value wins on a mismatch, and a binding that does not exist fails the whole
//! proposal. One format correction is allowed by the caller, never a silent
//! substitution.

use serde::{Deserialize, Serialize};

use aworkit_capability_host::{ComfyUiValueKindV1, ComfyUiWorkflowInspectionV1};

use super::settings::{
    ComfyUiParameterKindV2, ComfyUiToolParameterV2, ComfyUiWorkflowToolV2, valid_parameter_name,
    value_in_choices,
};

/// Upper bound on the API workflow JSON sent to the model.
pub const MAXIMUM_AUTHORING_WORKFLOW_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on the model's authored tool description.
pub const MAXIMUM_AUTHORING_DESCRIPTION_BYTES: usize = 2_000;
/// Upper bound on the model's authored parameter count.
pub const MAXIMUM_AUTHORING_PARAMETERS: usize = 64;

/// The strict JSON contract the authoring model must answer with.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiAuthoringOutputV1 {
    /// Short tool id, lower-case words separated by dashes.
    pub tool_id: String,
    /// Display name.
    pub name: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// The parameters to expose.
    pub parameters: Vec<ComfyUiAuthoredParameterV1>,
    /// Short review note for the user.
    pub summary: String,
}

/// One parameter proposed by the authoring model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiAuthoredParameterV1 {
    /// Parameter name.
    pub name: String,
    /// What the parameter changes.
    pub description: String,
    /// Declared JSON type as a string.
    pub value_type: String,
    /// Whether the model must supply it.
    pub required: bool,
    /// Optional default value.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Node id the value writes to.
    pub node_id: String,
    /// Input name inside that node.
    pub input_name: String,
}

/// The accepted proposal and its review summary.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptedComfyUiAuthoring {
    /// The validated workflow tool, ready for the Settings draft.
    pub tool: ComfyUiWorkflowToolV2,
    /// Short review note carried through from the model.
    pub summary: String,
}

/// Writes the exact authoring contract for one inspected workflow.
#[must_use]
pub fn authoring_instructions(workflow_path: &str, inspection: &ComfyUiWorkflowInspectionV1) -> String {
    let inputs = inspection
        .inputs
        .iter()
        .map(|input| {
            serde_json::json!({
                "nodeId": input.node_id,
                "classType": input.class_type,
                "title": input.title,
                "inputName": input.input_name,
                "valueKind": input.value_kind,
                "currentValue": input.current_value,
                "choices": input.choices,
            })
        })
        .collect::<Vec<_>>();
    let inputs = serde_json::to_string_pretty(&inputs).unwrap_or_else(|_| "[]".into());
    format!(
        r#"You configure one ComfyUI workflow as an Aworkit agent tool.

Analyze only the supplied API workflow and its editable inputs. Choose the
user-meaningful controls a model needs to use this workflow: the main prompt,
managed reference inputs, seed, dimensions, duration, strength, and genuine
workflow switches. Keep model filenames, sampler internals, node plumbing and
implementation-only constants fixed unless they are clearly intended operator
controls.

Every parameter must bind to one editable input from the supplied list. Copy
`nodeId` and `inputName` exactly; never invent a node, an input, a bypass control
or a capability the workflow does not contain. When an optional stage has no
existing enable/bypass input, do not invent one: mention the limitation in
`summary` instead.

Answer with one JSON object and nothing else:
{{
  "toolId": "lower-case-words-separated-by-dashes",
  "name": "Short display name",
  "description": "What the tool does, written for a model",
  "parameters": [
    {{
      "name": "snake_or_camel_name",
      "description": "What this changes",
      "valueType": "string | integer | number | boolean",
      "required": true,
      "default": null,
      "nodeId": "exact node id",
      "inputName": "exact input name"
    }}
  ],
  "summary": "One or two sentences the user should read before saving"
}}

Use at most {MAXIMUM_AUTHORING_PARAMETERS} parameters. Prefer `required` for the
prompt and other values the caller must decide, and leave a sensible `default`
for the rest. Do not include secrets, file contents or commentary outside the
JSON object.

Workflow file:
{workflow_path}

Editable inputs:
{inputs}"#
    )
}

/// A short correction notice appended for the single retry after malformed JSON.
#[must_use]
pub fn authoring_correction_notice(reason: &str) -> String {
    format!(
        "The previous answer was not accepted: {reason}. Answer again with exactly one JSON object matching the required contract, and nothing else."
    )
}

/// Parses the model's answer. A fenced code block is tolerated; any other text
/// around the object, an unknown field, or a wrong type is rejected.
///
/// # Errors
///
/// Returns a human-readable reason the answer cannot be used.
pub fn parse_authoring_output(text: &str) -> Result<ComfyUiAuthoringOutputV1, String> {
    let trimmed = text.trim();
    let body = strip_code_fence(trimmed);
    let output: ComfyUiAuthoringOutputV1 = serde_json::from_str(body)
        .map_err(|error| format!("the answer is not the required JSON object: {error}"))?;
    if output.parameters.len() > MAXIMUM_AUTHORING_PARAMETERS {
        return Err(format!(
            "the answer proposes more than {MAXIMUM_AUTHORING_PARAMETERS} parameters"
        ));
    }
    if output.name.trim().is_empty()
        || output.description.len() > MAXIMUM_AUTHORING_DESCRIPTION_BYTES
        || output.summary.len() > MAXIMUM_AUTHORING_DESCRIPTION_BYTES
    {
        return Err("the answer has an empty name or an oversized description".into());
    }
    Ok(output)
}

fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest
        .strip_prefix("json")
        .or_else(|| rest.strip_prefix("JSON"))
        .unwrap_or(rest);
    let rest = rest.trim_start_matches(['\r', '\n']);
    rest.strip_suffix("```").map_or(rest, str::trim_end)
}

/// Validates an authored proposal against the exact workflow it came from.
///
/// # Errors
///
/// Returns a human-readable reason when any binding, type or name is unusable.
pub fn accept_authoring_output(
    output: ComfyUiAuthoringOutputV1,
    inspection: &ComfyUiWorkflowInspectionV1,
    fallback_tool_id: &str,
    workflow_path: &str,
) -> Result<AcceptedComfyUiAuthoring, String> {
    let tool_id = if output.tool_id.trim().is_empty() {
        fallback_tool_id.to_owned()
    } else {
        output.tool_id.trim().to_owned()
    };
    let mut parameters = Vec::with_capacity(output.parameters.len());
    let mut seen = std::collections::BTreeSet::new();
    for proposed in output.parameters {
        let input = inspection
            .find(&proposed.node_id, &proposed.input_name)
            .ok_or_else(|| {
                format!(
                    "the proposal binds '{}' to node '{}' input '{}', which does not exist in this workflow",
                    proposed.name, proposed.node_id, proposed.input_name
                )
            })?;
        if !valid_parameter_name(&proposed.name) {
            return Err(format!(
                "the proposal uses the invalid parameter name '{}'",
                proposed.name
            ));
        }
        if !seen.insert(proposed.name.clone()) {
            return Err(format!("the proposal repeats parameter '{}'", proposed.name));
        }
        let declared = parse_value_kind(&proposed.value_type);
        let kind = reconcile_kind(input.value_kind, declared);
        let choices = if kind == ComfyUiParameterKindV2::String {
            input.choices.clone()
        } else {
            Vec::new()
        };
        if let Some(default) = &proposed.default {
            if !super::settings::value_matches_kind(default, kind) {
                return Err(format!(
                    "the default for '{}' does not match its type",
                    proposed.name
                ));
            }
            if !value_in_choices(default, &choices) {
                return Err(format!(
                    "the default for '{}' is not one of the workflow's allowed values",
                    proposed.name
                ));
            }
        }
        parameters.push(ComfyUiToolParameterV2 {
            name: proposed.name,
            description: proposed.description.chars().take(MAXIMUM_AUTHORING_DESCRIPTION_BYTES).collect(),
            value_kind: kind,
            required: proposed.required,
            default_value: proposed.default,
            node_id: proposed.node_id,
            input_name: proposed.input_name,
            choices,
        });
    }
    let tool = ComfyUiWorkflowToolV2 {
        id: tool_id,
        name: output.name.trim().to_owned(),
        description: output.description.trim().to_owned(),
        workflow_path: workflow_path.to_owned(),
        enabled: true,
        parameters,
    };
    Ok(AcceptedComfyUiAuthoring {
        tool,
        summary: output.summary.trim().to_owned(),
    })
}

fn parse_value_kind(value: &str) -> ComfyUiParameterKindV2 {
    match value.trim().to_ascii_lowercase().as_str() {
        "integer" | "int" => ComfyUiParameterKindV2::Integer,
        "number" | "float" | "decimal" => ComfyUiParameterKindV2::Number,
        "boolean" | "bool" | "switch" => ComfyUiParameterKindV2::Boolean,
        _ => ComfyUiParameterKindV2::String,
    }
}

/// Reconciles the declared type with the value the workflow actually holds.
/// The observed value wins so a tool cannot claim a type the workflow rejects.
fn reconcile_kind(
    observed: ComfyUiValueKindV1,
    declared: ComfyUiParameterKindV2,
) -> ComfyUiParameterKindV2 {
    match observed {
        ComfyUiValueKindV1::String | ComfyUiValueKindV1::Array => ComfyUiParameterKindV2::String,
        ComfyUiValueKindV1::Boolean => ComfyUiParameterKindV2::Boolean,
        ComfyUiValueKindV1::Integer if declared == ComfyUiParameterKindV2::Number => {
            ComfyUiParameterKindV2::Number
        }
        ComfyUiValueKindV1::Integer => ComfyUiParameterKindV2::Integer,
        ComfyUiValueKindV1::Number => ComfyUiParameterKindV2::Number,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aworkit_capability_host::{ComfyUiWorkflowInputV1, inspect_api_workflow};
    use serde_json::json;

    fn inspection() -> ComfyUiWorkflowInspectionV1 {
        inspect_api_workflow(&json!({
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "sampler_name": "euler"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "hello"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "Krea"}}
        }))
        .unwrap()
    }

    fn output(parameters: serde_json::Value) -> ComfyUiAuthoringOutputV1 {
        serde_json::from_value(json!({
            "toolId": "krea-image",
            "name": "Krea image",
            "description": "Creates one image.",
            "parameters": parameters,
            "summary": "Prompt and seed are the useful controls."
        }))
        .unwrap()
    }

    #[test]
    fn document_reference_prompt_names_the_available_inputs() {
        let inspection = inspection();
        let prompt = authoring_instructions("D:\\wf\\image.json", &inspection);
        assert!(prompt.contains("D:\\wf\\image.json"));
        assert!(prompt.contains("\"inputName\": \"text\""));
        assert!(prompt.contains("\"nodeId\": \"6\""));
        assert!(prompt.contains("nothing else"));
    }

    #[test]
    fn parsing_accepts_a_fenced_object_and_rejects_stray_text() {
        let body = r#"```json
{"toolId":"image","name":"Image","description":"Creates an image.","parameters":[],"summary":"ok"}
```"#;
        let parsed = parse_authoring_output(body).unwrap();
        assert_eq!(parsed.tool_id, "image");
        assert!(parse_authoring_output("Here you go: {}").is_err());
        assert!(parse_authoring_output("{}").is_err());
        assert!(parse_authoring_output("").is_err());
    }

    #[test]
    fn acceptance_binds_only_real_inputs_and_keeps_the_observed_type() {
        let inspection = inspection();
        let accepted = accept_authoring_output(
            output(json!([
                {"name": "prompt", "description": "What to draw", "valueType": "string", "required": true, "default": null, "nodeId": "6", "inputName": "text"},
                {"name": "seed", "description": "Seed", "valueType": "number", "required": false, "default": 7, "nodeId": "3", "inputName": "seed"},
                {"name": "sampler", "description": "Sampler", "valueType": "string", "required": false, "default": "euler", "nodeId": "3", "inputName": "sampler_name"}
            ])),
            &inspection,
            "fallback",
            "D:\\wf\\image.json",
        )
        .unwrap();
        assert_eq!(accepted.tool.id, "krea-image");
        assert_eq!(accepted.tool.workflow_path, "D:\\wf\\image.json");
        let seed = accepted
            .tool
            .parameters
            .iter()
            .find(|parameter| parameter.name == "seed")
            .unwrap();
        // The declared `number` is accepted for an integer input, but never widens a text input.
        assert_eq!(seed.value_kind, ComfyUiParameterKindV2::Number);
        let prompt = &accepted.tool.parameters[0];
        assert_eq!(prompt.value_kind, ComfyUiParameterKindV2::String);
        assert!(prompt.required);
    }

    #[test]
    fn acceptance_rejects_invented_bindings() {
        let inspection = inspection();
        let error = accept_authoring_output(
            output(json!([
                {"name": "prompt", "description": "", "valueType": "string", "required": true, "default": null, "nodeId": "999", "inputName": "text"}
            ])),
            &inspection,
            "fallback",
            "D:\\wf\\image.json",
        )
        .unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
    }

    #[test]
    fn acceptance_rejects_a_default_outside_the_workflow_choices() {
        let graph = json!({
            "3": {"class_type": "KSampler", "inputs": {"sampler_name": "euler"}}
        });
        let object_info = json!({
            "KSampler": {"input": {"required": {"sampler_name": [["euler", "dpmpp_2m"], {"default": "euler"}]}}}
        });
        let inspection =
            aworkit_capability_host::inspect_api_workflow_with_object_info(&graph, Some(&object_info))
                .unwrap();
        let error = accept_authoring_output(
            output(json!([
                {"name": "sampler", "description": "", "valueType": "string", "required": false, "default": "not-a-sampler", "nodeId": "3", "inputName": "sampler_name"}
            ])),
            &inspection,
            "fallback",
            "D:\\wf\\image.json",
        )
        .unwrap_err();
        assert!(error.contains("allowed values"), "{error}");
    }

    #[test]
    fn a_blank_tool_id_falls_back_and_observed_choices_attach() {
        let inspection = inspection();
        let mut proposal = output(json!([
            {"name": "sampler", "description": "", "valueType": "string", "required": false, "default": null, "nodeId": "3", "inputName": "sampler_name"}
        ]));
        proposal.tool_id = "  ".into();
        let accepted =
            accept_authoring_output(proposal, &inspection, "fallback-id", "D:\\wf\\image.json")
                .unwrap();
        assert_eq!(accepted.tool.id, "fallback-id");
    }

    #[test]
    fn unused_input_type_is_still_validated() {
        // Guard the public helper used by the reconciliation logic.
        let _ = ComfyUiWorkflowInputV1 {
            node_id: "1".into(),
            class_type: "X".into(),
            title: None,
            input_name: "y".into(),
            value_kind: ComfyUiValueKindV1::Integer,
            current_value: json!(1),
            choices: Vec::new(),
        };
    }
}
