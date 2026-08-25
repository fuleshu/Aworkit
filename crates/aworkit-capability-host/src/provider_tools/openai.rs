//! OpenAI Chat Completions function-tool wire mapping.

use serde_json::{Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolRequestV1, ProviderError,
    model_tools::{TextMessageRoleV1, normalize_text_input, result_text},
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
        "stream": true,
        "stream_options": {"include_usage": true},
    }))
}

fn invalid_request() -> ProviderError {
    ProviderError::Failed("OpenAI tool request is invalid".to_owned())
}
