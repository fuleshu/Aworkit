//! Validated user decisions enter the pipeline before any resumed effect.

use super::*;
use crate::runtime::approvals::{ApprovalChoice, ApprovalResolution};

impl WorkflowExecutionPipeline {
    pub(crate) fn validate_approval_choice(
        &self,
        decision_id: &str,
        resolution: &ApprovalResolution,
    ) -> Result<(), WorkflowPipelineError> {
        resolution
            .validate()
            .map_err(WorkflowPipelineError::InvalidInput)?;
        if resolution.choice == ApprovalChoice::AlwaysApproveInProject {
            let pending = self
                .records
                .pending_approval(decision_id)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            let prepared = self
                .records
                .execution(&stable(&pending.request_id)?)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            if pending.agent_loop.is_none() || prepared.approvals.project_key.is_none() {
                return Err(WorkflowPipelineError::InvalidInput(
                    "Only a tool action in a selected project can create a project approval".into(),
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn validate_approval_target(
        &self,
        decision_id: &str,
        chat_id: &str,
    ) -> Result<(), WorkflowPipelineError> {
        let pending = self.records.pending_approval(decision_id)?.ok_or_else(|| {
            WorkflowPipelineError::InvalidInput("Unknown approval decision".into())
        })?;
        if pending.chat_id != chat_id || self.records.approval_resolved(decision_id)? {
            return Err(WorkflowPipelineError::InvalidInput(
                "Approval does not belong to the active Chat or was already resolved".into(),
            ));
        }
        Ok(())
    }

    /// Compatibility entry point for already persisted boolean decisions.
    pub fn resume_approval(
        &self,
        decision_id: &str,
        approved: bool,
    ) -> Result<WorkflowExecutionResultV1, WorkflowPipelineError> {
        if self.records.approval_resolved(decision_id)? {
            return Err(WorkflowPipelineError::InvalidInput(
                "Approval decision was already applied".into(),
            ));
        }
        self.resume_approval_choice(decision_id, &ApprovalResolution::once(approved))
    }

    pub(crate) fn resume_approval_choice(
        &self,
        decision_id: &str,
        resolution: &ApprovalResolution,
    ) -> Result<WorkflowExecutionResultV1, WorkflowPipelineError> {
        resolution
            .validate()
            .map_err(WorkflowPipelineError::InvalidInput)?;
        let store = &self.file_tool_authority.approvals;
        if let Some(previous) = store
            .resolution(decision_id)
            .map_err(WorkflowPipelineError::Store)?
        {
            if previous != *resolution {
                return Err(WorkflowPipelineError::InvalidInput(
                    "Approval already has a different decision".into(),
                ));
            }
            if let Some(mut result) = store
                .result(decision_id)
                .map_err(WorkflowPipelineError::Store)?
            {
                result.replayed = true;
                return Ok(result);
            }
        }
        let pending = self.records.pending_approval(decision_id)?.ok_or_else(|| {
            WorkflowPipelineError::InvalidInput("Unknown approval decision".into())
        })?;
        let prepared = self
            .records
            .execution(&stable(&pending.request_id)?)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        if let Some(result) = self.recover_committed_approval(&prepared, &pending)? {
            store
                .save_result(decision_id, &result)
                .map_err(WorkflowPipelineError::Store)?;
            self.records.mark_approval_resolved(&stable(decision_id)?)?;
            return Ok(result);
        }
        if self.records.approval_resolved(decision_id)? {
            return Err(WorkflowPipelineError::IncompleteEvidence);
        }
        let grant = if resolution.choice == ApprovalChoice::AlwaysApproveInProject {
            let agent = pending.agent_loop.as_ref().ok_or_else(|| {
                WorkflowPipelineError::InvalidInput(
                    "Workflow approval steps cannot create tool permissions".into(),
                )
            })?;
            let call = &agent.pending.call;
            let binding = prepared
                .tool_bindings
                .iter()
                .find(|binding| binding.capability_id == call.capability_id)
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            Some(
                super::super::tool_loop::approval_policy::project_grant(
                    &prepared.approvals,
                    binding,
                    call,
                )
                .ok_or_else(|| {
                    WorkflowPipelineError::InvalidInput(
                        "Always approve requires a selected project".into(),
                    )
                })?,
            )
        } else {
            None
        };
        let workspace = prepared
            .workspace
            .as_ref()
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        self.projects
            .revalidate_workspace_v1(workspace)
            .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;
        self.file_tool_authority
            .approvals
            .resolve(decision_id, resolution, grant.as_ref())
            .map_err(WorkflowPipelineError::Store)?;
        self.resume_approval_committed(decision_id, resolution.approved())
    }

    /// Covers a crash after the provider outcome or next gate was durable but
    /// before the UI-facing approval receipt was written. No provider is called.
    fn recover_committed_approval(
        &self,
        prepared: &PreparedExecutionRecordV1,
        pending: &PendingGraphPassStateV1,
    ) -> Result<Option<WorkflowExecutionResultV1>, WorkflowPipelineError> {
        let invocation_id = stable(&pending.invocation_id)?;
        let outcome = self.records.outcome(&invocation_id)?;
        let next = self
            .records
            .pending_approvals()?
            .into_iter()
            .skip_while(|candidate| candidate.decision_id != pending.decision_id)
            .skip(1)
            .filter(|candidate| candidate.invocation_id == pending.invocation_id)
            .last();
        if outcome.is_none() && next.is_none() {
            return Ok(None);
        }
        let mut result = WorkflowExecutionResultV1 {
            request_id: prepared.request_id.clone(),
            chat_id: prepared.snapshot.chat_id.clone(),
            run_id: prepared.snapshot.run_id.clone(),
            snapshot_id: prepared.snapshot.snapshot_id.clone(),
            snapshot_hash: prepared.snapshot.snapshot_hash.clone(),
            authority_manifest_id: prepared.manifest.manifest_id.clone(),
            worker_invocation_id: prepared.worker_proposal.invocation_id.clone(),
            broker_invocation_id: invocation_id.clone(),
            outcome_hash: String::new(),
            status: WorkflowExecutionStatusV1::AwaitingApproval,
            assistant_text: None,
            reasoning: None,
            error: None,
            model: prepared.provider.model.clone(),
            input_units: 0,
            output_units: 0,
            model_turns: 0,
            tool_calls: 0,
            tool_activity: Vec::new(),
            node_activity: Vec::new(),
            approval: None,
            replayed: true,
        };
        if let Some(outcome) = outcome {
            let broker = DurableInvocationBroker::new(self.ledger.clone(), APPROVAL_TTL_MILLIS);
            self.reconcile_persisted_outcomes(&broker)?;
            result.outcome_hash = self
                .ledger
                .settlement(&invocation_id)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?
                .0;
            result.status = outcome.status;
            result.assistant_text = outcome.assistant_text;
            result.reasoning = outcome.reasoning;
            result.error = outcome.error;
            result.model = outcome.model;
            result.input_units = outcome.input_units;
            result.output_units = outcome.output_units;
            result.model_turns = u64::from(outcome.attempted_model_turns);
            result.tool_calls = u64::from(outcome.settled_tool_calls);
            result.tool_activity = outcome.tool_activity;
            result.node_activity = outcome.node_activity;
        } else if let Some(next) = next {
            result.input_units = next.input_units;
            result.output_units = next.output_units;
            result.model_turns = u64::from(next.attempted_model_turns);
            result.tool_calls = u64::from(next.settled_tool_calls);
            result.tool_activity = next.tool_activity;
            result.node_activity = next.activity;
            result.reasoning = next.reasoning_body.map(|body| WorkflowReasoningActivityV1 {
                body,
                category: next
                    .reasoning_category
                    .unwrap_or_else(|| "source_provided".into()),
            });
            result.approval = Some(GraphApprovalRequestV1 {
                project_scope: next
                    .agent_loop
                    .as_ref()
                    .and_then(|agent| agent.pending.challenge.project_scope.clone()),
                decision_id: next.decision_id,
                node_id: next.pending_node_id,
                title: next.title,
                message: next.message,
            });
        }
        Ok(Some(result))
    }
}
