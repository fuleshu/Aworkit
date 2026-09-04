//! Advisory detection for repetitive model tool activity.
//!
//! The detector mirrors DeepSeek Harness' repeat-tool reminder strategy: it
//! observes calls without blocking them and emits progressively stronger
//! model-visible notices at the third, fifth, and eighth identical call. Web
//! search also has a cumulative progress reminder because changing the query
//! on every call can still form an expensive, non-converging search loop.

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
    #[serde(default, skip_serializing_if = "is_zero")]
    web_search_calls: u32,
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

        if call.capability_id == "tool.web_search" {
            self.web_search_calls = self.web_search_calls.saturating_add(1);
            if REMINDER_THRESHOLDS.contains(&self.web_search_calls) {
                return Some(web_search_reminder(self.web_search_calls));
            }
        }

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

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn web_search_reminder(count: u32) -> String {
    let guidance = if count == REMINDER_THRESHOLDS[0] {
        "Review the accumulated results now. If they answer the request, stop searching and respond. For current or source-sensitive claims, use web_extract on the best candidate URLs. Search again only for a concrete missing fact."
    } else if count == REMINDER_THRESHOLDS[1] {
        "This search sequence is not converging. Stop broad discovery, identify the exact missing fact, and either extract the best existing URLs or answer with the evidence already gathered. Do not issue another search merely to collect more similar results."
    } else {
        "Eight searches is strong evidence that further query variation is not productive. Finish from the available evidence, use web_extract for a specific unverified page, or clearly state the remaining uncertainty."
    };
    format!(
        "Aworkit web-search progress notice: you have made {count} web_search calls in this Agent run. {guidance}"
    )
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

    #[test]
    fn changed_web_queries_still_trigger_cumulative_progress_reminders() {
        let mut state = RepeatToolReminderStateV1::default();
        assert!(
            state
                .observe_calls(&[call("web_search", json!({"query":"first"}))])
                .is_empty()
        );
        assert!(
            state
                .observe_calls(&[call("web_search", json!({"query":"second"}))])
                .is_empty()
        );
        let third = state.observe_calls(&[call("web_search", json!({"query":"third"}))]);
        assert_eq!(third.len(), 1);
        assert!(third[0].contains("3 web_search calls"));
        assert!(third[0].contains("web_extract"));
    }

    #[test]
    fn other_tools_do_not_reset_the_web_search_progress_count() {
        let mut state = RepeatToolReminderStateV1::default();
        assert!(
            state
                .observe_calls(&[call("web_search", json!({"query":"first"}))])
                .is_empty()
        );
        assert!(
            state
                .observe_calls(&[call("web_extract", json!({"urls":["https://example.com"]}))])
                .is_empty()
        );
        assert!(
            state
                .observe_calls(&[call("web_search", json!({"query":"second"}))])
                .is_empty()
        );
        let third = state.observe_calls(&[call("web_search", json!({"query":"third"}))]);
        assert!(third[0].contains("3 web_search calls"));
    }

    #[test]
    fn web_search_progress_survives_a_durable_checkpoint() {
        let mut state = RepeatToolReminderStateV1::default();
        assert!(
            state
                .observe_calls(&[call("web_search", json!({"query":"first"}))])
                .is_empty()
        );
        let encoded = serde_json::to_value(&state).expect("encode reminder checkpoint");
        let mut restored = serde_json::from_value::<RepeatToolReminderStateV1>(encoded)
            .expect("decode reminder checkpoint");
        assert!(
            restored
                .observe_calls(&[call("web_search", json!({"query":"second"}))])
                .is_empty()
        );
        let third = restored.observe_calls(&[call("web_search", json!({"query":"third"}))]);
        assert!(third[0].contains("3 web_search calls"));
    }
}
