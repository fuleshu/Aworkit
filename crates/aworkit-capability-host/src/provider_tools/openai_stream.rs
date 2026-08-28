//! Bounded OpenAI-compatible Server-Sent Event normalization.

use std::{collections::BTreeMap, io::BufRead};

use serde_json::{Map, Value};

use crate::{
    CancellationToken, ModelToolDefinitionV1, ModelToolEventV1, ProviderError,
    model_tools::normalize_tool_call,
};

#[derive(Default)]
struct PendingToolCall {
    provider_call_id: String,
    name: String,
    arguments: String,
    announced: bool,
}

#[derive(Default)]
struct StreamState {
    calls: BTreeMap<usize, PendingToolCall>,
    finish_reason: Option<String>,
    usage_seen: bool,
    done: bool,
}

/// Consumes one bounded Chat Completions SSE response and emits normalized
/// chunks immediately. Tool calls are emitted only after their streamed JSON
/// arguments are complete and validated.
pub(crate) fn consume_openai_stream<R: BufRead>(
    mut reader: R,
    maximum_response_bytes: usize,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
    cancellation: &CancellationToken,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let mut total_bytes = 0_usize;
    let mut line = String::new();
    let mut data = Vec::new();
    let mut state = StreamState::default();
    loop {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|_| invalid_stream("transport failed"))?;
        total_bytes = total_bytes.saturating_add(read);
        if total_bytes > maximum_response_bytes {
            return Err(invalid_stream("response exceeded the size bound"));
        }
        if read == 0 {
            if !data.is_empty() {
                process_event(
                    &data.join("\n"),
                    tools,
                    provider_namespace,
                    &mut state,
                    emit,
                )?;
            }
            break;
        }
        let field = line.trim_end_matches(['\r', '\n']);
        if field.is_empty() {
            if !data.is_empty() {
                process_event(
                    &data.join("\n"),
                    tools,
                    provider_namespace,
                    &mut state,
                    emit,
                )?;
                data.clear();
            }
        } else if let Some(value) = field.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        } else if !field.starts_with(':')
            && !field.starts_with("event:")
            && !field.starts_with("id:")
            && !field.starts_with("retry:")
        {
            return Err(invalid_stream("contained an unsupported SSE field"));
        }
    }
    if !state.done || !state.usage_seen || state.finish_reason.is_none() {
        return Err(invalid_stream(
            "ended before terminal usage and finish evidence",
        ));
    }
    Ok(())
}

fn process_event(
    data: &str,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
    state: &mut StreamState,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    if state.done {
        return Err(invalid_stream("continued after the terminal marker"));
    }
    if data == "[DONE]" {
        finalize_calls(tools, provider_namespace, state, emit)?;
        state.done = true;
        return Ok(());
    }
    let chunk: Value =
        serde_json::from_str(data).map_err(|_| invalid_stream("contained invalid JSON"))?;
    let object = chunk
        .as_object()
        .ok_or_else(|| invalid_stream("chunk was not an object"))?;
    consume_usage(object, state, emit)?;
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_stream("chunk omitted choices"))?;
    if choices.is_empty() {
        return Ok(());
    }
    if choices.len() != 1 {
        return Err(invalid_stream("returned multiple choices"));
    }
    let choice = choices[0]
        .as_object()
        .ok_or_else(|| invalid_stream("choice was not an object"))?;
    if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
        return Err(invalid_stream("returned an unexpected choice index"));
    }
    let delta = choice
        .get("delta")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_stream("choice omitted its delta"))?;
    emit_reasoning(delta, emit)?;
    emit_text(
        delta,
        "content",
        |text| ModelToolEventV1::AssistantOutput { text },
        emit,
    )?;
    consume_tool_deltas(delta, state, emit)?;
    match choice.get("finish_reason") {
        None | Some(Value::Null) => {}
        Some(Value::String(reason)) if !reason.is_empty() => {
            if state.finish_reason.replace(reason.clone()).is_some() {
                return Err(invalid_stream("contained duplicate finish evidence"));
            }
        }
        Some(_) => return Err(invalid_stream("contained an invalid finish reason")),
    }
    Ok(())
}

fn emit_reasoning(
    delta: &Map<String, Value>,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    // OpenAI-compatible servers currently use both spellings. vLLM's Qwen 3
    // parser emits `reasoning`, while several other adapters expose
    // `reasoning_content`. Validate both and prefer the established field if a
    // server redundantly returns both in one delta.
    let reasoning_content = optional_text(delta, "reasoning_content")?;
    let reasoning = optional_text(delta, "reasoning")?;
    if let Some(text) = reasoning_content.or(reasoning) {
        emit(ModelToolEventV1::ReasoningRaw { text })?;
    }
    Ok(())
}

fn optional_text(delta: &Map<String, Value>, key: &str) -> Result<Option<String>, ProviderError> {
    match delta.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(invalid_stream("contained invalid text content")),
    }
}

fn emit_text(
    delta: &Map<String, Value>,
    key: &str,
    event: impl FnOnce(String) -> ModelToolEventV1,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    match delta.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(text)) if text.is_empty() => Ok(()),
        Some(Value::String(text)) => emit(event(text.clone())),
        Some(_) => Err(invalid_stream("contained invalid text content")),
    }
}

fn consume_tool_deltas(
    delta: &Map<String, Value>,
    state: &mut StreamState,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let Some(value) = delta.get("tool_calls") else {
        return Ok(());
    };
    // Qwen and other OpenAI-compatible servers serialize the optional delta
    // field as null on ordinary reasoning/text chunks. This is equivalent to
    // omitting the field and must not be treated as a malformed tool call.
    if value.is_null() {
        return Ok(());
    }
    let calls = value
        .as_array()
        .ok_or_else(|| invalid_stream("contained invalid tool-call deltas"))?;
    for call in calls {
        let call = call
            .as_object()
            .ok_or_else(|| invalid_stream("tool-call delta was not an object"))?;
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| invalid_stream("tool-call delta omitted its index"))?;
        let pending = state.calls.entry(index).or_default();
        append_optional_string(call, "id", &mut pending.provider_call_id)?;
        if let Some(kind) = call.get("type")
            && kind != "function"
        {
            return Err(invalid_stream("requested a non-function tool"));
        }
        if let Some(function) = call.get("function") {
            let function = function
                .as_object()
                .ok_or_else(|| invalid_stream("tool function delta was invalid"))?;
            append_optional_string(function, "name", &mut pending.name)?;
            append_optional_string(function, "arguments", &mut pending.arguments)?;
        }
        if !pending.announced {
            pending.announced = true;
            emit(ModelToolEventV1::Progress {
                text: "Model is preparing a tool call…".to_owned(),
            })?;
        }
    }
    Ok(())
}

fn append_optional_string(
    object: &Map<String, Value>,
    key: &str,
    target: &mut String,
) -> Result<(), ProviderError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value)) => {
            target.push_str(value);
            Ok(())
        }
        Some(_) => Err(invalid_stream("contained an invalid tool-call field")),
    }
}

fn consume_usage(
    object: &Map<String, Value>,
    state: &mut StreamState,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let Some(usage) = object.get("usage") else {
        return Ok(());
    };
    if usage.is_null() {
        return Ok(());
    }
    if state.usage_seen {
        return Err(invalid_stream("contained duplicate usage"));
    }
    let usage = usage
        .as_object()
        .ok_or_else(|| invalid_stream("contained invalid usage"))?;
    state.usage_seen = true;
    emit(ModelToolEventV1::Usage {
        input_tokens: required_u64(usage, "prompt_tokens")?,
        output_tokens: required_u64(usage, "completion_tokens")?,
    })
}

fn finalize_calls(
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
    state: &mut StreamState,
    emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let finish_reason = state
        .finish_reason
        .as_deref()
        .ok_or_else(|| invalid_stream("omitted its finish reason"))?;
    if state.calls.is_empty() {
        if finish_reason != "stop" {
            return Err(invalid_stream("finished without a supported stop reason"));
        }
        return Ok(());
    }
    if finish_reason != "tool_calls" || tools.is_empty() {
        return Err(invalid_stream("tool calls conflicted with finish evidence"));
    }
    for (ordinal, (_, pending)) in std::mem::take(&mut state.calls).into_iter().enumerate() {
        if pending.name.is_empty() || pending.arguments.is_empty() {
            return Err(invalid_stream("ended with an incomplete tool call"));
        }
        let arguments = serde_json::from_str::<Value>(&pending.arguments)
            .map_err(|_| invalid_stream("ended with invalid tool arguments"))?;
        let call = normalize_tool_call(
            tools,
            (!pending.provider_call_id.is_empty()).then_some(pending.provider_call_id),
            pending.name,
            arguments,
            None,
            ordinal,
            provider_namespace,
        )
        .map_err(|_| invalid_stream("ended with an unsupported tool call"))?;
        emit(ModelToolEventV1::ToolCall { call })?;
    }
    Ok(())
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_stream("usage was incomplete"))
}

fn invalid_stream(reason: &str) -> ProviderError {
    ProviderError::Failed(format!("OpenAI stream {reason}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn reasoning_from(delta: Value) -> Vec<String> {
        let mut state = StreamState::default();
        let mut events = Vec::new();
        process_event(
            &json!({
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": null
                }]
            })
            .to_string(),
            &[],
            "openai.fixture",
            &mut state,
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .expect("reasoning delta");
        events
            .into_iter()
            .filter_map(|event| match event {
                ModelToolEventV1::ReasoningRaw { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn accepts_vllm_qwen_reasoning_alias() {
        assert_eq!(
            reasoning_from(json!({"reasoning": "inspect the repository"})),
            ["inspect the repository"]
        );
    }

    #[test]
    fn prefers_reasoning_content_when_both_aliases_are_present() {
        assert_eq!(
            reasoning_from(json!({
                "reasoning_content": "canonical chunk",
                "reasoning": "duplicate chunk"
            })),
            ["canonical chunk"]
        );
    }
}
