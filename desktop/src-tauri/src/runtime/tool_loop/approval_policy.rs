//! Shared approval routing for every brokered built-in and MCP invocation.

use super::*;
use crate::runtime::approvals::{
    self, ApprovalMode, ProjectApprovalGrant, reviewer::ReviewOutcome,
};

pub(crate) fn project_grant(
    context: &approvals::ApprovalContext,
    binding: &StoredFileToolBindingV1,
    call: &ModelToolCallV1,
) -> Option<ProjectApprovalGrant> {
    // Workspace operations need no standing grant. External file operations
    // must never inherit a permission labelled "in project".
    if binding.file_access_version.is_some() || binding.capability_id == jobs::KEEP {
        return None;
    }
    let project_key = context.project_key.as_ref()?;
    let binding_hash = approvals::permission_identity::binding_hash(binding);
    let mut grant = ProjectApprovalGrant {
        id: String::new(),
        project_key: project_key.clone(),
        project_name: context
            .project_name
            .clone()
            .unwrap_or_else(|| "Project".into()),
        capability_id: call.capability_id.clone(),
        scope: String::new(),
        action_summary: String::new(),
        binding_hash,
        action_hash: String::new(),
    };
    grant.set_tool_scope();
    Some(grant)
}

impl BoundFileToolAuthorityV1 {
    pub(super) fn review_tool_approval(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        proposal_id: &StableId,
        challenge: ApprovalChallengeV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        if cancellation.is_cancelled() {
            return Err(WorkflowPipelineError::Host(
                "Approval review cancelled".into(),
            ));
        }
        let store = &self.runtime.approvals;
        let mode = store
            .mode(&self.context.approvals.chat_id, self.context.approvals.mode)
            .map_err(WorkflowPipelineError::Store)?;
        let binding = self
            .context
            .bindings
            .iter()
            .find(|binding| binding.capability_id == call.capability_id)
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let mode = binding.options.approval_mode.unwrap_or(mode);
        let grant = project_grant(&self.context.approvals, binding, call);
        let grants = store.grants().map_err(WorkflowPipelineError::Store)?;
        let saved = grant.as_ref().is_some_and(|expected| {
            grants.iter().any(|grant| {
                grant.id == expected.id
                    && grant.project_key == expected.project_key
                    && grant.capability_id == expected.capability_id
                    && grant.binding_hash == expected.binding_hash
                    && grant.action_hash == expected.action_hash
            })
        });
        let mut pending = tool_approval_challenge(&challenge, call);
        pending.project_scope = grant.as_ref().map(|grant| grant.scope.clone());
        let file_access = self
            .runtime
            .records
            .invocation(proposal_id)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?
            .file_access;
        let mut filesystem_saved = false;
        if let Some(Ok(access)) = &file_access {
            if access.outside_workspace {
                let required = filesystem_permissions::required_access(&call.capability_id);
                pending.filesystem = Some(approvals::FilesystemApprovalRequest {
                    access: required,
                    directory: access.directory.root.to_string_lossy().into_owned(),
                });
                pending.project_scope = self.context.approvals.project_key.as_ref().map(|_| {
                    "Selected filesystem permission across all chats in this project".into()
                });
                let permissions = store
                    .filesystem_grants()
                    .map_err(WorkflowPipelineError::Store)?;
                if let Some(permission) = permissions.iter().find(|grant| {
                    grant.permits(
                        &self.context.approvals,
                        required,
                        &access.target(),
                        &self.runtime.projects,
                    )
                }) {
                    filesystem_saved = true;
                    self.run_events
                        .publish_filesystem_permission(call, permission);
                }
                pending.summary.push_str(&format!(
                    "\n\nResolved path: {}\nThis file is outside the Chat workspace.",
                    access.display_target()
                ));
            }
        }
        let local_file = file_access.as_ref().is_some_and(|access| {
            access
                .as_ref()
                .map_or(true, |access| !access.outside_workspace)
        });
        let approved = if local_file
            || mode == ApprovalMode::FullAccess
            || saved
            || filesystem_saved
        {
            Some(true)
        } else if mode == ApprovalMode::ApproveForMe {
            let review = match store
                .review(challenge.invocation_id.as_str())
                .map_err(WorkflowPipelineError::Store)?
            {
                Some(review) => review,
                None => {
                    self.run_events.publish_tool_waiting(
                        call,
                        "Reviewing approval automatically.".into(),
                        json!({"approvalMode":"approve_for_me"}),
                    );
                    let gateway = self
                        .context
                        .model_gateway
                        .as_ref()
                        .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                    let records = &self.runtime.records;
                    let exchanges = records.cache.recent(&records.store, TOOL_RECORD_CHAT_ID, STORE_BRANCH_ID,
                        "pipeline.model-tool-exchange", approvals::review_context::RECENT_EXCHANGES, |event| {
                            let record = event.payload.get("record")?;
                            (record["outerInvocationId"] == outer_invocation_id.as_str())
                                .then(|| approvals::review_context::exchange_evidence(record))
                        }).map_err(WorkflowPipelineError::Store)?;
                    let review = approvals::review_action(
                        gateway,
                        self.context
                            .model_binding_id
                            .as_deref()
                            .ok_or(WorkflowPipelineError::IncompleteEvidence)?,
                        self.context
                            .model_version_hash
                            .as_deref()
                            .ok_or(WorkflowPipelineError::IncompleteEvidence)?,
                        &self.context.review_messages,
                        &exchanges,
                        call,
                        &json!({"root":self.context.workspace.root,"project":self.context.approvals.project_name,"fileAccess":file_access}),
                        cancellation,
                    );
                    // One accounting event per actual review attempt, including
                    // cancellation. Reusing a stored decision incurs no new usage.
                    self.run_events.publish_approval_review(call, &review);
                    if cancellation.is_cancelled() {
                        return Err(WorkflowPipelineError::Host(
                            "Approval review cancelled".into(),
                        ));
                    }
                    store
                        .save_review(challenge.invocation_id.as_str(), &review)
                        .map_err(WorkflowPipelineError::Store)?;
                    review
                }
            };
            pending
                .summary
                .push_str(&format!("\n\nAutomatic review: {}", review.reason));
            match review.decision {
                ReviewOutcome::Approve => Some(true),
                ReviewOutcome::Deny => Some(false),
                ReviewOutcome::AskUser => None,
            }
        } else {
            None
        };
        match approved {
            Some(approved) => self.resolve_invoke_v1_with_delivery(
                outer_invocation_id,
                turn,
                call,
                &ApprovalResponseV1 {
                    invocation_id: challenge.invocation_id,
                    nonce: challenge.nonce,
                    approved,
                    now_epoch_millis: current_epoch_millis(),
                },
                cancellation,
            ),
            None => Err(WorkflowPipelineError::ToolApproval(pending)),
        }
    }

    pub(super) fn denial_reason(
        &self,
        invocation_id: &StableId,
    ) -> Result<String, WorkflowPipelineError> {
        let store = &self.runtime.approvals;
        if let Some(resolution) = store
            .resolution(invocation_id.as_str())
            .map_err(WorkflowPipelineError::Store)?
        {
            return Ok(format!(
                "The user denied this action. {} Do not retry it or pursue the same outcome through a workaround. Follow the user's reason and choose a materially safer alternative or ask the user.",
                resolution.reason.as_deref().unwrap_or("")
            ));
        }
        if let Some(review) = store
            .review(invocation_id.as_str())
            .map_err(WorkflowPipelineError::Store)?
        {
            return Ok(format!(
                "Automatic approval review denied this action: {} Do not retry it or bypass the decision. Choose a materially safer alternative, or stop and ask the user.",
                review.reason
            ));
        }
        Ok(
            "The user denied this tool invocation. Do not retry it without explicit user approval."
                .into(),
        )
    }
}
