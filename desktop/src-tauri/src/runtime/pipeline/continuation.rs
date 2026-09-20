//! A later pass of an existing Chat adopts the current documents. Node, edge and
//! agent tool ids come from the workflow document the user has now, and tool
//! interfaces come from this build. Only Chat identity, the Run's authority
//! ceiling, the workspace binding and committed evidence carry over.
use super::*;

/// New pass identities and working state resolved from the current documents.
///
/// The saved record still owns the Chat identity, the authority ceiling, the
/// workspace binding and the recorded position; everything the documents own is
/// resolved again here, so a tool the user enabled mid-Chat is offered to the
/// model in the very next pass without a new Chat.
pub(super) fn prepare(
    request: &WorkflowExecutionRequestV1,
    mut saved: PreparedExecutionRecordV1,
) -> Result<PreparedExecutionRecordV1, WorkflowPipelineError> {
    let workflow = current_workflow_document(request, &saved)?;
    saved.request_id = request.request_id.clone();
    saved.approvals.mode = request.approvals.mode;
    let budget = &saved.snapshot.budget;
    saved.deadline_epoch_millis = if budget.deadline_ms == NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS {
        NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
    } else {
        request.now_epoch_millis.saturating_add(budget.deadline_ms)
    };
    // The capabilities this pass may call are the ones the current document
    // binds. The saved set supplies authority only: a capability the Run already
    // held keeps its frozen approval class, executable identity and credential
    // binding, while a capability the Chat never held resolves through the same
    // freeze path as a first input, so the broker still settles every invocation
    // at its point of use instead of silently granting it.
    saved.tool_bindings = frozen_tools::effective(&request.tools, Some(&saved.tool_bindings))?;
    // The document must compile against that tool set before the pass is
    // admitted: a node binding a capability this pass cannot resolve is a stable
    // preflight diagnostic, never a silently skipped branch.
    compile_graph_pass(&workflow, &saved.tool_bindings).map_err(|error| {
        WorkflowPipelineError::InvalidInput(format!(
            "current workflow document is not executable: {error}"
        ))
    })?;
    let budget_ref = digest_id("budget", request.request_id.as_str())?;
    let mut config = saved.agent_checkpoint.config.clone();
    config.loop_id = digest_id("agent.loop", request.request_id.as_str())?;
    config.budget_ref = budget_ref.clone();
    // A capability this pass may now call must be listed for the worker exactly
    // as it would have been listed at first-input freeze.
    for capability_id in tool_capability_refs(&saved.tool_bindings)? {
        if !config
            .allowed_tool_capability_refs
            .contains(&capability_id)
        {
            config.allowed_tool_capability_refs.push(capability_id);
        }
    }
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
    // The pass compiles and runs the document the user has now, never the saved
    // snapshot: the snapshot remains the record of what the Chat started with.
    saved.worker_proposal.payload["config"] = json!({"workflow": workflow});
    saved.broker_proposal.proposal_id = saved.worker_proposal.invocation_id.clone();
    saved.broker_proposal.payload_hash = canonical_hash(&saved.worker_proposal.payload)?;
    extend_manifest_capability_bindings(&mut saved)?;
    enforce_serialized_bound(
        &saved,
        MAXIMUM_PREPARED_RECORD_BYTES,
        "prepared execution record",
    )?;
    Ok(saved)
}

/// The workflow document this pass executes.
///
/// A request that carries no document leaves the saved one in place, which keeps
/// in-process callers that reuse a request verbatim valid. A document that is
/// present must be an executable v1 workflow: the pass fails closed with a
/// stable diagnostic rather than running a graph the documents do not describe.
fn current_workflow_document(
    request: &WorkflowExecutionRequestV1,
    saved: &PreparedExecutionRecordV1,
) -> Result<Value, WorkflowPipelineError> {
    let saved_document = saved
        .worker_proposal
        .payload
        .get("config")
        .and_then(|config| config.get("workflow"))
        .cloned()
        .unwrap_or(Value::Null);
    if request.workflow_snapshot.is_null() {
        return Ok(saved_document);
    }
    if serialized_len(&request.workflow_snapshot)? > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES {
        return Err(WorkflowPipelineError::InvalidInput(
            "current workflow document exceeds the executable persistence bound".to_owned(),
        ));
    }
    Ok(request.workflow_snapshot.clone())
}

/// The capability references one effective tool set authorises.
fn tool_capability_refs(
    bindings: &[StoredFileToolBindingV1],
) -> Result<Vec<StableId>, WorkflowPipelineError> {
    bindings
        .iter()
        .map(|tool| {
            if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                stable(&tool.internal_id)
            } else {
                stable(&tool.capability_id)
            }
        })
        .collect()
}

/// The manifest is the broker's authority boundary: a capability a later pass may
/// call must be bound there, exactly as at first-input freeze.
fn extend_manifest_capability_bindings(
    saved: &mut PreparedExecutionRecordV1,
) -> Result<(), WorkflowPipelineError> {
    let descriptors = file_tool_descriptors()?;
    for tool in saved.tool_bindings.clone() {
        let capability_id = if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
            stable(&tool.internal_id)?
        } else {
            stable(&tool.capability_id)?
        };
        if saved
            .manifest
            .capability_bindings
            .iter()
            .any(|binding| binding.capability_id == capability_id)
        {
            continue;
        }
        let key = if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
            &tool.internal_id
        } else {
            &tool.capability_id
        };
        let binding = match descriptors.get(key) {
            Some(descriptor) => file_tool_capability_binding_with_nodes(
                &tool,
                descriptor,
                vec!["agent".to_owned(), "tool".to_owned()],
            )?,
            // A dynamic MCP capability carries this generation's descriptor
            // rather than a built-in matrix entry.
            None if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) => {
                let descriptor = mcp_tool_descriptor(&tool.internal_id)?;
                file_tool_capability_binding_with_nodes(
                    &tool,
                    &descriptor,
                    vec!["agent".to_owned(), "tool".to_owned()],
                )?
            }
            None => return Err(WorkflowPipelineError::IncompleteEvidence),
        };
        saved.manifest.capability_bindings.push(binding);
    }
    Ok(())
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
