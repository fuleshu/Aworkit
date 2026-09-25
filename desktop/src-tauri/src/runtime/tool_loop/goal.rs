//! The Run's single durable goal.
//!
//! The goal is the one outcome the Chat is working toward. It is recorded as
//! immutable snapshots beside the run-local task list, so it survives a Wait
//! resume and crash recovery without replaying anything, and it is injected
//! into each Agent pass as generated context rather than as a new instruction.
//! The tool owns no host authority: it only reads and rewrites Chat-owned state.

use super::*;

/// Bound for the objective text. Kept equal to the manifest schema bound.
pub(crate) const MAXIMUM_GOAL_BYTES: usize = 8 * 1024;
/// Bound for the optional progress note.
const MAXIMUM_GOAL_NOTE_BYTES: usize = 4 * 1024;
/// The only statuses the tool itself writes. `none` is returned by `get` before
/// any goal exists; `cleared` marks an abandoned goal in the audit trail.
const WRITTEN_STATUSES: [&str; 2] = ["active", "completed"];

pub(super) fn schema() -> Value {
    super::super::tool_registry::native_tool(GOAL_CAPABILITY_ID)
        .expect("installed native tool")
        .input_schema
        .clone()
}

/// Validates one goal call against the frozen argument contract before the
/// durable state transition runs.
pub(crate) fn validate(
    arguments: &serde_json::Map<String, Value>,
) -> Result<(), WorkflowPipelineError> {
    let operation = arguments
        .get("operation")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "set" | "get" | "update" | "complete" | "clear"))
        .ok_or_else(|| {
            invalid_tool("goal operation must be set, get, update, complete, or clear")
        })?;
    if let Some(goal) = arguments.get("goal") {
        bounded_text(goal, MAXIMUM_GOAL_BYTES, "goal text")?;
    }
    if let Some(note) = arguments.get("note") {
        bounded_text(note, MAXIMUM_GOAL_NOTE_BYTES, "goal note")?;
    }
    if matches!(operation, "set" | "update") && arguments.get("goal").is_none() {
        return Err(invalid_tool(
            "setting or updating a goal requires the goal text",
        ));
    }
    Ok(())
}

fn bounded_text(value: &Value, maximum: usize, label: &str) -> Result<(), WorkflowPipelineError> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid_tool(&format!("{label} must be a string")))?;
    if text.trim().is_empty() || text.len() > maximum || text.contains('\0') {
        return Err(invalid_tool(&format!(
            "{label} is empty, oversized, or malformed"
        )));
    }
    Ok(())
}

/// Whether a recorded snapshot represents a goal that is still in effect.
///
/// `cleared` and the synthetic `none` state carry no objective, so they are
/// absent from context, from the UI fact, and from completion checks.
pub(crate) fn is_live(state: &Value) -> bool {
    matches!(
        state.get("status").and_then(Value::as_str),
        Some("active" | "completed")
    )
}

fn live_goal(current: Option<&Value>) -> Option<&Value> {
    current.filter(|state| is_live(state))
}

/// Applies one validated operation to the durable goal state.
///
/// `current` is the latest recorded snapshot, which may already be completed or
/// cleared. The returned value is the next snapshot to record. The transition
/// is pure so it can be tested without a Run.
pub(crate) fn apply(current: Option<&Value>, arguments: &Value) -> Result<Value, String> {
    let operation = arguments["operation"]
        .as_str()
        .expect("validated goal operation");
    let goal = arguments.get("goal").and_then(Value::as_str);
    let note = arguments.get("note").and_then(Value::as_str);
    match operation {
        "get" => Ok(current
            .cloned()
            .unwrap_or_else(|| json!({"status": "none"}))),
        "set" => Ok(goal_state("active", goal.expect("validated goal"), note)),
        "update" => {
            let previous = live_goal(current)
                .ok_or_else(|| "there is no goal to update; use set".to_owned())?;
            Ok(goal_state(
                previous["status"].as_str().unwrap_or("active"),
                goal.expect("validated goal"),
                note.or_else(|| previous.get("note").and_then(Value::as_str)),
            ))
        }
        "complete" => {
            let previous =
                live_goal(current).ok_or_else(|| "there is no goal to complete".to_owned())?;
            Ok(goal_state(
                "completed",
                goal.or_else(|| previous.get("goal").and_then(Value::as_str))
                    .unwrap_or_default(),
                note.or_else(|| previous.get("note").and_then(Value::as_str)),
            ))
        }
        "clear" => Ok(json!({"status": "cleared"})),
        _ => Err("goal operation is unsupported".to_owned()),
    }
}

fn goal_state(status: &str, goal: &str, note: Option<&str>) -> Value {
    debug_assert!(WRITTEN_STATUSES.contains(&status));
    let mut state = json!({"status": status, "goal": goal});
    if let Some(note) = note.filter(|note| !note.trim().is_empty()) {
        state["note"] = json!(note);
    }
    state
}

/// Applies a goal change issued by the desktop user rather than the model.
///
/// The goal is Chat-owned, non-authoritative state, so the user may set or
/// clear it at any time, including while a Run is active; the write is a plain
/// durable state update and never injects text into the conversation. A clear
/// abandons the objective instead of completing it, so the audit trail keeps
/// the distinction between "done" and "dropped".
pub(crate) fn apply_user_change(text: &str, clear: bool) -> Result<Value, String> {
    if clear {
        return Ok(json!({"status": "cleared"}));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > MAXIMUM_GOAL_BYTES || trimmed.contains('\0') {
        return Err("goal text is empty, oversized, or malformed".to_owned());
    }
    Ok(goal_state("active", trimmed, None))
}

/// Builds the generated context message that carries the live goal into an
/// Agent request. It is labelled as state, not as a user instruction, so the
/// model never mistakes a durable objective for a new request.
pub(crate) fn context_message(goal: &Value) -> aworkit_capability_host::ModelToolContextV1 {
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("active");
    let objective = goal.get("goal").and_then(Value::as_str).unwrap_or_default();
    let note = goal
        .get("note")
        .and_then(Value::as_str)
        .filter(|note| !note.trim().is_empty());
    let mut content = format!(
        "{}{status}; durable state for this Chat, not a new instruction):\n{objective}",
        crate::runtime::compaction::GOAL_STATE_LABEL
    );
    if let Some(note) = note {
        content.push_str("\nLatest note: ");
        content.push_str(note);
    }
    aworkit_capability_host::ModelToolContextV1 {
        after_exchanges: 0,
        role: Some("user".into()),
        content,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validated(arguments: Value) -> Value {
        let object = arguments.as_object().expect("goal arguments");
        validate(object).expect("valid goal call");
        arguments
    }

    #[test]
    fn goal_lifecycle_transitions_are_single_sourced() {
        assert_eq!(
            call(validated(json!({"operation": "get"})), None)["status"],
            "none"
        );
        let set = call(
            validated(json!({"operation": "set", "goal": "Fix the failing run"})),
            None,
        );
        assert_eq!(set["status"], "active");
        assert_eq!(set["goal"], "Fix the failing run");
        let updated = apply(
            Some(&set),
            &validated(json!({"operation": "update", "goal": "Fix and verify"})),
        )
        .expect("update");
        assert_eq!(updated["goal"], "Fix and verify");
        let completed = apply(
            Some(&updated),
            &validated(json!({"operation": "complete", "note": "tests green"})),
        )
        .expect("complete");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["goal"], "Fix and verify");
        assert_eq!(completed["note"], "tests green");
        let cleared =
            apply(Some(&completed), &validated(json!({"operation": "clear"}))).expect("clear");
        assert_eq!(cleared["status"], "cleared");
        assert!(!is_live(&cleared));
    }

    fn call(arguments: Value, current: Option<&Value>) -> Value {
        apply(current, &arguments).expect("goal call")
    }

    #[test]
    fn goal_operations_reject_missing_and_oversized_objectives() {
        assert!(validate(json!({"operation": "set"}).as_object().expect("object")).is_err());
        let oversized = "x".repeat(MAXIMUM_GOAL_BYTES + 1);
        assert!(
            validate(
                json!({"operation": "set", "goal": oversized})
                    .as_object()
                    .expect("object")
            )
            .is_err()
        );
        assert!(apply(None, &json!({"operation": "update", "goal": "later"})).is_err());
        assert!(apply(None, &json!({"operation": "complete"})).is_err());
    }

    #[test]
    fn user_changes_set_or_clear_without_completing() {
        let set = apply_user_change("  User objective  ", false).expect("user set");
        assert_eq!(set["status"], "active");
        assert_eq!(set["goal"], "User objective");
        let cleared = apply_user_change("ignored", true).expect("user clear");
        assert_eq!(cleared["status"], "cleared");
        assert!(!is_live(&cleared));
        assert!(apply_user_change("   ", false).is_err());
        assert!(apply_user_change(&"x".repeat(MAXIMUM_GOAL_BYTES + 1), false).is_err());
    }

    #[test]
    fn context_message_labels_durable_state_and_carries_the_note() {
        let message = context_message(&json!({
            "status": "active",
            "goal": "Ship task 108",
            "note": "blocked on tests",
        }));
        assert_eq!(message.role.as_deref(), Some("user"));
        assert!(message.content.contains("Ship task 108"));
        assert!(message.content.contains("blocked on tests"));
        assert!(message.content.contains("not a new instruction"));
    }
}
