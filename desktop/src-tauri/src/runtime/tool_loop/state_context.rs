//! Durable Run state re-emitted after a compaction replaced the transcript.
//!
//! A summary is a lossy projection, so the one goal, the task list and the
//! files this Run already observed must not depend on the summariser keeping
//! them. They are re-derived from the same records the tools wrote, which also
//! means a compaction can never carry a stale copy forward: the newest record
//! is the live state. Workspace instructions and the host clock are not
//! duplicated here; they have their own restoration path in
//! `workspace_context`.
//!
//! Where a block goes is a cost decision, not a formatting one. A provider
//! caches the longest common prefix of a prompt, so a byte changed in the middle
//! of a selection re-bills every token after it: the measured 100k benchmark run
//! paid about 50k uncached tokens for a 60-character edit to a state message
//! that sat seven exchanges deep. A replaced selection is rewritten anyway, so a
//! compaction may collapse every copy into one block; between compactions a
//! changed part is appended at the tail instead, which leaves the cached prefix
//! intact.

use super::*;
use crate::runtime::compaction::{
    is_generated_state, is_occupancy_state, FILES_STATE_LABEL, GOAL_STATE_LABEL,
    OCCUPANCY_STATE_LABEL, TASK_STATE_LABEL,
};
use aworkit_capability_host::ModelToolRequestV1;

/// Longest state message one emission appends. The content is generated from
/// records, so the bound is a formatting guard rather than a real truncation.
const MAXIMUM_STATE_BYTES: usize = 2048;

/// How much superseded state a selection may carry before the block is
/// collapsed instead of appended to again.
///
/// Appending keeps the cached prefix, but a superseded copy is never free: a
/// long run that changes state on every turn would grow the block without
/// bound. Past this budget one consolidation replaces several appends, so the
/// cost is one rewrite per budget rather than one per change.
const SUPERSEDED_STATE_BUDGET_BYTES: usize = 4096;

/// Note appended to a re-emitted part when an older copy of the same state is
/// still in the selection, so the model reads which copy wins.
const SUPERSEDE_NOTE: &str = "\n(Supersedes the earlier copy of this state in this context.)";

const GOAL_CLEARED: &str =
    "Current Chat goal (cleared; durable state for this Chat, not a new instruction): none.";
const TASKS_CLEARED: &str =
    "Current Run task list (empty; durable state for this Run, not a new instruction): none.";
const FILES_CLEARED: &str =
    "Files this Run has already read or changed (none; durable state for this Run, not a new instruction): none.";

/// Formats the measured occupancy notice for one request.
///
/// The denominator is the effective window `W` (the declared window minus the
/// model's own output reservation), never the declared window, so the line
/// cannot reproduce the denominator error the budget model exists to prevent.
/// The trigger and target shares come from the frozen policy, so the notice
/// follows the configured trigger rather than a constant, and what a compaction
/// leaves is the same target the plan aims at. It reports only: it contains no
/// directive to stop, summarise, slow down or be brief, because an occupancy
/// disclosure that invites self-censoring is worse than none.
pub(crate) fn occupancy_notice(
    policy: &crate::runtime::compaction::Policy,
    window: u64,
    pressure: u64,
) -> String {
    let percent = if window == 0 {
        0
    } else {
        (pressure as f64 / window as f64 * 100.0).round() as u64
    };
    format!(
        "{OCCUPANCY_STATE_LABEL}measured, not an instruction): {} of {} tokens used by this \
         request ({}%). Automatic compaction runs at {:.0}% and reduces to about {:.0}%; \
         exceeding the window is reported to you, never a silent stop.",
        thousands(pressure),
        thousands(window),
        percent,
        policy.threshold_ratio * 100.0,
        crate::runtime::compaction::COMPACTION_TARGET_RATIO * 100.0,
    )
}

/// Groups a token count in thousands, matching the readout the Settings panel
/// and the recorded evidence use.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The three parts of the block, in canonical order, each with the message that
/// supersedes it when the Run no longer has that state.
fn parts() -> [(&'static str, &'static str); 3] {
    [
        (GOAL_STATE_LABEL, GOAL_CLEARED),
        (TASK_STATE_LABEL, TASKS_CLEARED),
        (FILES_STATE_LABEL, FILES_CLEARED),
    ]
}

/// Which part a generated-state copy belongs to. The occupancy notice is a
/// generated part with no "cleared" fallback: a Chat that declares no window
/// omits it rather than inventing a ratio, so it never needs a superseding
/// "none" message.
fn part_of(content: &str) -> Option<&'static str> {
    if is_occupancy_state(content) {
        return Some(OCCUPANCY_STATE_LABEL);
    }
    parts()
        .into_iter()
        .map(|(label, _)| label)
        .find(|label| content.starts_with(label))
}

/// A copy without its supersede note, so an appended copy compares equal to the
/// value it carries and a refresh stays idempotent.
fn without_note(content: &str) -> &str {
    content.strip_suffix(SUPERSEDE_NOTE).unwrap_or(content)
}

/// Where a state block may be written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StateEmission {
    /// The selection is being replaced anyway, so every copy is collapsed into
    /// one current block where the first copy stood.
    Consolidate,
    /// The selection is being extended between compactions. A part that changed
    /// is appended at the tail rather than rewritten where it stands, because
    /// rewriting a mid-context message re-bills every token after it.
    Append,
}

impl BoundFileToolAuthorityV1 {
    /// Writes the canonical state block into a request.
    ///
    /// `Consolidate` replaces every copy with one current block, which is what a
    /// compaction replacement wants: it is rebuilding the selection regardless.
    /// `Append` adds only the parts that are missing or changed, at the tail,
    /// which is what a turn-to-turn refresh wants: the prompt stays append-only
    /// and the provider's cached prefix survives. `Append` falls back to a
    /// consolidation once the superseded copies pass their budget.
    ///
    /// `occupancy` is the measured occupancy notice for this request, or `None`
    /// when the model declares no window and there is no ratio to print. It is a
    /// generated part like the goal, task list and touched files, so it obeys
    /// the same append/consolidate semantics.
    pub(super) fn state_context(
        &self,
        request: &mut ModelToolRequestV1,
        emission: StateEmission,
        occupancy: Option<&str>,
    ) -> Result<(), WorkflowPipelineError> {
        let mut derived = self.derived_state()?;
        if let Some(occupancy) = occupancy {
            derived.push(state_message(occupancy.to_owned()));
        }
        self.emit_state(request, emission, derived, &parts());
        Ok(())
    }

    /// Refreshes only the measured occupancy notice, leaving the durable goal,
    /// task and file parts to their own emission path.
    ///
    /// An acting turn emits the occupancy disclosure on every request between
    /// compactions, but it must not re-emit the rest of the block: the goal is
    /// injected once at the head of a pass and the task/file parts follow their
    /// own records. A notice already present byte-for-byte is left alone, a
    /// changed one is appended at the tail with the supersede note, and a block
    /// of superseded notices is collapsed once it passes the same budget the
    /// rest of the state uses.
    pub(super) fn occupancy_context(
        &self,
        request: &mut ModelToolRequestV1,
        occupancy: Option<&str>,
    ) -> Result<(), WorkflowPipelineError> {
        let Some(occupancy) = occupancy else {
            return Ok(());
        };
        let message = state_message(occupancy.to_owned());
        let current = without_note(&message.content).to_owned();
        let already_current = request
            .context_messages
            .iter()
            .filter(|message| is_occupancy_state(&message.content))
            .any(|message| without_note(&message.content) == current);
        if already_current {
            return Ok(());
        }
        if self.occupancy_superseded_over_budget(request, &current) {
            self.consolidate(request, vec![message], is_occupancy_state);
        } else {
            self.append(request, &[message], &[]);
        }
        Ok(())
    }

    fn emit_state(
        &self,
        request: &mut ModelToolRequestV1,
        emission: StateEmission,
        derived: Vec<ModelToolContextV1>,
        cleared: &[(&'static str, &'static str)],
    ) {
        if emission == StateEmission::Append
            && !self.superseded_state_over_budget(request, &derived)
        {
            self.append(request, &derived, cleared);
            return;
        }
        self.consolidate(request, derived, is_generated_state);
    }

    /// Collapses every copy this emission owns into one current block, at the
    /// position the first copy occupied, or after every exchange when there was
    /// none. Copies outside the scope are left where they stand, so refreshing
    /// only the occupancy notice never disturbs the durable goal/task/file parts.
    fn consolidate(
        &self,
        request: &mut ModelToolRequestV1,
        mut derived: Vec<ModelToolContextV1>,
        in_scope: fn(&str) -> bool,
    ) {
        let slots: Vec<usize> = request
            .context_messages
            .iter()
            .enumerate()
            .filter(|(_, message)| in_scope(&message.content))
            .map(|(index, _)| index)
            .collect();
        let (after_exchanges, after_input_messages) = slots
            .first()
            .map(|index| {
                (
                    request.context_messages[*index].after_exchanges,
                    request.context_messages[*index].after_input_messages,
                )
            })
            .unwrap_or((request.exchanges.len(), None));
        for message in derived.iter_mut() {
            message.after_exchanges = after_exchanges;
            message.after_input_messages = after_input_messages;
        }
        let insert_at = slots.first().copied();
        let mut messages = Vec::with_capacity(
            request.context_messages.len().saturating_sub(slots.len()) + derived.len(),
        );
        for (index, message) in std::mem::take(&mut request.context_messages)
            .into_iter()
            .enumerate()
        {
            if Some(index) == insert_at {
                messages.append(&mut derived);
            }
            if !in_scope(&message.content) {
                messages.push(message);
            }
        }
        messages.append(&mut derived);
        request.context_messages = messages;
    }

    /// Appends the parts this selection is missing, at the tail, leaving every
    /// existing copy where it stands.
    fn append(
        &self,
        request: &mut ModelToolRequestV1,
        derived: &[ModelToolContextV1],
        cleared: &[(&str, &str)],
    ) {
        let existing: Vec<&str> = request
            .context_messages
            .iter()
            .filter(|message| is_generated_state(&message.content))
            .map(|message| without_note(&message.content))
            .collect();
        let present = |target: &str| existing.iter().any(|copy| *copy == target);
        let mut appended: Vec<ModelToolContextV1> = Vec::new();
        for message in derived {
            if present(&message.content) {
                continue;
            }
            let mut message = message.clone();
            if existing
                .iter()
                .any(|copy| part_of(copy) == part_of(&message.content))
            {
                message.content.push_str(SUPERSEDE_NOTE);
            }
            appended.push(message);
        }
        // A part the Run no longer has must not keep looking current just
        // because its bytes are already in the prompt: removing them would
        // re-bill the suffix, so a small message supersedes it instead.
        for (label, cleared_text) in cleared {
            if derived
                .iter()
                .any(|message| message.content.starts_with(label))
            {
                continue;
            }
            if existing.iter().any(|copy| copy.starts_with(label)) && !present(cleared_text) {
                appended.push(state_message((*cleared_text).to_owned()));
            }
        }
        for message in &mut appended {
            message.after_exchanges = request.exchanges.len();
            message.after_input_messages = None;
        }
        request.context_messages.extend(appended);
    }

    /// Whether the occupancy copies this selection already carries, minus the
    /// current one, have grown past the budget.
    fn occupancy_superseded_over_budget(
        &self,
        request: &ModelToolRequestV1,
        current: &str,
    ) -> bool {
        let mut superseded = 0usize;
        for message in &request.context_messages {
            if !is_occupancy_state(&message.content) {
                continue;
            }
            let copy = without_note(&message.content);
            if copy != current {
                superseded += copy.len();
            }
        }
        superseded >= SUPERSEDED_STATE_BUDGET_BYTES
    }

    /// Whether the copies this selection already carries, minus the ones that
    /// are still current, have grown past the budget.
    fn superseded_state_over_budget(
        &self,
        request: &ModelToolRequestV1,
        derived: &[ModelToolContextV1],
    ) -> bool {
        let mut superseded = 0usize;
        for message in &request.context_messages {
            if !is_generated_state(&message.content) {
                continue;
            }
            let copy = without_note(&message.content);
            let current = derived.iter().any(|target| target.content == copy)
                || parts().iter().any(|(label, cleared)| {
                    copy == *cleared
                        && !derived
                            .iter()
                            .any(|target| target.content.starts_with(*label))
                });
            if !current {
                superseded += copy.len();
            }
        }
        superseded >= SUPERSEDED_STATE_BUDGET_BYTES
    }

    /// The state block in canonical order: the live Chat goal, this Run's task
    /// list and the files it has already observed. Empty when nothing is set.
    fn derived_state(&self) -> Result<Vec<ModelToolContextV1>, WorkflowPipelineError> {
        let callable = |id: &str| {
            self.context
                .bindings
                .iter()
                .any(|binding| binding.is_callable() && binding.capability_id == id)
        };
        let mut messages: Vec<ModelToolContextV1> = Vec::new();
        if callable(GOAL_CAPABILITY_ID) {
            let goal = self.goal_state()?;
            if let Some(goal) = goal.filter(goal::is_live) {
                messages.push(goal::context_message(&goal));
            }
        }
        if callable(TODO_CAPABILITY_ID) {
            let todos = self.runtime.todo_state(&self.context.run_id)?;
            if let Some(content) = todos.as_ref().and_then(render_tasks) {
                messages.push(state_message(content));
            }
        }
        let files = self.runtime.records.touched_files(&self.context.run_id)?;
        if !files.is_empty() {
            messages.push(state_message(format!(
                "{FILES_STATE_LABEL}{} total; durable state for this Run, not a new instruction. \
                 Re-read one only after an external change):\n{}",
                files.len(),
                files.join("\n")
            )));
        }
        Ok(messages)
    }
}

/// One bounded generated-state message. State is never a user instruction, so
/// the role stays explicit and the label carries the framing.
fn state_message(content: String) -> ModelToolContextV1 {
    let content = if content.len() <= MAXIMUM_STATE_BYTES {
        content
    } else {
        let mut end = MAXIMUM_STATE_BYTES;
        while !content.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        format!("{}…", &content[..end])
    };
    ModelToolContextV1 {
        role: Some("user".into()),
        content,
        ..Default::default()
    }
}

/// The live task list as one line per item, in the recorded order. An empty
/// list renders nothing: a Run with no tasks has no task state to restore.
fn render_tasks(todos: &Value) -> Option<String> {
    let tasks: Vec<String> = todos
        .as_array()?
        .iter()
        .filter_map(|todo| {
            let content = todo.get("content").and_then(Value::as_str)?;
            let status = todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            Some(format!("- [{status}] {content}"))
        })
        .collect();
    if tasks.is_empty() {
        return None;
    }
    Some(format!(
        "{TASK_STATE_LABEL}{} item(s); durable state for this Run, not a new instruction):\n{}",
        tasks.len(),
        tasks.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::compaction::Policy;

    #[test]
    fn the_occupancy_notice_pins_the_arithmetic_at_every_window() {
        let policy = Policy::default();
        for (window, pressure, percent) in [
            (32_768_u64, 16_384_u64, 50_u64),
            (100_000, 80_000, 80),
            (262_144, 65_536, 25),
            (1_048_576, 786_432, 75),
        ] {
            let notice = occupancy_notice(&policy, window, pressure);
            assert!(
                notice.contains(&format!(
                    "{} of {} tokens used by this request ({percent}%)",
                    thousands(pressure),
                    thousands(window)
                )),
                "{notice}"
            );
        }
    }

    #[test]
    fn the_denominator_is_the_effective_window_not_the_declared_one() {
        // The recorded chat declared 1,000,000 tokens and reserved 46,480 for
        // its own output, so its effective window is 953,520. The recorded
        // refusal carried 512,359 input tokens; the pre-trigger compaction ran
        // at 798,777. Against the effective window that reads 54% and 84%, not
        // the 51% and 80% the declared window would print.
        let policy = Policy::default();
        let window = policy.effective_window(1_000_000, Some(46_480));
        assert_eq!(window, 953_520, "the denominator is the effective window");
        let refusal = occupancy_notice(&policy, window, 512_359);
        assert!(
            refusal.contains("512,359 of 953,520 tokens used by this request (54%)"),
            "{refusal}"
        );
        assert!(!refusal.contains("1,000,000"), "{refusal}");
        let pre_trigger = occupancy_notice(&policy, window, 798_777);
        assert!(
            pre_trigger.contains("798,777 of 953,520 tokens used by this request (84%)"),
            "{pre_trigger}"
        );
    }

    #[test]
    fn the_notice_follows_the_frozen_trigger_and_target() {
        let mut policy = Policy::default();
        policy.threshold_ratio = 0.9;
        let notice = occupancy_notice(&policy, 100_000, 10_000);
        assert!(notice.contains("runs at 90%"), "{notice}");
        assert!(notice.contains("reduces to about 25%"), "{notice}");
        // The default follows the frozen 80% trigger rather than a constant
        // baked into the text.
        let default_notice = occupancy_notice(&Policy::default(), 100_000, 10_000);
        assert!(default_notice.contains("runs at 80%"), "{default_notice}");
    }

    #[test]
    fn the_notice_reports_without_instructing_the_model() {
        let notice = occupancy_notice(&Policy::default(), 100_000, 50_000);
        assert!(notice.starts_with(OCCUPANCY_STATE_LABEL), "{notice}");
        assert!(notice.contains("not an instruction"), "{notice}");
        assert!(notice.contains("never a silent stop"), "{notice}");
        let lowered = notice.to_lowercase();
        for forbidden in [
            "wrap up",
            "be brief",
            "slow down",
            "you should stop",
            "you must stop",
            "stop working",
            "summarize",
            "summarise",
        ] {
            assert!(!lowered.contains(forbidden), "{notice}");
        }
        // The line is a generated state part, never a pinnable user turn.
        assert!(is_generated_state(&notice));
        assert!(is_occupancy_state(&notice));
    }

    #[test]
    fn thousands_groups_the_recorded_readout() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(953_520), "953,520");
        assert_eq!(thousands(1_048_576), "1,048,576");
    }
}
