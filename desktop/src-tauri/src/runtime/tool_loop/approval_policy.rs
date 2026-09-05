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
    let project_key = context.project_key.as_ref()?;
    let (scope, action_hash) = approvals::action_scope(&call.capability_id, &call.arguments);
    let binding_hash = approvals::digest(binding);
    let action_summary = if action_hash == "project_files" {
        format!(
            "All {} calls within this project's file boundary.",
            call.capability_id
        )
    } else {
        tool_approval_copy(call).1.chars().take(4096).collect()
    };
    Some(ProjectApprovalGrant {
        id: approvals::digest(&(project_key, &binding_hash, &action_hash)),
        project_key: project_key.clone(),
        project_name: context
            .project_name
            .clone()
            .unwrap_or_else(|| "Project".into()),
        capability_id: call.capability_id.clone(),
        scope,
        action_summary,
        binding_hash,
        action_hash,
    })
}

impl BoundFileToolAuthorityV1 {
    pub(super) fn review_tool_approval(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
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
        let grant = project_grant(&self.context.approvals, binding, call);
        let grants = store.grants().map_err(WorkflowPipelineError::Store)?;
        let saved = grant.as_ref().is_some_and(|expected| {
            grants.iter().any(|grant| {
                grant.id == expected.id
                    && grant.project_key == expected.project_key
                    && grant.binding_hash == expected.binding_hash
                    && grant.action_hash == expected.action_hash
            })
        });
        let mut pending = tool_approval_challenge(&challenge, call);
        pending.project_scope = grant.as_ref().map(|grant| grant.scope.clone());
        let approved = if mode == ApprovalMode::FullAccess || saved {
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
                    let exchanges: Vec<_> = self
                        .runtime
                        .records
                        .events("pipeline.model-tool-exchange")?
                        .into_iter()
                        .filter(|value| {
                            value.get("outerInvocationId").and_then(Value::as_str)
                                == Some(outer_invocation_id.as_str())
                        })
                        .collect();
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
                        &json!({"root":self.context.workspace.root,"project":self.context.approvals.project_name}),
                        cancellation,
                    );
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
            self.run_events.publish_approval_review(call, &review);
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
            Some(approved) => self.resolve_invoke_v1_inner(
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
