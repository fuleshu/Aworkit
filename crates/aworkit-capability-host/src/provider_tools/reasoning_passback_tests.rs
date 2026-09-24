//! Chain-of-thought passback and the append-only prompt invariant.
//!
//! DeepSeek's thinking mode counts a turn's reasoning inside every later prompt
//! whether or not the client sends it back. A run that omits it leaves the
//! provider to re-insert those tokens, so the prompt the provider caches and the
//! prompt the harness believes it sent drift apart, and a whole context can be
//! re-billed when a cache block stops matching. The recorded benchmark run
//! showed exactly that: every turn where the provider credited only the stable
//! header sat on a turn where the re-inserted reasoning appeared or vanished,
//! and those turns were most of the run's uncached tokens.
//!
//! These assertions pin the two properties the fix depends on: retained
//! reasoning reaches the wire, and consecutive prompts stay extensions of one
//! another so a provider cache can still match them.

use super::openai::{OpenAiRequestParametersV1, openai_tool_request};
use crate::{
    ModelAssistantContentV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolExchangeV1,
    ModelToolRequestV1, ModelToolResultV1,
};
use serde_json::{Value, json};

fn call() -> ModelToolCallV1 {
    ModelToolCallV1 {
        call_id: "call.read".into(),
        provider_call_id: Some("call.read".into()),
        capability_id: "read".into(),
        name: "read_file".into(),
        arguments: json!({"path": "src/engine.js"}),
        provider_context: None,
    }
}

fn definition() -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "read".into(),
        name: "read_file".into(),
        description: "Read a UTF-8 file.".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }
}

/// One exchange whose assistant turn carries `reasoning`.
fn request(reasoning: Option<&str>) -> ModelToolRequestV1 {
    let mut assistant_content = Vec::new();
    if let Some(text) = reasoning {
        // The provider streams its chain of thought in fragments; the retained
        // content keeps them in one part, in arrival order.
        assistant_content.push(ModelAssistantContentV1::Reasoning {
            text: text.to_owned(),
        });
    }
    assistant_content.push(ModelAssistantContentV1::ToolCall { call: call() });
    ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: json!({"messages":[{"role":"system","content":"You are a coding agent."},{"role":"user","content":"Read the engine."}]}),
        parameters: Default::default(),
        tools: vec![definition()],
        exchanges: vec![ModelToolExchangeV1 {
            assistant_content,
            results: vec![ModelToolResultV1 {
                call_id: "call.read".into(),
                content: json!("engine"),
                is_error: false,
                images: Vec::new(),
            }],
        }],
        retry_notice: None,
    }
}

fn render(request: &ModelToolRequestV1) -> Value {
    openai_tool_request(
        "deepseek-flash",
        request,
        &OpenAiRequestParametersV1::default(),
    )
    .expect("wire body")
}

fn assistant_message(body: &Value) -> &Value {
    body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("assistant message")
}

#[test]
fn retained_reasoning_replays_as_reasoning_content() {
    let body = render(&request(Some("Think about the reader.")));
    let assistant = assistant_message(&body);
    assert_eq!(
        assistant["reasoning_content"], "Think about the reader.",
        "the retained chain of thought has to reach the provider"
    );
    // Keys serialize in the map's canonical (sorted) order, so the field lands
    // in the same place on every turn — the cached bytes depend on that.
    let keys: Vec<&str> = assistant
        .as_object()
        .expect("assistant object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["content", "reasoning_content", "role", "tool_calls"]);
}

#[test]
fn a_turn_without_reasoning_sends_no_reasoning_content() {
    let body = render(&request(None));
    let assistant = assistant_message(&body);
    assert!(
        assistant.get("reasoning_content").is_none(),
        "a provider that streamed no reasoning must not receive the field"
    );
}

#[test]
fn consecutive_prompts_stay_extensions_of_one_another() {
    let first = render(&request(Some("first thought")));
    // The following turn keeps the earlier exchange verbatim and appends its own,
    // exactly as a tool loop does.
    let mut next = request(Some("first thought"));
    next.exchanges.push(ModelToolExchangeV1 {
        assistant_content: vec![ModelAssistantContentV1::Text {
            text: "Read it.".into(),
        }],
        results: Vec::new(),
    });
    let second = render(&next);

    let messages = |body: &Value| {
        body["messages"]
            .as_array()
            .expect("messages array")
            .clone()
    };
    let (earlier, current) = (messages(&first), messages(&second));
    assert!(
        current.len() > earlier.len(),
        "the following turn appended its own exchange"
    );
    assert_eq!(
        &current[..earlier.len()],
        &earlier[..],
        "the provider visible message list has to stay an extension of the previous one"
    );
    // The replayed reasoning is what keeps that true across a reasoning turn.
    assert!(
        serde_json::to_string(&earlier)
            .expect("earlier json")
            .contains("reasoning_content"),
        "the earlier prompt carries the retained reasoning"
    );
}
