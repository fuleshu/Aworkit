//! Declared, bounded projection of the delegating Agent's conversation.
//!
//! A fork inherits only what the frozen delegation contract declares, never a
//! runtime heuristic: the same parent prefix and the same frozen bounds always
//! produce the same child prefix and the same projection hash.
use super::*;
use crate::runtime::run_events::ParentConversationV1;

/// Longest rendered fragment of one inherited entry. Bounding each entry keeps
/// one huge tool result from crowding out the rest of the inherited history.
const MAXIMUM_ENTRY_BYTES: usize = 8 * 1024;

/// A bounded transcript plus its deterministic identity.
pub(crate) struct ForkProjectionV1 {
    pub transcript: String,
    pub hash: String,
    pub items: usize,
    pub dropped: usize,
}

/// Projects the newest conversation items that fit the frozen message and byte
/// bounds. The oldest items are dropped first, so the child always sees the
/// most recent parent context.
pub(crate) fn project(
    conversation: &ParentConversationV1,
    maximum_items: usize,
    maximum_bytes: usize,
) -> Result<ForkProjectionV1, String> {
    let mut entries = conversation_entries(conversation);
    let total = entries.len();
    let mut kept: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    while let Some(entry) = entries.pop() {
        if kept.len() >= maximum_items {
            break;
        }
        let cost = entry.len().saturating_add(2);
        if !kept.is_empty() && bytes.saturating_add(cost) > maximum_bytes {
            break;
        }
        bytes = bytes.saturating_add(cost);
        kept.push(entry);
        if bytes >= maximum_bytes {
            break;
        }
    }
    kept.reverse();
    let dropped = total.saturating_sub(kept.len());
    let transcript = kept.join("\n\n");
    Ok(ForkProjectionV1 {
        hash: canonical_hash(&transcript).map_err(|error| error.to_string())?,
        transcript,
        items: kept.len(),
        dropped,
    })
}

/// Renders the parent prefix as ordered entries: ordinary user/assistant
/// messages first, then one entry per assistant content and per tool result of
/// every committed exchange. System instructions are never inherited.
fn conversation_entries(conversation: &ParentConversationV1) -> Vec<String> {
    let mut entries = Vec::new();
    if let Some(messages) = conversation.input.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if !matches!(role, "user" | "assistant") {
                continue;
            }
            if let Some(content) = message.get("content").and_then(Value::as_str)
                && !content.trim().is_empty()
            {
                entries.push(format!("{role}: {}", bounded_entry(content)));
            }
        }
    }
    for exchange in &conversation.exchanges {
        for content in &exchange.assistant_content {
            match content {
                ModelAssistantContentV1::Text { text } if !text.trim().is_empty() => {
                    entries.push(format!("assistant: {}", bounded_entry(text)));
                }
                ModelAssistantContentV1::ToolCall { call } => entries.push(format!(
                    "assistant called {} ({}): {}",
                    call.name,
                    call.capability_id,
                    bounded_entry(&call.arguments.to_string())
                )),
                _ => {}
            }
        }
        for result in &exchange.results {
            entries.push(format!(
                "result of {}: {}",
                result.call_id,
                bounded_entry(&result.content.to_string())
            ));
        }
    }
    entries
}

/// Truncates one inherited fragment on a UTF-8 boundary.
fn bounded_entry(value: &str) -> String {
    if value.len() <= MAXIMUM_ENTRY_BYTES {
        return value.to_owned();
    }
    let mut end = MAXIMUM_ENTRY_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}
