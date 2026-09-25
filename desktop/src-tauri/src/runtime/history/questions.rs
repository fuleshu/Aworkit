//! Question lifetime follows its reply, including legacy stops without cancellation facts.
use super::*;

pub(super) fn ends_questions(kind: &str) -> bool {
    matches!(
        kind,
        "chat.turn_stopped" | "chat.cancelled" | "execution.failed" | "context.compaction-ended"
    )
}

/// Fold once so stopping several questions never rescans the whole Chat per question.
pub(super) fn open_questions<'a>(events: impl Iterator<Item = &'a Event>) -> BTreeSet<String> {
    let mut open = BTreeSet::new();
    for event in events {
        if ends_questions(&event.kind) {
            open.clear();
        } else if let Some(id) = event.payload["questionId"].as_str() {
            match event.kind.as_str() {
                "question.asked" => {
                    open.insert(id.to_owned());
                }
                "question.answered" | "question.cancelled" => {
                    open.remove(id);
                }
                _ => {}
            }
        }
    }
    open
}

impl ChatHistory {
    /// A recorded answer may recover only in its original, still-live reply.
    pub(crate) fn ensure_question_resumable(
        &self,
        question_id: &str,
        command_id: &str,
    ) -> Result<(), String> {
        let events = self.events()?;
        let mut asked = false;
        let mut closed = false;
        for event in events.iter() {
            if event.payload["questionId"].as_str() == Some(question_id) {
                match event.kind.as_str() {
                    "question.asked" => {
                        asked = true;
                        closed = false;
                    }
                    "question.answered" | "question.cancelled" => {
                        closed |= event.payload["requestId"].as_str() != Some(command_id);
                    }
                    _ => {}
                }
            }
            if asked && ends_questions(&event.kind) {
                closed = true;
            }
        }
        if !asked || closed {
            return Err("This question is no longer waiting for an answer. Stop the interrupted reply to send a new message.".into());
        }
        Ok(())
    }

    /// Stop is not Skip: close the question without resuming the model.
    pub(crate) fn cancel_open_questions(
        &self,
        created_at: &str,
    ) -> Result<Vec<(&'static str, Value)>, String> {
        let events = self.events()?;
        Ok(open_questions(events.iter().map(AsRef::as_ref)).into_iter().map(|id| (
            "question.cancelled", json!({"questionId":id,"cancelled":true,"createdAt":created_at,"reason":"Reply stopped."}),
        )).collect())
    }
}
