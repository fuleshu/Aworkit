//! OpenAI Chat Completions function-tool wire mapping.

use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolRequestV1, ProviderError,
    model_tools::{ModelInputRoleV1, normalize_model_input, result_text},
};

/// Serializes one request object with the conversation prompt first.
///
/// `serde_json` orders an object's keys by name (its map is a `BTreeMap` unless
/// the optional `preserve_order` feature is enabled), so a request-scoped
/// parameter such as `max_tokens` would be written before `messages` and change
/// the leading bytes of an otherwise unchanged prompt. A provider-side prefix
/// cache matches from the start of the prompt, and Aworkit's own
/// `sentBytes`/`commonPrefixBytes` evidence measures the same bytes, so a
/// leading parameter both hides and — for a body-prefix cache — destroys the
/// reuse of a prompt that is a strict extension of the previous one. Writing
/// `messages` first keeps the wire prefix equal to the prompt prefix whatever
/// parameters a call adds.
struct PromptFirstBody<'a>(&'a Map<String, Value>);

impl Serialize for PromptFirstBody<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        if let Some(messages) = self.0.get("messages") {
            map.serialize_entry("messages", messages)?;
        }
        for (key, value) in self.0 {
            if key != "messages" {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

/// The exact bytes to put on the wire for one request object, prompt first.
pub(crate) fn prompt_first_body(body: &Value) -> Result<Vec<u8>, ProviderError> {
    let object = body.as_object().ok_or_else(invalid_request)?;
    serde_json::to_vec(&PromptFirstBody(object)).map_err(|_| invalid_request())
}

/// The exact bytes of one OpenAI-compatible tool request. The transport sends
/// these bytes verbatim so the measured body is the body the provider receives.
pub(crate) fn openai_tool_request_body(
    model: &str,
    request: &ModelToolRequestV1,
    parameters: &OpenAiRequestParametersV1,
) -> Result<Vec<u8>, ProviderError> {
    prompt_first_body(&openai_tool_request(model, request, parameters)?)
}

pub(crate) fn openai_tool_request(
    model: &str,
    request: &ModelToolRequestV1,
    parameters: &OpenAiRequestParametersV1,
) -> Result<Value, ProviderError> {
    let mut messages = normalize_model_input(&request.projected_input()?)?
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

    for (index, exchange) in request.exchanges.iter().enumerate() {
        for context in request
            .context_messages
            .iter()
            .filter(|c| c.after_input_messages.is_none() && c.after_exchanges == index)
        {
            messages.push(super::context_message(context, "openai")?);
        }
        let mut reasoning = String::new();
        let mut text = String::new();
        let mut calls = Vec::new();
        for content in &exchange.assistant_content {
            match content {
                ModelAssistantContentV1::Reasoning { text: part } => reasoning.push_str(part),
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
        let mut assistant = json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { Value::String(text) },
        });
        // DeepSeek's thinking mode requires the turn's chain of thought on
        // tool-call turns and counts it in every later prompt either way. Sending
        // the exact retained text keeps this prompt an extension of the one the
        // provider cached, instead of leaving it to re-insert those tokens.
        if !reasoning.is_empty() {
            assistant["reasoning_content"] = Value::String(reasoning);
        }
        if !calls.is_empty() { assistant["tool_calls"] = json!(calls); }
        messages.push(assistant);
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
                "content": result_text(result, &call.capability_id)?,
            }));
        }
    }
    for context in request.context_messages.iter().filter(|c| {
        c.after_input_messages.is_none() && c.after_exchanges == request.exchanges.len()
    }) {
        messages.push(super::context_message(context, "openai")?);
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
    if request.tools.is_empty() {
        let object = body.as_object_mut().expect("request object");
        object.remove("tools");
        object.remove("tool_choice");
    }
    parameters.apply(&mut body);
    Ok(body)
}

/// Closed, non-secret subset of model Settings consumed by the
/// OpenAI-compatible Chat Completions adapter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OpenAiRequestParametersV1 {
    max_output_tokens: Option<u64>,
    reasoning_effort: Option<String>,
    enable_thinking: Option<bool>,
    preserve_thinking: Option<bool>,
}

impl OpenAiRequestParametersV1 {
    pub(crate) fn from_settings(parameters: &BTreeMap<String, Value>) -> Result<Self, ()> {
        if parameters.keys().any(|key| {
            !matches!(
                key.as_str(),
                "reasoningEffort" | "enableThinking" | "preserveThinking" | "maxOutputTokens"
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
            max_output_tokens: parameters
                .get("maxOutputTokens")
                .map(|v| v.as_u64().filter(|n| *n > 0 && *n <= 1_048_576).ok_or(()))
                .transpose()?,
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
            max_output_tokens: overrides.max_output_tokens.or(self.max_output_tokens),
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
        if let Some(cap) = self.max_output_tokens {
            body.insert("max_tokens".into(), json!(cap));
        }
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
