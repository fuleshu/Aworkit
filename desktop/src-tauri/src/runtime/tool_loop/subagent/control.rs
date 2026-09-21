//! Owner-isolated listing, continuation and cancellation of delegated children.
//!
//! Every control is scoped to the exact Chat, Run and delegating Agent that
//! created the child. A child scope can never be reached by a sibling Agent or
//! another Chat, and a control never re-runs a settled child effect.
use super::*;

/// Dispatches one `tool.subagent_*` control.
pub(super) fn execute(
    dispatcher: &FileToolDispatcherV1,
    operation: &str,
    envelope: &ApprovedInvocationEnvelopeV1,
    cancellation: &CancellationToken,
) -> Result<(Value, String), String> {
    match operation {
        "list" => list(dispatcher),
        "message" => message(dispatcher, envelope, cancellation),
        "cancel" => cancel(dispatcher),
        _ => Err("unknown subagent control operation".into()),
    }
}

/// The delegating Agent node this control invocation acts for. It is resolved
/// from the same frozen identity scope a spawn uses, so ownership never depends
/// on which graph node the dispatcher happens to run under.
fn caller_node(dispatcher: &FileToolDispatcherV1) -> Result<String, String> {
    dispatcher.parent_selection(true).map(|(node, _)| node)
}

/// Children this Agent created in this Chat and Run, oldest first.
fn owned(dispatcher: &FileToolDispatcherV1) -> Result<Vec<SubagentChildFrameV1>, String> {
    let run_id = dispatcher.record.proposal.run_id.clone();
    let node = caller_node(dispatcher)?;
    Ok(dispatcher
        .runtime
        .records
        .subagent_children(&run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|frame| frame.chat_id == dispatcher.context.chat_id && frame.node_id == node)
        .collect())
}

/// Resolves one owned child or explains the ownership failure.
fn find_owned(
    dispatcher: &FileToolDispatcherV1,
    child_id: &str,
) -> Result<SubagentChildFrameV1, String> {
    let run_id = dispatcher.record.proposal.run_id.clone();
    let node = caller_node(dispatcher)?;
    dispatcher
        .runtime
        .records
        .subagent_child(&run_id, child_id)
        .map_err(|error| error.to_string())?
        .filter(|frame| frame.chat_id == dispatcher.context.chat_id && frame.node_id == node)
        .ok_or_else(|| format!("subagent {child_id} is not owned by this Agent in this Run"))
}

/// Lists durable child identities and their latest outcome.
fn list(dispatcher: &FileToolDispatcherV1) -> Result<(Value, String), String> {
    let listings = owned(dispatcher)?
        .iter()
        .map(SubagentChildFrameV1::listing)
        .collect::<Vec<_>>();
    let count = listings.len();
    Ok((
        json!({"children": listings, "count": count}),
        format!("{count} subagent(s) recorded for this Agent."),
    ))
}

/// Resumes one settled child for a bounded continuation turn-set.
fn message(
    dispatcher: &FileToolDispatcherV1,
    envelope: &ApprovedInvocationEnvelopeV1,
    cancellation: &CancellationToken,
) -> Result<(Value, String), String> {
    let child_id = dispatcher.record.call.arguments["childId"]
        .as_str()
        .ok_or_else(|| "subagent childId is invalid".to_owned())?
        .to_owned();
    let message = dispatcher.record.call.arguments["message"]
        .as_str()
        .ok_or_else(|| "subagent message is invalid".to_owned())?;
    let frame = find_owned(dispatcher, &child_id)?;
    if !frame.status.is_continuable() {
        return Ok((
            frame.outcome(dispatcher.child_jobs(&child_id)?),
            format!("Subagent {child_id} was cancelled and cannot be continued."),
        ));
    }
    let child = stable(&child_id).map_err(|error| error.to_string())?;
    let (context, agent) = dispatcher.child_scope_for(
        &child,
        &frame.node_id,
        &frame.inherited_tool_ids,
        frame.read_only,
    )?;
    let turn = dispatcher.run_child_turns(
        envelope,
        context,
        agent,
        frame.input.clone(),
        frame.exchanges.clone(),
        Some(message.to_owned()),
        cancellation,
    )?;
    let error = turn.error.clone();
    let updated = apply_turn(frame, turn);
    dispatcher.persist_child(&updated)?;
    if let Some(error) = error {
        return Err(error);
    }
    Ok((
        dispatcher.child_result(&updated)?,
        dispatcher.child_summary(&updated),
    ))
}

/// Closes one child scope and stops the jobs it still owns. Repeating the
/// control for the same child is a no-op that returns the closed outcome.
fn cancel(dispatcher: &FileToolDispatcherV1) -> Result<(Value, String), String> {
    let child_id = dispatcher.record.call.arguments["childId"]
        .as_str()
        .ok_or_else(|| "subagent childId is invalid".to_owned())?
        .to_owned();
    let frame = find_owned(dispatcher, &child_id)?;
    if frame.status == ChildStatusV1::Cancelled {
        return Ok((
            frame.outcome(dispatcher.child_jobs(&child_id)?),
            format!("Subagent {child_id} was already cancelled."),
        ));
    }
    dispatcher
        .runtime
        .jobs
        .stop_unkept_scoped(&dispatcher.context.chat_id, Some(child_id.as_str()));
    let updated = SubagentChildFrameV1 {
        head_revision: frame.head_revision.saturating_add(1),
        status: ChildStatusV1::Cancelled,
        updated_at: crate::runtime::history::now_label(),
        ..frame
    };
    dispatcher.persist_child(&updated)?;
    Ok((
        updated.outcome(dispatcher.child_jobs(&child_id)?),
        format!("Cancelled subagent {child_id} and stopped its remaining jobs."),
    ))
}
