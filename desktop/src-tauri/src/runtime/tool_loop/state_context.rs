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
    is_generated_state, FILES_STATE_LABEL, GOAL_STATE_LABEL, TASK_STATE_LABEL,
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

/// The three parts of the block, in canonical order, each with the message that
/// supersedes it when the Run no longer has that state.
fn parts() -> [(&'static str, &'static str); 3] {
    [
        (GOAL_STATE_LABEL, GOAL_CLEARED),
        (TASK_STATE_LABEL, TASKS_CLEARED),
        (FILES_STATE_LABEL, FILES_CLEARED),
    ]
}

/// Which part a generated-state copy belongs to.
fn part_of(content: &str) -> Option<&'static str> {
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
    pub(super) fn state_context(
        &self,
        request: &mut ModelToolRequestV1,
        emission: StateEmission,
    ) -> Result<(), WorkflowPipelineError> {
        let derived = self.derived_state()?;
        if emission == StateEmission::Append
            && !self.superseded_state_over_budget(request, &derived)
        {
            self.append_state(request, &derived);
            return Ok(());
        }
        self.consolidate_state(request, derived);
        Ok(())
    }

    /// Collapses every copy into one current block, at the position the first
    /// copy occupied, or after every exchange when there was none.
    fn consolidate_state(
        &self,
        request: &mut ModelToolRequestV1,
        mut derived: Vec<ModelToolContextV1>,
    ) {
        let slots: Vec<usize> = request
            .context_messages
            .iter()
            .enumerate()
            .filter(|(_, message)| is_generated_state(&message.content))
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
            if !is_generated_state(&message.content) {
                messages.push(message);
            }
        }
        messages.append(&mut derived);
        request.context_messages = messages;
    }

    /// Appends the parts this selection is missing, at the tail, leaving every
    /// existing copy where it stands.
    fn append_state(&self, request: &mut ModelToolRequestV1, derived: &[ModelToolContextV1]) {
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
        for (label, cleared) in parts() {
            if derived
                .iter()
                .any(|message| message.content.starts_with(label))
            {
                continue;
            }
            if existing.iter().any(|copy| copy.starts_with(label)) && !present(cleared) {
                appended.push(state_message(cleared.to_owned()));
            }
        }
        for message in &mut appended {
            message.after_exchanges = request.exchanges.len();
            message.after_input_messages = None;
        }
        request.context_messages.extend(appended);
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
