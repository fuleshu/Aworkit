//! Durable Run state re-emitted after a compaction replaced the transcript.
//!
//! A summary is a lossy projection, so the one goal, the task list and the
//! files this Run already observed must not depend on the summariser keeping
//! them. They are re-derived from the same records the tools wrote, which also
//! means a compaction can never carry a stale copy forward: the newest record
//! is the live state. Workspace instructions and the host clock are not
//! duplicated here; they have their own restoration path in
//! `workspace_context`.

use super::*;
use crate::runtime::compaction::{FILES_STATE_LABEL, TASK_STATE_LABEL};
use aworkit_capability_host::ModelToolRequestV1;

/// Longest state message one compaction appends. The content is generated from
/// records, so the bound is a formatting guard rather than a real truncation.
const MAXIMUM_STATE_BYTES: usize = 2048;

impl BoundFileToolAuthorityV1 {
    /// Writes the canonical state block into a request, either as a compaction
    /// append or as a refresh of the copy a restored checkpoint already carries.
    ///
    /// A block that is already present is rewritten where it stands, so a
    /// refresh never reorders the committed selection; only a request without
    /// one appends after every exchange. Either way exactly one current copy
    /// remains: the goal, the task list and the touched files are re-derived
    /// from their records, never carried forward as prose.
    pub(super) fn state_context(
        &self,
        request: &mut ModelToolRequestV1,
    ) -> Result<(), WorkflowPipelineError> {
        let mut derived = self.derived_state()?;
        let slots: Vec<usize> = request
            .context_messages
            .iter()
            .enumerate()
            .filter(|(_, message)| crate::runtime::compaction::is_generated_state(&message.content))
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
        for (index, message) in derived.iter_mut().enumerate() {
            let position = slots
                .get(index)
                .map(|slot| {
                    (
                        request.context_messages[*slot].after_exchanges,
                        request.context_messages[*slot].after_input_messages,
                    )
                })
                .unwrap_or((after_exchanges, after_input_messages));
            message.after_exchanges = position.0;
            message.after_input_messages = position.1;
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
                messages.extend(derived.drain(..));
            }
            if !crate::runtime::compaction::is_generated_state(&message.content) {
                messages.push(message);
            }
        }
        messages.extend(derived);
        request.context_messages = messages;
        Ok(())
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
