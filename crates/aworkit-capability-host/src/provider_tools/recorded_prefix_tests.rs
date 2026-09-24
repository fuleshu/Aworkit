//! Offline diagnosis of a recorded Run's rendered provider prefix.
//!
//! The store keeps every model-turn request surface verbatim, and the provider
//! cache is keyed on the rendered message bytes, so the two together can say
//! exactly where a cacheable prefix breaks without running anything. Point
//! `AWORKIT_PREFIX_DIR` at a directory of stored request surfaces named in
//! sequence order; the test renders each with the real OpenAI wire mapping and
//! reports, per consecutive pair, the first message that differs and whether the
//! difference is only in bytes (key order, escaping, number form) or in value.
//!
//! A byte-only difference is the interesting one: the provider re-bills
//! everything after it even though the request means the same thing.

#![cfg(test)]

use super::openai::{OpenAiRequestParametersV1, openai_tool_request};
use crate::ModelToolRequestV1;
use serde_json::Value;

/// First index at which two rendered message lists differ in bytes.
fn first_byte_difference(earlier: &[Value], current: &[Value]) -> Option<usize> {
    let shared = earlier.len().min(current.len());
    (0..shared).find(|index| {
        serde_json::to_string(&earlier[*index]).expect("message")
            != serde_json::to_string(&current[*index]).expect("message")
    })
}

/// Whether two messages mean the same thing despite differing bytes.
fn same_value(earlier: &Value, current: &Value) -> bool {
    earlier == current
}

fn rendered_messages(request: &ModelToolRequestV1) -> Vec<Value> {
    let body = openai_tool_request(
        "deepseek-flash",
        request,
        &OpenAiRequestParametersV1::default(),
    )
    .expect("render the stored request");
    body["messages"].as_array().cloned().unwrap_or_default()
}

#[test]
fn recorded_turns_render_to_a_stable_prefix() {
    let Ok(directory) = std::env::var("AWORKIT_PREFIX_DIR") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(&directory)
        .expect("surface directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();

    let mut previous: Option<(String, Vec<Value>)> = None;
    let mut breaks = 0_usize;
    let mut appended = 0_usize;
    for path in paths {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        if name == "index.json" {
            continue;
        }
        let stored: ModelToolRequestV1 =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("surface"))
                .expect("stored request deserializes");
        let messages = rendered_messages(&stored);
        if let Some((earlier_name, earlier)) = previous.as_ref() {
            match first_byte_difference(earlier, &messages) {
                None if messages.len() >= earlier.len() => appended += 1,
                None => {
                    // The new request is shorter: a message was dropped.
                    breaks += 1;
                    eprintln!(
                        "{earlier_name} -> {name}: rendered {} messages then {} (history shrank)",
                        earlier.len(),
                        messages.len()
                    );
                }
                Some(index) => {
                    breaks += 1;
                    let kind = if same_value(&earlier[index], &messages[index]) {
                        "BYTE-ONLY (same meaning)"
                    } else {
                        "value differs"
                    };
                    eprintln!(
                        "{earlier_name} -> {name}: first difference at message {index} of {} -> {}; {kind}",
                        earlier.len(),
                        messages.len()
                    );
                    eprintln!(
                        "    earlier: {}",
                        serde_json::to_string(&earlier[index])
                            .unwrap_or_default()
                            .chars()
                            .take(240)
                            .collect::<String>()
                    );
                    eprintln!(
                        "    current: {}",
                        serde_json::to_string(&messages[index])
                            .unwrap_or_default()
                            .chars()
                            .take(240)
                            .collect::<String>()
                    );
                }
            }
        }
        previous = Some((name, messages));
    }
    eprintln!("pairs appended with no divergence: {appended}; pairs with a break: {breaks}");
}
