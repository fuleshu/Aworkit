//! Finish an interrupted assistant turn before asking the model to continue.
use super::*;

/// Restores the original assistant content and results, settles the selected
/// decision once, then handles the rest of that same provider response. Returns
/// false if another call needs approval; no provider turn runs in that case.
pub(super) fn resume_pending_turn(
    request: &ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    pending: &mut ModelToolLoopPendingV1,
    approved: bool,
    now_epoch_millis: u64,
    cancellation: &CancellationToken,
) -> Result<bool, ModelToolLoopFailureV1> {
    let mut exchange = pending
        .pending_exchange
        .clone()
        .unwrap_or_else(|| ModelToolExchangeV1 {
            assistant_content: vec![ModelAssistantContentV1::ToolCall {
                call: pending.call.clone(),
            }],
            results: Vec::new(),
        });
    let calls: Vec<_> = exchange
        .assistant_content
        .iter()
        .filter_map(|content| match content {
            ModelAssistantContentV1::ToolCall { call } => Some(call.clone()),
            _ => None,
        })
        .collect();
    let index = exchange.results.len();
    if calls.len() > MAXIMUM_TOOL_CALLS_PER_TURN
        || calls.get(index) != Some(&pending.call)
        || exchange
            .results
            .iter()
            .zip(&calls)
            .any(|(result, call)| result.call_id != call.call_id)
    {
        return Err(authority_failure(
            pending,
            "invalid pending assistant turn".into(),
        ));
    }
    ensure_bound(pending, &exchange)?;
    let response = ApprovalResponseV1 {
        invocation_id: StableId::parse(pending.challenge.invocation_id.clone()).map_err(|_| {
            authority_failure(pending, "invalid approval invocation identity".into())
        })?,
        nonce: StableId::parse(pending.challenge.nonce.clone())
            .map_err(|_| authority_failure(pending, "invalid approval nonce".into()))?,
        approved,
        now_epoch_millis,
    };
    let settled = authority
        .resolve(
            request.outer_invocation_id,
            pending.turn,
            &pending.call,
            &response,
            cancellation,
        )
        .map_err(|error| authority_failure(pending, error))?;
    record_settlement(request, pending, &mut exchange, &calls[index], settled);

    for call in &calls[index + 1..] {
        if !approved {
            // Do not run further actions from an already issued batch after a
            // user rejection. Retain a correlated non-execution result for each
            // request so the model can reconsider using the user's reason.
            exchange.results.push(ModelToolResultV1 {
                images: Vec::new(),
                call_id: call.call_id.clone(),
                content: serde_json::json!({"error":"not_executed_after_denial",
                    "detail":"Not executed: an earlier action in this response was denied. Reconsider the remaining requests in light of the user's reason. The available tool definitions are unchanged."}),
                is_error: true,
            });
            continue;
        }
        ensure_bound(pending, &exchange)?;
        let invocation = authority
            .invoke_extended(
                request.outer_invocation_id,
                pending.turn,
                call,
                cancellation,
            )
            .map_err(|error| authority_failure(pending, error))?;
        match invocation {
            ToolInvokeV1::Settled(settled) => {
                record_settlement(request, pending, &mut exchange, call, settled)
            }
            ToolInvokeV1::Approval(challenge) => {
                ensure_bound(pending, &exchange)?;
                pending.call = call.clone();
                pending.challenge = challenge;
                pending.pending_exchange = Some(exchange);
                return Ok(false);
            }
        }
    }
    ensure_bound(pending, &exchange)?;
    authority
        .commit_exchange(request.outer_invocation_id, pending.turn, &exchange)
        .map_err(|error| authority_failure(pending, error))?;
    pending.exchanges.push(exchange);
    pending.pending_exchange = None;
    Ok(true)
}

fn record_settlement(
    request: &ModelToolLoopRequestV1<'_>,
    pending: &mut ModelToolLoopPendingV1,
    exchange: &mut ModelToolExchangeV1,
    call: &ModelToolCallV1,
    settled: SettledModelToolCallV1,
) {
    exchange.results.push(model_facing_tool_result(
        &settled.result,
        &call.capability_id,
        request.maximum_tool_output_bytes,
    ));
    pending.activities.push(settled.activity);
    pending.settled_tool_calls = pending.settled_tool_calls.saturating_add(1);
    append_runtime_notices(
        &mut pending.pending_runtime_notice,
        pending
            .repeat_tool_reminder
            .observe_calls(std::slice::from_ref(call)),
    );
}

fn authority_failure(pending: &ModelToolLoopPendingV1, message: String) -> ModelToolLoopFailureV1 {
    pending_failure(pending, ModelToolLoopErrorV1::ToolAuthority(message))
}

fn pending_failure(
    pending: &ModelToolLoopPendingV1,
    error: ModelToolLoopErrorV1,
) -> ModelToolLoopFailureV1 {
    failure(
        error,
        pending.input_tokens,
        pending.output_tokens,
        pending.attempted_model_turns,
        pending.settled_tool_calls,
        &pending.exchanges,
        &pending.activities,
    )
}

fn ensure_bound(
    pending: &ModelToolLoopPendingV1,
    exchange: &ModelToolExchangeV1,
) -> Result<(), ModelToolLoopFailureV1> {
    if serde_json::to_vec(exchange)
        .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
    {
        return Err(pending_failure(
            pending,
            ModelToolLoopErrorV1::Budget("individual model/tool exchange byte limit"),
        ));
    }
    Ok(())
}

/// Apply the same history bound before persisting an incomplete turn.
pub(super) fn suspend(
    pending: ModelToolLoopPendingV1,
) -> Result<ModelToolLoopRunV1, ModelToolLoopFailureV1> {
    if let Some(exchange) = &pending.pending_exchange {
        ensure_bound(&pending, exchange)?;
    }
    Ok(ModelToolLoopRunV1::Suspended {
        challenge: pending.challenge.clone(),
        pending,
    })
}

#[cfg(test)]
mod tests;
