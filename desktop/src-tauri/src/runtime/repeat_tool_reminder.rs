//! Advisory detection for consecutive identical model tool calls.
//!
//! The detector mirrors DeepSeek Harness' repeat-tool reminder strategy: it
//! observes calls without blocking them and emits progressively stronger
//! model-visible notices at the third, fifth, and eighth identical call.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use aworkit_capability_host::ModelToolCallV1;

const REMINDER_THRESHOLDS: [u32; 3] = [3, 5, 8];
const ARGUMENT_PREVIEW_CHARS: usize = 500;

const GENTLE_REMINDER: &str = "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.";

/// Durable per-agent consecutive-call state. Call ids are intentionally not
/// part of identity because providers mint a new id for every repetition.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RepeatToolReminderStateV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chain: Option<RepeatToolChainV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepeatToolChainV1 {
    key: String,
    tool_name: String,
    canonical_arguments: String,
    count: u32,
}

impl RepeatToolReminderStateV1 {
    /// Advances the chain in provider order and returns every notice triggered
    /// by this response. The requested calls are never vetoed or rewritten.
    pub(crate) fn observe_calls(&mut self, calls: &[ModelToolCallV1]) -> Vec<String> {
        calls.iter().filter_map(|call| self.observe(call)).collect()
    }

    fn observe(&mut self, call: &ModelToolCallV1) -> Option<String> {
        let canonical_arguments = canonicalize(&call.arguments);
        let key = serde_json::to_string(&(&call.name, &canonical_arguments))
            .expect("tool reminder identity is always JSON encodable");
        let count = self
            .chain
            .as_ref()
            .filter(|chain| chain.key == key)
            .map_or(1, |chain| chain.count.saturating_add(1));
        self.chain = Some(RepeatToolChainV1 {
            key,
            tool_name: call.name.clone(),
            canonical_arguments: canonical_arguments.clone(),
            count,
        });

        if !REMINDER_THRESHOLDS.contains(&count) {
            return None;
        }
        if count == REMINDER_THRESHOLDS[0] {
            return Some(GENTLE_REMINDER.to_owned());
        }
        Some(format!(
            "Repeated tool call detected:\n- tool: {}\n- consecutive_calls: {count}\n- arguments: {}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered.",
            call.name,
            preview_arguments(&canonical_arguments),
        ))
    }
}

/// Deep-sorts object keys before encoding so semantically identical argument
/// objects compare equally even when the provider changes property order.
fn canonicalize(value: &Value) -> String {
    serde_json::to_string(&sort_json(value))
        .expect("model tool arguments are always JSON encodable")
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sort_json).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let sorted = keys
                .into_iter()
                .map(|key| (key.clone(), sort_json(&object[key])))
                .collect::<Map<String, Value>>();
            Value::Object(sorted)
        }
        value => value.clone(),
    }
}

fn preview_arguments(arguments: &str) -> String {
    let count = arguments.chars().count();
    if count <= ARGUMENT_PREVIEW_CHARS {
        return arguments.to_owned();
    }
    let prefix = arguments
        .chars()
        .take(ARGUMENT_PREVIEW_CHARS)
        .collect::<String>();
    format!("{prefix}… (+{} more chars)", count - ARGUMENT_PREVIEW_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: Value) -> ModelToolCallV1 {
        ModelToolCallV1 {
            call_id: format!("call.{name}"),
            provider_call_id: Some(format!("provider-call.{name}")),
            capability_id: format!("tool.{name}"),
            name: name.to_owned(),
            arguments,
            provider_context: None,
        }
    }

    #[test]
    fn exact_repeats_trigger_gentle_then_detailed_reminders() {
        let mut state = RepeatToolReminderStateV1::default();
        let repeated = call("read", json!({"path":"notes.txt"}));
        assert!(state.observe_calls(&[repeated.clone()]).is_empty());
        assert!(state.observe_calls(&[repeated.clone()]).is_empty());
        assert_eq!(state.observe_calls(&[repeated.clone()]), [GENTLE_REMINDER]);
        assert!(state.observe_calls(&[repeated.clone()]).is_empty());
        let fifth = state.observe_calls(&[repeated]);
        assert_eq!(fifth.len(), 1);
        assert!(fifth[0].contains("consecutive_calls: 5"));
        assert!(fifth[0].contains("arguments: {\"path\":\"notes.txt\"}"));

        let repeated = call("read", json!({"path":"notes.txt"}));
        assert!(state.observe_calls(&[repeated.clone()]).is_empty());
        assert!(state.observe_calls(&[repeated.clone()]).is_empty());
        let eighth = state.observe_calls(&[repeated]);
        assert_eq!(eighth.len(), 1);
        assert!(eighth[0].contains("consecutive_calls: 8"));
    }

    #[test]
    fn key_order_is_ignored_but_a_different_call_resets_the_chain() {
        let mut state = RepeatToolReminderStateV1::default();
        let first = call("search", json!({"query":"needle","path":"src"}));
        let reordered = call("search", json!({"path":"src","query":"needle"}));
        assert!(state.observe_calls(&[first]).is_empty());
        assert!(state.observe_calls(&[reordered.clone()]).is_empty());
        assert_eq!(state.observe_calls(&[reordered]), [GENTLE_REMINDER]);

        assert!(
            state
                .observe_calls(&[call("search", json!({"query":"other","path":"src"}))])
                .is_empty()
        );
        assert!(
            state
                .observe_calls(&[call("search", json!({"query":"needle","path":"src"}))])
                .is_empty()
        );
    }

    #[test]
    fn argument_preview_is_bounded_without_weakening_identity() {
        let preview = preview_arguments(&"x".repeat(510));
        assert_eq!(
            preview.chars().take(500).collect::<String>(),
            "x".repeat(500)
        );
        assert!(preview.ends_with("… (+10 more chars)"));
    }
}
