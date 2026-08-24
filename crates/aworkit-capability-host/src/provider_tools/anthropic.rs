//! Anthropic Messages client-tool wire mapping.

use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderError,
    model_tools::{TextMessageRoleV1, normalize_text_input, normalize_tool_call, result_text},
};

pub(crate) fn anthropic_tool_request(
    model: &str,
    maximum_output_tokens: u32,
    request: &ModelToolRequestV1,
) -> Result<Value, ProviderError> {
    let base = normalize_text_input(&request.input)?;
    let system = base
        .iter()
        .filter(|message| message.role == TextMessageRoleV1::System)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut messages = base
        .into_iter()
        .filter(|message| message.role != TextMessageRoleV1::System)
        .map(|message| {
            json!({
                "role": match message.role {
                    TextMessageRoleV1::User => "user",
                    TextMessageRoleV1::Assistant => "assistant",
                    TextMessageRoleV1::System => unreachable!("system messages were filtered"),
                },
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();

    for exchange in &request.exchanges {
        let assistant = exchange
            .assistant_content
            .iter()
            .map(|content| match content {
                ModelAssistantContentV1::Text { text } => Ok(json!({
                    "type": "text",
                    "text": text,
                })),
                ModelAssistantContentV1::ToolCall { call } => {
                    if call.provider_call_id.as_deref() != Some(call.call_id.as_str())
                        || call.provider_context.is_some()
                    {
                        return Err(invalid_request());
                    }
                    Ok(json!({
                        "type": "tool_use",
                        "id": call.call_id,
                        "name": call.name,
                        "input": call.arguments,
                    }))
                }
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        messages.push(json!({"role":"assistant","content":assistant}));

        let results = exchange
            .results
            .iter()
            .map(|result| {
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
                Ok(json!({
                    "type": "tool_result",
                    "tool_use_id": call.call_id,
                    "content": result_text(result)?,
                    "is_error": result.is_error,
                }))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        messages.push(json!({"role":"user","content":results}));
    }

    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(model.to_owned()));
    body.insert("max_tokens".to_owned(), Value::from(maximum_output_tokens));
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("tools".to_owned(), Value::Array(tools));
    body.insert("tool_choice".to_owned(), json!({"type":"auto"}));
    body.insert("stream".to_owned(), Value::Bool(false));
    if !system.is_empty() {
        body.insert("system".to_owned(), Value::String(system));
    }
    Ok(Value::Object(body))
}

pub(crate) fn normalize_anthropic_tool_response(
    response: Value,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
) -> Result<Vec<ModelToolEventV1>, ProviderError> {
    let object = response.as_object().ok_or_else(invalid_response)?;
    let stop_reason = object
        .get("stop_reason")
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)?;
    let blocks = object
        .get("content")
        .and_then(Value::as_array)
        .filter(|blocks| !blocks.is_empty())
        .ok_or_else(invalid_response)?;
    let mut events = Vec::new();
    let mut call_count = 0_usize;
    for block in blocks {
        let block = block.as_object().ok_or_else(invalid_response)?;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => events.push(ModelToolEventV1::AssistantOutput {
                text: required_string(block, "text")?,
            }),
            Some("tool_use") => {
                let id = required_string(block, "id")?;
                let name = required_string(block, "name")?;
                let arguments = block.get("input").cloned().ok_or_else(invalid_response)?;
                let call = normalize_tool_call(
                    tools,
                    Some(id),
                    name,
                    arguments,
                    None,
                    call_count,
                    provider_namespace,
                )
                .map_err(|_| invalid_response())?;
                call_count = call_count.saturating_add(1);
                events.push(ModelToolEventV1::ToolCall { call });
            }
            _ => return Err(invalid_response()),
        }
    }
    if (call_count > 0 && stop_reason != "tool_use")
        || (call_count == 0 && !matches!(stop_reason, "end_turn" | "stop_sequence"))
    {
        return Err(invalid_response());
    }

    let usage = object
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    let cache_creation = optional_u64(usage, "cache_creation_input_tokens")?;
    let cache_read = optional_u64(usage, "cache_read_input_tokens")?;
    let input_tokens = required_u64(usage, "input_tokens")?
        .checked_add(cache_creation)
        .and_then(|total| total.checked_add(cache_read))
        .ok_or_else(invalid_response)?;
    events.push(ModelToolEventV1::Usage {
        input_tokens,
        output_tokens: required_u64(usage, "output_tokens")?,
    });
    Ok(events)
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, ProviderError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(invalid_response)
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    object
        .get(key)
        .map_or(Ok(0), |value| value.as_u64().ok_or_else(invalid_response))
}

fn invalid_request() -> ProviderError {
    ProviderError::Failed("Anthropic tool request is invalid".to_owned())
}

fn invalid_response() -> ProviderError {
    ProviderError::Failed("Anthropic tool response is incomplete or unsupported".to_owned())
}
