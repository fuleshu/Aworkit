//! A Chat is frozen once. Further input reuses that saved authority directly,
//! independently of current catalog descriptions, schemas and compiler output.
use super::*;

/// New pass identities and working state; never recompile the saved Run.
pub(super) fn prepare(
    request: &WorkflowExecutionRequestV1,
    mut saved: PreparedExecutionRecordV1,
) -> Result<PreparedExecutionRecordV1, WorkflowPipelineError> {
    saved.request_id = request.request_id.clone();
    saved.approvals.mode = request.approvals.mode;
    let budget = &saved.snapshot.budget;
    saved.deadline_epoch_millis = if budget.deadline_ms == NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS {
        NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
    } else {
        request.now_epoch_millis.saturating_add(budget.deadline_ms)
    };
    let budget_ref = digest_id("budget", request.request_id.as_str())?;
    let mut config = saved.agent_checkpoint.config.clone();
    config.loop_id = digest_id("agent.loop", request.request_id.as_str())?;
    config.budget_ref = budget_ref.clone();
    saved.limit_checkpoint = LimitLedger::new(
        config.scope_id.clone(),
        BudgetEnvelope {
            turns: budget.turns,
            attempts: budget.attempts,
            tool_calls: budget.tool_calls,
            tokens: budget.tokens,
            cost_micros: budget.cost_micros,
            actions: budget.actions,
            max_depth: budget.depth,
            max_fan_out: budget.fanout,
            max_parallel: budget.parallel,
            deadline_tick: budget.deadline_ms,
        },
    )
    .map_err(|e| WorkflowPipelineError::Worker(e.to_string()))?
    .checkpoint();
    saved.agent_checkpoint = AgentLoopV1::new(config)
        .map_err(|e| WorkflowPipelineError::Worker(e.to_string()))?
        .checkpoint();
    saved.scheduler_checkpoint = None;
    saved.scheduler_trace.clear();
    saved.scheduler_continuation = 0;
    saved.agent_token_id = None;
    saved.worker_proposal.invocation_id = digest_id("pass", request.request_id.as_str())?;
    saved.worker_proposal.attempt_id = digest_id("pass.attempt", request.request_id.as_str())?;
    saved.worker_proposal.budget_ref = budget_ref;
    saved.worker_proposal.payload["context"] = json!({"messages":request.messages});
    saved.worker_proposal.payload["compactNode"] = json!(request.compact_node);
    saved.worker_proposal.payload["steerFromRequestId"] = json!(request.steer_from_request_id);
    saved.broker_proposal.proposal_id = saved.worker_proposal.invocation_id.clone();
    saved.broker_proposal.payload_hash = canonical_hash(&saved.worker_proposal.payload)?;
    enforce_serialized_bound(
        &saved,
        MAXIMUM_PREPARED_RECORD_BYTES,
        "prepared execution record",
    )?;
    Ok(saved)
}

/// A duplicate command returns its original result. Only its actual turn input
/// and ownership identify it; echoed Settings/catalog metadata is not authority.
pub(super) fn validate_replay(
    request: &WorkflowExecutionRequestV1,
    saved: &PreparedExecutionRecordV1,
) -> Result<(), WorkflowPipelineError> {
    if saved.request_id != request.request_id
        || saved.snapshot.chat_id != request.chat_id
        || saved.snapshot.run_id != request.run_id
        || saved.worker_proposal.payload["context"]["messages"] != json!(request.messages)
        || saved.worker_proposal.payload["compactNode"] != json!(request.compact_node)
        || saved.worker_proposal.payload["steerFromRequestId"]
            != json!(request.steer_from_request_id)
    {
        return Err(WorkflowPipelineError::Store(
            "request ID was reused for different Chat input".into(),
        ));
    }
    Ok(())
}

/// Validate new conversation input without resolving tools or live configuration.
pub(super) fn validate_messages(
    request: &WorkflowExecutionRequestV1,
) -> Result<(), WorkflowPipelineError> {
    if request.messages.iter().any(|m| {
        !matches!(m.role.as_str(), "user" | "assistant")
            || (m.content.is_empty() && m.images.is_empty())
            || m.content.contains('\0')
            || (!m.images.is_empty() && m.role != "user")
    }) || request.messages.last().is_none_or(|m| m.role != "user")
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "messages require supported roles, non-empty content, and a final user turn".into(),
        ));
    }
    aworkit_capability_host::model_images::validate_image_attachments(
        &request
            .messages
            .iter()
            .flat_map(|m| m.images.clone())
            .collect::<Vec<_>>(),
    )
    .map_err(|e| WorkflowPipelineError::InvalidInput(e.to_string()))
}
