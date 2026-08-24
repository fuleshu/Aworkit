//! Gemini `generateContent` function-call wire mapping.

use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderError,
    model_tools::{TextMessageRoleV1, normalize_text_input, normalize_tool_call},
};

pub(crate) fn gemini_tool_request(request: &ModelToolRequestV1) -> Result<Value, ProviderError> {
    let base = normalize_text_input(&request.input)?;
    let system_parts = base
        .iter()
        .filter(|message| message.role == TextMessageRoleV1::System)
        .map(|message| json!({"text":message.content}))
        .collect::<Vec<_>>();
    let mut contents = base
        .into_iter()
        .filter(|message| message.role != TextMessageRoleV1::System)
        .map(|message| {
            json!({
                "role": match message.role {
                    TextMessageRoleV1::User => "user",
                    TextMessageRoleV1::Assistant => "model",
                    TextMessageRoleV1::System => unreachable!("system messages were filtered"),
                },
                "parts": [{"text":message.content}],
            })
        })
        .collect::<Vec<_>>();

    for exchange in &request.exchanges {
        let model_parts = exchange
            .assistant_content
            .iter()
            .map(|content| match content {
                ModelAssistantContentV1::Text { text } => Ok(json!({"text":text})),
                ModelAssistantContentV1::ToolCall { call } => {
                    if call
                        .provider_call_id
                        .as_deref()
                        .is_some_and(|id| id != call.call_id)
                    {
                        return Err(invalid_request());
                    }
                    let mut function = Map::new();
                    if let Some(id) = &call.provider_call_id {
                        function.insert("id".to_owned(), Value::String(id.clone()));
                    }
                    function.insert("name".to_owned(), Value::String(call.name.clone()));
                    function.insert("args".to_owned(), call.arguments.clone());
                    let mut part = Map::new();
                    part.insert("functionCall".to_owned(), Value::Object(function));
                    if let Some(context) = &call.provider_context {
                        part.insert(
                            "thoughtSignature".to_owned(),
                            Value::String(context.as_str().to_owned()),
                        );
                    }
                    Ok(Value::Object(part))
                }
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        contents.push(json!({"role":"model","parts":model_parts}));

        let result_parts = exchange
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
                let mut function = Map::new();
                if let Some(id) = &call.provider_call_id {
                    function.insert("id".to_owned(), Value::String(id.clone()));
                }
                function.insert("name".to_owned(), Value::String(call.name.clone()));
                function.insert(
                    "response".to_owned(),
                    if result.is_error {
                        json!({"error":result.content})
                    } else {
                        json!({"output":result.content})
                    },
                );
                Ok(json!({"functionResponse":Value::Object(function)}))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        contents.push(json!({"role":"user","parts":result_parts}));
    }

    let declarations = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name":tool.name,
                "description":tool.description,
                "parametersJsonSchema":tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::new();
    body.insert("contents".to_owned(), Value::Array(contents));
    body.insert(
        "tools".to_owned(),
        json!([{"functionDeclarations":declarations}]),
    );
    body.insert(
        "toolConfig".to_owned(),
        json!({"functionCallingConfig":{"mode":"AUTO"}}),
    );
    body.insert("generationConfig".to_owned(), json!({"candidateCount":1}));
    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_owned(),
            json!({"parts":system_parts}),
        );
    }
    Ok(Value::Object(body))
}

pub(crate) fn normalize_gemini_tool_response(
    response: Value,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
) -> Result<Vec<ModelToolEventV1>, ProviderError> {
    let object = response.as_object().ok_or_else(invalid_response)?;
    let candidates = object
        .get("candidates")
        .and_then(Value::as_array)
        .filter(|candidates| candidates.len() == 1)
        .ok_or_else(invalid_response)?;
    let candidate = candidates[0].as_object().ok_or_else(invalid_response)?;
    if candidate.get("finishReason").and_then(Value::as_str) != Some("STOP") {
        return Err(invalid_response());
    }
    let content = candidate
        .get("content")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    if content.get("role").and_then(Value::as_str) != Some("model") {
        return Err(invalid_response());
    }
    let parts = content
        .get("parts")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(invalid_response)?;
    let mut events = Vec::new();
    let mut call_count = 0_usize;
    for part in parts {
        let part = part.as_object().ok_or_else(invalid_response)?;
        match (part.get("text"), part.get("functionCall")) {
            (Some(Value::String(text)), None) if !text.is_empty() => {
                if part.contains_key("thoughtSignature") {
                    return Err(invalid_response());
                }
                events.push(ModelToolEventV1::AssistantOutput { text: text.clone() });
            }
            (None, Some(Value::Object(function))) => {
                let provider_call_id = function
                    .get("id")
                    .map(|id| {
                        id.as_str()
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(invalid_response)
                    })
                    .transpose()?;
                let name = required_string(function, "name")?;
                let arguments = function.get("args").cloned().unwrap_or_else(|| json!({}));
                let context = part
                    .get("thoughtSignature")
                    .map(|context| {
                        context
                            .as_str()
                            .filter(|context| !context.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(invalid_response)
                    })
                    .transpose()?;
                let call = normalize_tool_call(
                    tools,
                    provider_call_id,
                    name,
                    arguments,
                    context,
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

    let usage = object
        .get("usageMetadata")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    let output_tokens = required_u64(usage, "candidatesTokenCount")?
        .checked_add(optional_u64(usage, "thoughtsTokenCount")?)
        .ok_or_else(invalid_response)?;
    events.push(ModelToolEventV1::Usage {
        input_tokens: required_u64(usage, "promptTokenCount")?,
        output_tokens,
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
    ProviderError::Failed("Gemini tool request is invalid".to_owned())
}

fn invalid_response() -> ProviderError {
    ProviderError::Failed("Gemini tool response is incomplete or unsupported".to_owned())
}
