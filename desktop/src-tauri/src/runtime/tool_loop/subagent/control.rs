//! Owner-isolated listing, continuation and cancellation of delegated children.
//!
//! Every control is scoped to the exact Chat, Run and delegating Agent that
//! created the child. A running child is steered through its job; a settled
//! child resumes its own conversation. A control never re-runs a settled child
//! effect.
use super::*;

/// Dispatches one `tool.subagent_*` control.
pub(super) fn execute(
    dispatcher: &FileToolDispatcherV1,
    operation: &str,
    background: bool,
    envelope: &ApprovedInvocationEnvelopeV1,
    cancellation: &CancellationToken,
) -> Result<(Value, String), String> {
    match operation {
        "list" => list(dispatcher),
        "message" => message(dispatcher, background, envelope, cancellation),
        "cancel" => cancel(dispatcher, cancellation),
        _ => Err("unknown subagent control operation".into()),
    }
}

/// The delegating Agent node this control invocation acts for. It is resolved
/// from the same frozen identity scope a spawn uses, so ownership never depends
/// on which graph node the dispatcher happens to run under.
fn caller_node(dispatcher: &FileToolDispatcherV1) -> Result<String, String> {
    dispatcher.parent_selection(true).map(|(node, _)| node)
}

/// Children this Agent created in this Chat and Run, oldest first, with a
/// restart-interrupted child reported as interrupted rather than running.
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
        .map(|mut frame| {
            frame.status = effective_status(&frame, &dispatcher.runtime.jobs, &dispatcher.context.chat_id);
            frame
        })
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
        .map(|mut frame| {
            frame.status = effective_status(&frame, &dispatcher.runtime.jobs, &dispatcher.context.chat_id);
            frame
        })
        .ok_or_else(|| format!("subagent {child_id} is not owned by this Agent in this Run"))
}

/// Lists durable child identities and their live or settled outcome.
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

/// Steers a running child or resumes a settled one.
fn message(
    dispatcher: &FileToolDispatcherV1,
    background: bool,
    envelope: &ApprovedInvocationEnvelopeV1,
    cancellation: &CancellationToken,
) -> Result<(Value, String), String> {
    let child_id = dispatcher.record.call.arguments["childId"]
        .as_str()
        .ok_or_else(|| "subagent childId is invalid".to_owned())?
        .to_owned();
    let message = dispatcher.record.call.arguments["message"]
        .as_str()
        .ok_or_else(|| "subagent message is invalid".to_owned())?
        .to_owned();
    let frame = find_owned(dispatcher, &child_id)?;
    match frame.status {
        ChildStatusV1::Running => {
            // A live child is steered at its next step boundary through its job.
            let job_id = dispatcher
                .runtime
                .jobs
                .running_child_job(&dispatcher.context.chat_id, &child_id)
                .ok_or_else(|| format!("subagent {child_id} is no longer running"))?;
            let value = dispatcher.runtime.jobs.control_scoped(
                &dispatcher.context.chat_id,
                None,
                "job_input",
                &json!({"jobId": job_id, "text": message}),
                cancellation,
            )?;
            Ok((
                value,
                format!("Steered subagent {child_id} at its next step boundary."),
            ))
        }
        ChildStatusV1::Cancelled => Ok((
            frame.outcome(dispatcher.child_jobs(&child_id)?),
            format!("Subagent {child_id} was cancelled and cannot be continued."),
        )),
        _ if background => {
            let value = dispatcher.start_continuation_job(envelope, frame, message)?;
            Ok((
                value,
                format!("Subagent {child_id} continues in the background; observe it with job_output."),
            ))
        }
        _ => dispatcher.run_continuation_now(envelope, frame, message, cancellation),
    }
}

/// Closes one child scope and stops the jobs it still owns. A running child is
/// cancelled through its job; repeating the control is a no-op that returns the
/// closed outcome.
fn cancel(
    dispatcher: &FileToolDispatcherV1,
    cancellation: &CancellationToken,
) -> Result<(Value, String), String> {
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
    stop_child_job(dispatcher, &child_id, cancellation)?;
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
    publish_child_fact(&dispatcher.run_events, &updated, ChildStatusV1::Cancelled)?;
    Ok((
        updated.outcome(dispatcher.child_jobs(&child_id)?),
        format!("Cancelled subagent {child_id} and stopped its remaining jobs."),
    ))
}

/// Cancels the live job of one child, if it has one.
pub(super) fn stop_child_job(
    dispatcher: &FileToolDispatcherV1,
    child_id: &str,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let Some(job_id) = dispatcher
        .runtime
        .jobs
        .running_child_job(&dispatcher.context.chat_id, child_id)
    else {
        return Ok(());
    };
    dispatcher.runtime.jobs.control_scoped(
        &dispatcher.context.chat_id,
        None,
        "job_stop",
        &json!({"jobId": job_id}),
        cancellation,
    )?;
    Ok(())
}
