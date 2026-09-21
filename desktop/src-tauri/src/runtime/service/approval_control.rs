//! Desktop-only commands for approval preferences and saved project rules.

use super::*;
use crate::runtime::approvals::{
    ApprovalChoice, ApprovalMode, ApprovalResolution, ProjectApprovalGrant,
};
use crate::runtime::tool_loop::question::QuestionAnswerV1;

pub(crate) fn parse_approval_resolution(payload: &Value) -> Result<ApprovalResolution, String> {
    let resolution = if let Some(choice) = payload.get("choice") {
        let resolution = ApprovalResolution {
            filesystem: payload
                .get("filesystem")
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value(value.clone())
                        .map_err(|_| "Invalid filesystem permission")
                })
                .transpose()?,
            choice: serde_json::from_value(choice.clone())
                .map_err(|_| "Invalid approval choice")?,
            reason: payload
                .get("reason")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .map(|reason| reason.trim().to_owned())
                        .ok_or("Approval reason must be text")
                })
                .transpose()?,
        };
        if resolution.choice == ApprovalChoice::Deny
            && resolution.reason.as_deref().is_none_or(str::is_empty)
        {
            return Err("Give a reason for denying this action.".into());
        }
        if let Some(approved) = payload.get("approved")
            && approved.as_bool() != Some(resolution.approved())
        {
            return Err("Conflicting approval decision fields.".into());
        }
        resolution
    } else {
        if payload
            .get("filesystem")
            .is_some_and(|value| !value.is_null())
        {
            return Err("Filesystem permissions require an explicit approval choice.".into());
        }
        ApprovalResolution::once(
            payload
                .get("approved")
                .and_then(Value::as_bool)
                .ok_or("approval command requires a choice or boolean approved field")?,
        )
    };
    resolution.validate()?;
    Ok(resolution)
}

impl DesktopRuntime {
    pub fn filesystem_approval_grants(
        &self,
    ) -> Result<Vec<crate::runtime::approvals::FilesystemGrant>, String> {
        self.approvals.filesystem_grants()
    }

    pub fn revoke_filesystem_approval(&self, id: &str) -> Result<(), String> {
        self.approvals.revoke_filesystem(id)
    }
    pub fn project_approval_grants(&self) -> Result<Vec<ProjectApprovalGrant>, String> {
        self.approvals.grants()
    }

    pub fn revoke_project_approval(&self, id: &str) -> Result<(), String> {
        self.approvals.revoke(id)
    }

    pub(super) fn change_approval_mode(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(input.expected_version)?;
        let mode: ApprovalMode = serde_json::from_value(
            input
                .payload
                .get("mode")
                .cloned()
                .ok_or("Approval mode is required")?,
        )
        .map_err(|_| "Unknown approval mode")?;
        let chat_id = self.history.snapshot(0)?.chat.chat_id;
        self.approvals.set_mode(&chat_id, mode)?;
        self.history.append(
            &input.command_id,
            &fingerprint,
            input.expected_version,
            vec![(
                "approval.mode_changed",
                json!({"mode":mode,"chatId":chat_id,"createdAt":now_label()}),
            )],
        )
    }
}


/// Parses one typed user answer from a question command payload. The answer is
/// only a value: whether it is acceptable for the question it answers is
/// decided against the durable question, never by the renderer.
pub(crate) fn parse_question_answer(payload: &Value) -> Result<QuestionAnswerV1, String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "question command payload must be an object".to_owned())?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let cancelled = object
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(QuestionAnswerV1 {
        option_id: text("optionId"),
        free_text: text("freeText"),
        path: text("path"),
        cancelled,
    })
}
