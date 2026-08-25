//! OpenAI Chat Completions function-tool wire mapping.

use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderError,
    model_tools::{TextMessageRoleV1, normalize_text_input, normalize_tool_call, result_text},
};

pub(crate) fn openai_tool_request(
    model: &str,
    request: &ModelToolRequestV1,
) -> Result<Value, ProviderError> {
    let mut messages = normalize_text_input(&request.input)?
        .into_iter()
        .map(|message| {
            json!({
                "role": match message.role {
                    TextMessageRoleV1::System => "system",
                    TextMessageRoleV1::User => "user",
                    TextMessageRoleV1::Assistant => "assistant",
                },
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();

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
    Ok(json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": false,
    }))
}

pub(crate) fn normalize_openai_tool_response(
    response: Value,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
) -> Result<Vec<ModelToolEventV1>, ProviderError> {
    let object = response.as_object().ok_or_else(invalid_response)?;
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or_else(invalid_response)?;
    let choice = choices[0].as_object().ok_or_else(invalid_response)?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;

    let mut events = Vec::new();
    if let Some(content) = message.get("content") {
        match content {
            Value::Null => {}
            Value::String(text) if !text.is_empty() => {
                events.push(ModelToolEventV1::AssistantOutput { text: text.clone() });
            }
            _ => return Err(invalid_response()),
        }
    }

    // OpenAI-compatible servers commonly serialize the optional final-turn
    // field as `"tool_calls": null` instead of omitting it. Both shapes mean
    // that the assistant returned no calls.
    let calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) => calls.clone(),
        Some(_) => return Err(invalid_response()),
    };
    for (ordinal, call) in calls.iter().enumerate() {
        let call = call.as_object().ok_or_else(invalid_response)?;
        if call.get("type").and_then(Value::as_str) != Some("function") {
            return Err(invalid_response());
        }
        let id = required_string(call, "id")?;
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(invalid_response)?;
        let name = required_string(function, "name")?;
        let arguments = required_string(function, "arguments")?;
        let arguments =
            serde_json::from_str::<Value>(&arguments).map_err(|_| invalid_response())?;
        let call = normalize_tool_call(
            tools,
            Some(id),
            name,
            arguments,
            None,
            ordinal,
            provider_namespace,
        )
        .map_err(|_| invalid_response())?;
        events.push(ModelToolEventV1::ToolCall { call });
    }

    if (!calls.is_empty() && finish_reason != "tool_calls")
        || (calls.is_empty() && finish_reason != "stop")
    {
        return Err(invalid_response());
    }
    append_usage(&mut events, object)?;
    Ok(events)
}

fn append_usage(
    events: &mut Vec<ModelToolEventV1>,
    response: &Map<String, Value>,
) -> Result<(), ProviderError> {
    let usage = response
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    events.push(ModelToolEventV1::Usage {
        input_tokens: required_u64(usage, "prompt_tokens")?,
        output_tokens: required_u64(usage, "completion_tokens")?,
    });
    Ok(())
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

fn invalid_request() -> ProviderError {
    ProviderError::Failed("OpenAI tool request is invalid".to_owned())
}

fn invalid_response() -> ProviderError {
    ProviderError::Failed("OpenAI tool response is incomplete or unsupported".to_owned())
}
