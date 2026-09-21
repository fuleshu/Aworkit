//! Desktop-only command for the Chat's durable goal.

use super::*;

impl DesktopRuntime {
    /// Sets or clears the Chat goal from the desktop user.
    ///
    /// The goal is Chat-owned, non-authoritative state, so the user may change
    /// it at any time, including while a Run is active. The handler records the
    /// same durable snapshot the goal tool writes, so the next Agent pass
    /// injects the updated objective, and commits the canonical `tool.goal`
    /// fact the timeline reducer already folds, so the UI updates without a
    /// model turn. A clear abandons the goal rather than completing it.
    pub(super) fn change_goal(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(input.expected_version)?;
        let clear = input
            .payload
            .get("clear")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = input
            .payload
            .get("goal")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let goal = crate::runtime::tool_loop::apply_user_goal_change(text, clear)?;
        let snapshot = self.history.snapshot(0)?;
        let run_id =
            StableId::parse(snapshot.chat.run_id.clone()).map_err(|error| error.to_string())?;
        self.pipeline.set_goal_state(&run_id, &goal)?;
        self.history.append(
            &input.command_id,
            &fingerprint,
            input.expected_version,
            vec![(
                "tool.goal",
                json!({
                    "goal": goal,
                    "runId": run_id,
                    "createdAt": now_label(),
                }),
            )],
        )
    }
}
