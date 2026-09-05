//! OpenAI Chat Completions function-tool wire mapping.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolRequestV1, ProviderError,
    model_tools::{ModelInputRoleV1, normalize_model_input, result_text},
};

pub(crate) fn openai_tool_request(
    model: &str,
    request: &ModelToolRequestV1,
    parameters: &OpenAiRequestParametersV1,
) -> Result<Value, ProviderError> {
    let mut messages = normalize_model_input(&request.input)?
        .into_iter()
        .map(|message| {
            Ok(json!({
                "role": match message.role {
                    ModelInputRoleV1::System => "system",
                    ModelInputRoleV1::User => "user",
                    ModelInputRoleV1::Assistant => "assistant",
                },
                "content": crate::model_images::image_content(&message.content, &message.images, "openai")?,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    for exchange in &request.exchanges {
        let mut text = String::new();
        let mut calls = Vec::new();
        for content in &exchange.assistant_content {
            match content {
                ModelAssistantContentV1::Text { text: part } => text.push_str(part),
                ModelAssistantContentV1::ToolCall { call } => {
                    if call.provider_call_id.as_deref() != Some(call.call_id.as_str())
                        || call.provider_context.is_some()
                    {
                        return Err(invalid_request());
                    }
                    calls.push(json!({
                        "id": call.call_id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments)
                                .map_err(|_| invalid_request())?,
                        }
                    }));
                }
            }
        }
        messages.push(json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { Value::String(text) },
            "tool_calls": calls,
        }));
        for result in &exchange.results {
            let call = exchange
                .assistant_content
                .iter()
                .find_map(|content| match content {
                    ModelAssistantContentV1::ToolCall { call }
                        if call.call_id == result.call_id =>
                    {
                        Some(call)
                    }
                    _ => None,
                })
                .ok_or_else(invalid_request)?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.call_id,
                "content": result_text(result)?,
            }));
        }
    }
    if let Some(notice) = &request.retry_notice {
        messages.push(json!({"role":"user","content":notice}));
    }

    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    parameters.apply(&mut body);
    Ok(body)
}

/// Closed, non-secret subset of model Settings consumed by the
/// OpenAI-compatible Chat Completions adapter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OpenAiRequestParametersV1 {
    reasoning_effort: Option<String>,
    enable_thinking: Option<bool>,
    preserve_thinking: Option<bool>,
}

impl OpenAiRequestParametersV1 {
    pub(crate) fn from_settings(parameters: &BTreeMap<String, Value>) -> Result<Self, ()> {
        if parameters.keys().any(|key| {
            !matches!(
                key.as_str(),
                "reasoningEffort" | "enableThinking" | "preserveThinking"
            )
        }) {
            return Err(());
        }
        let reasoning_effort = parameters
            .get("reasoningEffort")
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| {
                        matches!(
                            *value,
                            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                        )
                    })
                    .map(str::to_owned)
                    .ok_or(())
            })
            .transpose()?;
        let enable_thinking = optional_bool(parameters, "enableThinking")?;
        let preserve_thinking = optional_bool(parameters, "preserveThinking")?;
        Ok(Self {
            reasoning_effort,
            enable_thinking,
            preserve_thinking,
        })
    }

    /// Applies request-scoped node overrides over the concrete model defaults.
    /// The same closed validation is used at both layers so unsupported fields
    /// never leak into an OpenAI-compatible request body.
    pub(crate) fn with_overrides(&self, overrides: &BTreeMap<String, Value>) -> Result<Self, ()> {
        let overrides = Self::from_settings(overrides)?;
        Ok(Self {
            reasoning_effort: overrides
                .reasoning_effort
                .or_else(|| self.reasoning_effort.clone()),
            enable_thinking: overrides.enable_thinking.or(self.enable_thinking),
            preserve_thinking: overrides.preserve_thinking.or(self.preserve_thinking),
        })
    }

    pub(crate) fn apply(&self, body: &mut Value) {
        let Some(body) = body.as_object_mut() else {
            return;
        };
        if let Some(reasoning_effort) = &self.reasoning_effort {
            body.insert(
                "reasoning_effort".into(),
                Value::String(reasoning_effort.clone()),
            );
        }
        let mut template = Map::new();
        if let Some(enable_thinking) = self.enable_thinking {
            template.insert("enable_thinking".into(), Value::Bool(enable_thinking));
        }
        if let Some(preserve_thinking) = self.preserve_thinking {
            template.insert("preserve_thinking".into(), Value::Bool(preserve_thinking));
        }
        if !template.is_empty() {
            body.insert("chat_template_kwargs".into(), Value::Object(template));
        }
    }
}

fn optional_bool(parameters: &BTreeMap<String, Value>, key: &str) -> Result<Option<bool>, ()> {
    parameters
        .get(key)
        .map(|value| value.as_bool().ok_or(()))
        .transpose()
}

fn invalid_request() -> ProviderError {
    ProviderError::Failed("OpenAI tool request is invalid".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen_thinking_settings_map_to_openai_compatible_wire_fields() {
        let parameters = OpenAiRequestParametersV1::from_settings(&BTreeMap::from([
            ("reasoningEffort".into(), json!("medium")),
            ("enableThinking".into(), json!(true)),
            ("preserveThinking".into(), json!(false)),
        ]))
        .expect("supported settings");
        let mut body = json!({"model":"qwen"});
        parameters.apply(&mut body);
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["chat_template_kwargs"]["preserve_thinking"], false);
    }

    #[test]
    fn unsupported_or_mistyped_parameters_fail_closed() {
        assert!(
            OpenAiRequestParametersV1::from_settings(&BTreeMap::from([(
                "temperature".into(),
                json!(0.5),
            )]))
            .is_err()
        );
        assert!(
            OpenAiRequestParametersV1::from_settings(&BTreeMap::from([(
                "enableThinking".into(),
                json!("yes"),
            )]))
            .is_err()
        );
    }
}
