//! How much larger the provider wire body is than the stored request surface.
//!
//! The benchmark investigation could not reconcile the billed prompt tokens of
//! one turn with the request the harness stored for it, and the two candidate
//! explanations had opposite fixes. This measures the mechanism directly: a
//! tool call's arguments and a tool result's structured content are JSON text
//! embedded as a string, so the wire escapes them a second time. The assertions
//! pin the measured factor so a future change that removes the doubling cannot
//! pass unnoticed.

use super::openai::{OpenAiRequestParametersV1, openai_tool_request};
use crate::{
    ModelAssistantContentV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolExchangeV1,
    ModelToolRequestV1, ModelToolResultV1,
};
use serde_json::json;

/// A file body with the newlines and quotes real source code carries.
fn source(lines: usize) -> String {
    (0..lines)
        .map(|index| format!("  const value{index} = \"line {index}\";\n"))
        .collect()
}

fn request(lines: usize) -> ModelToolRequestV1 {
    let body = source(lines);
    ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: json!({"messages":[{"role":"system","content":"You are a coding agent."},{"role":"user","content":"Write the module."}]}),
        parameters: Default::default(),
        tools: vec![ModelToolDefinitionV1 {
            capability_id: "read".into(),
            name: "read_file".into(),
            description: "Read a UTF-8 file.".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }],
        exchanges: vec![
            // An exchange pairs one assistant turn with the results of the calls
            // it made, which is the shape a validated request must have.
            ModelToolExchangeV1 {
                assistant_content: vec![ModelAssistantContentV1::ToolCall {
                    call: ModelToolCallV1 {
                        call_id: "call.write".into(),
                        provider_call_id: Some("call.write".into()),
                        capability_id: "write".into(),
                        name: "write_file".into(),
                        arguments: json!({"path": "src/engine.js", "content": body}),
                        provider_context: None,
                    },
                }],
                results: vec![ModelToolResultV1 {
                    call_id: "call.write".into(),
                    content: json!({"path": "src/engine.js", "content": body, "bytes": body.len()}),
                    is_error: false,
                    images: Vec::new(),
                }],
            },
            ModelToolExchangeV1 {
                assistant_content: vec![ModelAssistantContentV1::Text {
                    text: "Written.".into(),
                }],
                results: Vec::new(),
            },
        ],
        retry_notice: None,
    }
}

#[test]
fn the_wire_body_escapes_tool_content_a_second_time() {
    let request = request(400);
    let typed = serde_json::to_vec(&request).expect("typed request").len();
    let body = openai_tool_request("fixture", &request, &OpenAiRequestParametersV1::default())
        .expect("wire body");
    let rendered = serde_json::to_vec(&body).expect("rendered body").len();

    // The stored surface already carries the content as JSON, and the wire
    // carries that JSON inside a string, so escaping can only inflate it.
    assert!(
        rendered > typed,
        "wire {rendered} bytes is not larger than the stored {typed} bytes"
    );
    let factor = rendered as f64 / typed as f64;
    assert!(
        factor < 2.0,
        "wire escaping inflated the request {factor:.2}x, which is more than the single extra escape layer explains"
    );

    // The mechanism, asserted exactly: every newline of the file body is
    // escaped once for the stored surface and twice on the wire.
    let wire = serde_json::to_string(&body).expect("wire json");
    assert!(
        wire.contains(r#"\\n  const value399"#),
        "expected a doubly escaped body on the wire"
    );

    // Tokens, not bytes, are what a provider bills: report both factors so a
    // future investigation can compare them against a recorded run.
    let tokenizer = crate::context_compression::Tokenizer::default();
    let typed_tokens = crate::context_compression::count(
        &String::from_utf8_lossy(&serde_json::to_vec(&request).expect("typed")),
        tokenizer,
    );
    let wire_tokens = crate::context_compression::count(&wire, tokenizer);
    eprintln!(
        "wire_size: stored {typed} bytes / {typed_tokens} tokens, wire {rendered} bytes / {wire_tokens} tokens, \
         byte factor {factor:.3}, token factor {:.3}",
        wire_tokens as f64 / typed_tokens as f64
    );
}
