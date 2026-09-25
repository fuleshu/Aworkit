//! The wire body has to start with the prompt, not with a request parameter.
//!
//! A provider-side prefix cache matches from the beginning of the request, and
//! Aworkit's `sentBytes`/`commonPrefixBytes` evidence measures exactly the bytes
//! it sends. `serde_json` writes object keys in name order, so a call that adds
//! `max_tokens` would put it before `messages` and reduce the reusable prefix to
//! the three bytes `{"m`. The recorded test1_qwen compaction call showed that:
//! `context.compacted.auxiliary` reported `commonPrefixBytes` 3 against a 540,125
//! byte summary prompt that is a strict extension of the live conversation.
//!
//! These assertions pin the ordering itself and the compaction consequence: the
//! summary request may add its own output cap, and its prompt must still be the
//! leading bytes so the shared prefix survives.

use std::collections::BTreeMap;

use super::openai::{OpenAiRequestParametersV1, openai_tool_request, openai_tool_request_body};
use crate::{
    ModelAssistantContentV1, ModelToolCallV1, ModelToolContextV1, ModelToolDefinitionV1,
    ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1,
};
use serde_json::json;

/// Longest leading run of equal bytes in two rendered request bodies.
fn shared_prefix(earlier: &[u8], current: &[u8]) -> usize {
    earlier
        .iter()
        .zip(current.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

fn definition() -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "read".into(),
        name: "read_file".into(),
        description: "Read a UTF-8 file.".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }
}

/// One settled tool exchange, numbered so every exchange renders differently.
fn exchange(index: usize) -> ModelToolExchangeV1 {
    let call_id = format!("call.read.{index}");
    ModelToolExchangeV1 {
        assistant_content: vec![
            ModelAssistantContentV1::Text {
                text: format!("Reading file {index}."),
            },
            ModelAssistantContentV1::ToolCall {
                call: ModelToolCallV1 {
                    call_id: call_id.clone(),
                    provider_call_id: Some(call_id.clone()),
                    capability_id: "read".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": format!("src/module{index}.rs")}),
                    provider_context: None,
                },
            },
        ],
        results: vec![ModelToolResultV1 {
            call_id,
            content: json!(format!("file {index} body")),
            is_error: false,
            images: Vec::new(),
        }],
    }
}

/// A live conversation with `exchanges` settled tool turns.
fn live_request(exchanges: usize) -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: json!({"messages":[
            {"role":"system","content":"You are a coding agent."},
            {"role":"user","content":"Inspect the modules."}
        ]}),
        parameters: Default::default(),
        tools: vec![definition()],
        exchanges: (0..exchanges).map(exchange).collect(),
        retry_notice: None,
    }
}

fn parameters(max_output_tokens: Option<u64>) -> OpenAiRequestParametersV1 {
    let mut settings = BTreeMap::new();
    if let Some(cap) = max_output_tokens {
        settings.insert("maxOutputTokens".to_string(), json!(cap));
    }
    OpenAiRequestParametersV1::from_settings(&settings).expect("supported parameters")
}

fn body(request: &ModelToolRequestV1, max_output_tokens: Option<u64>) -> Vec<u8> {
    openai_tool_request_body("deepseek-flash", request, &parameters(max_output_tokens))
        .expect("wire body")
}

#[test]
fn the_prompt_leads_the_body_even_when_a_parameter_would_sort_first() {
    let request = live_request(2);
    let plain = body(&request, None);
    let capped = body(&request, Some(8192));

    // `max_tokens` sorts before `messages`; the prompt still leads.
    assert!(
        plain.starts_with(b"{\"messages\":["),
        "the plain body must open with the prompt: {}",
        String::from_utf8_lossy(&plain[..40.min(plain.len())])
    );
    assert!(
        capped.starts_with(b"{\"messages\":["),
        "an output cap must not be written before the prompt: {}",
        String::from_utf8_lossy(&capped[..40.min(capped.len())])
    );
    assert!(capped.len() > plain.len(), "the cap is still on the wire");
    // The two calls carry the same prompt, so the entire prompt region is
    // shared and only the trailing parameter differs.
    let prompt_end = plain
        .windows(9)
        .position(|window| window == b"],\"model\"")
        .expect("messages array closes before the model key");
    // Both bodies continue with `,"m` (",\"model\"" against ",\"max_tokens\""),
    // so the shared run reaches just past the prompt before the keys diverge.
    let shared = shared_prefix(&plain, &capped);
    assert!(
        shared >= prompt_end + 2 && shared < prompt_end + 8,
        "the shared prefix ({shared}) must cover the whole identical prompt ending at {prompt_end}"
    );
}

#[test]
fn a_compaction_prompt_extends_the_live_prompt_instead_of_reshaping_it() {
    // The live turn the model just sent, and the auxiliary summary call built
    // from its shadowed leading exchanges plus the summarization directive.
    let live = live_request(3);
    let mut summary = live_request(1);
    summary.context_messages.push(ModelToolContextV1 {
        content: "Condense the conversation above.".into(),
        after_exchanges: 1,
        ..Default::default()
    });
    // The summary call raises its own output cap; it must not move the prompt.
    let live_body = body(&live, None);
    let summary_body = body(&summary, Some(8192));

    let shared = shared_prefix(&live_body, &summary_body);
    // The directive is the only novel part of the summary prompt. Everything
    // before it is the shadowed prefix the previous request already carried.
    assert!(
        shared > 3,
        "the summary body must reuse the live prompt prefix, shared only {shared} bytes"
    );
    const DIRECTIVE: &[u8] = b"Condense the conversation above.";
    let directive = summary_body
        .windows(DIRECTIVE.len())
        .position(|window| window == DIRECTIVE)
        .expect("the directive is on the wire");
    assert!(
        shared >= directive,
        "the shared prefix ({shared}) must reach the directive at {directive}"
    );
    // The retained tail is what makes the summary prompt shorter than the live
    // one; the shared prefix is still the whole shadowed region.
    assert!(
        summary_body.len() < live_body.len(),
        "the summary request shadows a tail instead of resending it"
    );
}

#[test]
fn the_sorted_body_order_is_what_reduced_the_recorded_prefix_to_three_bytes() {
    let request = live_request(2);
    // Reproduce the wire bytes the provider received before the fix: the body
    // serialized in the map's canonical (sorted) order with the auxiliary cap
    // added as another key, so `max_tokens` is written before `messages`.
    let mut sorted_capped =
        openai_tool_request("deepseek-flash", &request, &parameters(None)).expect("wire body");
    sorted_capped["max_tokens"] = json!(8192);
    let sorted_capped = serde_json::to_vec(&sorted_capped).expect("sorted body");
    let sorted_plain = serde_json::to_vec(
        &openai_tool_request("deepseek-flash", &request, &parameters(None)).expect("wire body"),
    )
    .expect("sorted body");
    assert_eq!(
        shared_prefix(&sorted_plain, &sorted_capped),
        3,
        "the recorded commonPrefixBytes 3 is exactly a leading max_tokens key"
    );

    // The shipped serialization keeps the prompt in front, so the same two
    // calls share the prompt the provider could reuse.
    let plain = body(&request, None);
    let capped = body(&request, Some(8192));
    assert!(
        shared_prefix(&plain, &capped) > 100,
        "the shipped bodies must share the whole prompt"
    );
}
