//! A user's answer to a model question resumes the suspended pass.
//!
//! A question reuses the approval suspension, so resuming it re-enters the same
//! pass and the same pending tool call. The answer is recorded durably before
//! that resume, which is what makes it deliverable exactly once: a replayed
//! resume reads the recorded answer instead of asking again, and an unanswered
//! question is never answered on the user's behalf.

use super::*;
use crate::runtime::tool_loop::question::{self, QuestionAnswerV1};

impl WorkflowExecutionPipeline {
    /// Validates one answer against the exact question this Chat is waiting on.
    pub(crate) fn validate_question_target(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &QuestionAnswerV1,
    ) -> Result<(), WorkflowPipelineError> {
        let pending = self.records.pending_approval(question_id)?.ok_or_else(|| {
            WorkflowPipelineError::InvalidInput("Unknown question".into())
        })?;
        if pending.chat_id != chat_id {
            return Err(WorkflowPipelineError::InvalidInput(
                "Question does not belong to the active Chat".into(),
            ));
        }
        let asked = pending
            .agent_loop
            .as_ref()
            .and_then(|agent| agent.pending.challenge.question.clone())
            .ok_or_else(|| {
                WorkflowPipelineError::InvalidInput("That suspension is not a question".into())
            })?;
        if let Some(recorded) = self.file_tool_authority.question_answer_for(question_id)? {
            let recorded: QuestionAnswerV1 = serde_json::from_value(recorded).map_err(|error| {
                WorkflowPipelineError::Store(format!("recorded question answer is invalid: {error}"))
            })?;
            // A repeated delivery of the same answer is a replay, not a change.
            return if recorded == *answer {
                Ok(())
            } else {
                Err(WorkflowPipelineError::InvalidInput(
                    "Question already has a different answer".into(),
                ))
            };
        }
        question::validate_answer(&asked, answer).map_err(WorkflowPipelineError::InvalidInput)
    }

    /// Delivers one user answer and resumes the pass that asked the question.
    pub(crate) fn resume_question(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &QuestionAnswerV1,
    ) -> Result<WorkflowExecutionResultV1, WorkflowPipelineError> {
        self.validate_question_target(question_id, chat_id, answer)?;
        if self.file_tool_authority.question_answer_for(question_id)?.is_some() {
            // The same answer is being delivered again, which is a replay of a
            // decision already applied: the committed outcome is returned and
            // the pass is never resumed a second time.
            let pending = self.records.pending_approval(question_id)?.ok_or_else(|| {
                WorkflowPipelineError::InvalidInput("Unknown question".into())
            })?;
            let prepared = self
                .records
                .execution(&stable(&pending.request_id)?)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            return self
                .recover_committed_approval(&prepared, &pending)?
                .ok_or_else(|| {
                    WorkflowPipelineError::Store("the question was already answered".into())
                });
        }
        let encoded = serde_json::to_value(answer)
            .map_err(|error| WorkflowPipelineError::Store(error.to_string()))?;
        self.file_tool_authority.record_question_answer_for(question_id, &encoded)?;
        // The answer is durable. The resumed pass re-invokes the same tool call,
        // which finds it and settles without executing anything, so no question
        // is asked twice and no answer is consumed twice.
        self.resume_approval_committed(question_id, true)
    }
}
