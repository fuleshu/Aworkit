//! Desktop-only commands for approval preferences and saved project rules.

use super::*;
use crate::runtime::approvals::{
    ApprovalChoice, ApprovalMode, ApprovalResolution, ProjectApprovalGrant,
};

pub(crate) fn parse_approval_resolution(payload: &Value) -> Result<ApprovalResolution, String> {
    let resolution = if let Some(choice) = payload.get("choice") {
        let resolution = ApprovalResolution {
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
