//! Bounded provider/tool orchestration for one frozen Agent invocation.
//!
//! Providers can only request tools. Every request is settled by the injected
//! trusted-core port before the exact result is sent back on the next model
//! turn. This module owns neither workspace authority nor tool execution.

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelAssistantContentV1, ModelCandidateV1,
    ModelResolutionPlanV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolDispatchEvidenceV1,
    ModelToolEventV1, ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1, ProviderError,
};
use aworkit_protocol::StableId;
use aworkit_trusted_core::ApprovalResponseV1;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::tool_loop::{ToolApprovalChallengeV1, WorkflowToolActivityV1};

// These ceilings must accept every budget that the frozen workflow contract
// can produce. Agent nodes allow up to 12 turns and the run freezer caps the
// aggregate tool-call budget at 64.
const MAXIMUM_MODEL_TURNS: u32 = 12;
const MAXIMUM_TOOL_CALLS: u32 = 64;
const MAXIMUM_TOOL_CALLS_PER_TURN: usize = 8;
const MAXIMUM_DURABLE_EXCHANGE_BYTES: usize = 512 * 1024;

/// Trusted-core boundary used by the provider loop. Implementations must
/// durably settle a call before returning its provider-facing result.
pub(crate) trait ModelToolInvocationPortV1 {
    fn invoke(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String>;

    fn commit_exchange(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), String>;

    /// Approval-aware invocation: a PerInvocation binding suspends with a
    /// durable challenge instead of failing the call.
    fn invoke_extended(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<ToolInvokeV1, String> {
        self.invoke(outer_invocation_id, turn, call, cancellation)
            .map(ToolInvokeV1::Settled)
    }

    /// Resolves a suspended challenge with the committed user decision and
    /// settles the exact original call once.
    fn resolve(
        &self,
        _outer_invocation_id: &StableId,
        _turn: u32,
        _call: &ModelToolCallV1,
        _response: &ApprovalResponseV1,
        _cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        Err("approval resolution is unavailable for this tool authority".into())
    }
}

/// One approval-aware tool invocation step.
#[derive(Clone, Debug)]
pub(crate) enum ToolInvokeV1 {
    Settled(SettledModelToolCallV1),
    Approval(ToolApprovalChallengeV1),
}

/// Durable agent-loop prefix captured when a tool call suspends for approval.
/// Resuming restores this state and continues from the same turn without
/// recomputing model or tool work.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelToolLoopPendingV1 {
    pub turn: u32,
    pub call: ModelToolCallV1,
    pub challenge: ToolApprovalChallengeV1,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub activities: Vec<WorkflowToolActivityV1>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub total_calls: u32,
}

/// Outcome of one approval-aware agent loop invocation.
pub(crate) enum ModelToolLoopRunV1 {
    Completed(ModelToolLoopOutcomeV1),
    Suspended {
        challenge: ToolApprovalChallengeV1,
        pending: ModelToolLoopPendingV1,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettledModelToolCallV1 {
    pub result: ModelToolResultV1,
    pub activity: WorkflowToolActivityV1,
}

pub(crate) struct ModelToolLoopRequestV1<'a> {
    pub outer_invocation_id: &'a StableId,
    pub input: Value,
    pub parameters: BTreeMap<String, Value>,
    pub definitions: Vec<ModelToolDefinitionV1>,
    pub binding_id: String,
    pub binding_version_hash: String,
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
    pub maximum_turns: u32,
    pub maximum_tool_calls: u32,
    pub maximum_tokens: u64,
    pub deadline_epoch_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelToolLoopOutcomeV1 {
    pub assistant_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub activities: Vec<WorkflowToolActivityV1>,
}

#[derive(Debug, Error)]
pub(crate) enum ModelToolLoopErrorV1 {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("tool authority rejected the provider request: {0}")]
    ToolAuthority(String),
    #[error("Agent model/tool budget is exhausted: {0}")]
    Budget(&'static str),
    #[error("provider accepted the Agent turn but returned no final assistant text")]
    MissingAssistantOutput,
}

#[derive(Debug, Error)]
#[error("{error}")]
pub(crate) struct ModelToolLoopFailureV1 {
    pub error: ModelToolLoopErrorV1,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub activities: Vec<WorkflowToolActivityV1>,
}

/// Runs the exact frozen model/tool loop until a final assistant message or a
/// fail-closed budget/authority error is reached.
pub(crate) fn execute_model_tool_loop_v1(
    gateway: &FrozenModelGateway,
    request: ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    cancellation: &CancellationToken,
) -> Result<ModelToolLoopOutcomeV1, ModelToolLoopFailureV1> {
    validate_limits(&request).map_err(|error| failure(error, 0, 0, 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id,
            version_hash: request.binding_version_hash,
        }],
        maximum_input_bytes: request.maximum_input_bytes,
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = Vec::new();
    let mut activities = Vec::new();
    let mut total_calls = 0_u32;
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;

    for turn in 1..=request.maximum_turns {
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        attempted_model_turns = attempted_model_turns.saturating_add(1);
        let evidence = gateway
            .execute_tool_turn_cancellable(
                &plan,
                &ModelToolRequestV1 {
                    input: request.input.clone(),
                    parameters: request.parameters.clone(),
                    tools: request.definitions.clone(),
                    exchanges: exchanges.clone(),
                },
                cancellation,
            )
            .map_err(|error| {
                failure(
                    error.into(),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = normalize_turn(evidence);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        if input_tokens.saturating_add(output_tokens) > request.maximum_tokens {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("token limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }

        if turn_output.calls.is_empty() {
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                ));
            }
            return Ok(ModelToolLoopOutcomeV1 {
                assistant_text,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                exchanges,
                activities,
            });
        }
        if turn == request.maximum_turns {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("model turn limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        if turn_output.calls.len() > MAXIMUM_TOOL_CALLS_PER_TURN {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("per-turn tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        total_calls =
            total_calls.saturating_add(u32::try_from(turn_output.calls.len()).unwrap_or(u32::MAX));
        if total_calls > request.maximum_tool_calls {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }

        let mut results = Vec::with_capacity(turn_output.calls.len());
        for call in &turn_output.calls {
            enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
                failure(
                    error,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
            let settled = authority
                .invoke(request.outer_invocation_id, turn, call, cancellation)
                .map_err(|error| {
                    failure(
                        ModelToolLoopErrorV1::ToolAuthority(error),
                        input_tokens,
                        output_tokens,
                        attempted_model_turns,
                        settled_tool_calls,
                        &exchanges,
                        &activities,
                    )
                })?;
            results.push(settled.result);
            activities.push(settled.activity);
            settled_tool_calls = settled_tool_calls.saturating_add(1);
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        let mut durable_exchanges = exchanges.clone();
        durable_exchanges.push(exchange.clone());
        if serde_json::to_vec(&durable_exchanges)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("durable model/tool history byte limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        exchanges.push(exchange);
    }
    Err(failure(
        ModelToolLoopErrorV1::Budget("model turn limit"),
        input_tokens,
        output_tokens,
        attempted_model_turns,
        settled_tool_calls,
        &exchanges,
        &activities,
    ))
}

fn failure(
    error: ModelToolLoopErrorV1,
    input_tokens: u64,
    output_tokens: u64,
    attempted_model_turns: u32,
    settled_tool_calls: u32,
    exchanges: &[ModelToolExchangeV1],
    activities: &[WorkflowToolActivityV1],
) -> ModelToolLoopFailureV1 {
    ModelToolLoopFailureV1 {
        error,
        input_tokens,
        output_tokens,
        attempted_model_turns,
        settled_tool_calls,
        exchanges: exchanges.to_vec(),
        activities: activities.to_vec(),
    }
}

fn validate_limits(request: &ModelToolLoopRequestV1<'_>) -> Result<(), ModelToolLoopErrorV1> {
    if request.definitions.is_empty()
        || !(2..=MAXIMUM_MODEL_TURNS).contains(&request.maximum_turns)
        || request.maximum_tool_calls == 0
        || request.maximum_tool_calls > MAXIMUM_TOOL_CALLS
        || request.maximum_tokens == 0
    {
        return Err(ModelToolLoopErrorV1::Budget("invalid frozen limits"));
    }
    Ok(())
}

fn enforce_deadline(deadline_epoch_millis: u64) -> Result<(), ModelToolLoopErrorV1> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    if now >= deadline_epoch_millis {
        Err(ModelToolLoopErrorV1::Budget("Run deadline"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn expired_deadline_is_a_budget_failure_before_provider_or_tool_work() {
        assert!(matches!(
            enforce_deadline(0),
            Err(ModelToolLoopErrorV1::Budget("Run deadline"))
        ));
        enforce_deadline(u64::MAX).expect("maximum deadline remains open");
    }
}

struct NormalizedTurnV1 {
    assistant_text: String,
    assistant_content: Vec<ModelAssistantContentV1>,
    calls: Vec<ModelToolCallV1>,
    input_tokens: u64,
    output_tokens: u64,
}

fn normalize_turn(evidence: ModelToolDispatchEvidenceV1) -> NormalizedTurnV1 {
    let mut assistant_text = String::new();
    let mut assistant_content = Vec::new();
    let mut calls = Vec::new();
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    for event in evidence.events {
        match event {
            ModelToolEventV1::AssistantOutput { text } => {
                assistant_text.push_str(&text);
                assistant_content.push(ModelAssistantContentV1::Text { text });
            }
            ModelToolEventV1::ToolCall { call } => {
                calls.push(call.clone());
                assistant_content.push(ModelAssistantContentV1::ToolCall { call });
            }
            ModelToolEventV1::Usage {
                input_tokens: input,
                output_tokens: output,
            } => {
                input_tokens = input;
                output_tokens = output;
            }
            ModelToolEventV1::ReasoningRaw { .. }
            | ModelToolEventV1::ReasoningSummary { .. }
            | ModelToolEventV1::Progress { .. } => {}
        }
    }
    NormalizedTurnV1 {
        assistant_text,
        assistant_content,
        calls,
        input_tokens,
        output_tokens,
    }
}

/// Runs the frozen model/tool loop with approval awareness. A PerInvocation
/// tool binding suspends the loop with a durable prefix instead of failing.
pub(crate) fn execute_model_tool_loop_approval_v1(
    gateway: &FrozenModelGateway,
    request: ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    cancellation: &CancellationToken,
) -> Result<ModelToolLoopRunV1, ModelToolLoopFailureV1> {
    validate_limits(&request).map_err(|error| failure(error, 0, 0, 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request.maximum_input_bytes,
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = Vec::new();
    let mut activities = Vec::new();
    let mut total_calls = 0_u32;
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;

    for turn in 1..=request.maximum_turns {
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        attempted_model_turns = attempted_model_turns.saturating_add(1);
        let evidence = gateway
            .execute_tool_turn_cancellable(
                &plan,
                &ModelToolRequestV1 {
                    input: request.input.clone(),
                    parameters: request.parameters.clone(),
                    tools: request.definitions.clone(),
                    exchanges: exchanges.clone(),
                },
                cancellation,
            )
            .map_err(|error| {
                failure(
                    error.into(),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = normalize_turn(evidence);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        if input_tokens.saturating_add(output_tokens) > request.maximum_tokens {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("token limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        if turn_output.calls.is_empty() {
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                ));
            }
            return Ok(ModelToolLoopRunV1::Completed(ModelToolLoopOutcomeV1 {
                assistant_text,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                exchanges,
                activities,
            }));
        }
        if turn == request.maximum_turns {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("model turn limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        if turn_output.calls.len() > MAXIMUM_TOOL_CALLS_PER_TURN {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("per-turn tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        total_calls =
            total_calls.saturating_add(u32::try_from(turn_output.calls.len()).unwrap_or(u32::MAX));
        if total_calls > request.maximum_tool_calls {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }

        let mut results = Vec::with_capacity(turn_output.calls.len());
        for call in &turn_output.calls {
            enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
                failure(
                    error,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
            let settled = authority
                .invoke_extended(request.outer_invocation_id, turn, call, cancellation)
                .map_err(|error| {
                    failure(
                        ModelToolLoopErrorV1::ToolAuthority(error),
                        input_tokens,
                        output_tokens,
                        attempted_model_turns,
                        settled_tool_calls,
                        &exchanges,
                        &activities,
                    )
                })?;
            match settled {
                ToolInvokeV1::Settled(settled) => {
                    results.push(settled.result);
                    activities.push(settled.activity);
                    settled_tool_calls = settled_tool_calls.saturating_add(1);
                }
                ToolInvokeV1::Approval(challenge) => {
                    return Ok(ModelToolLoopRunV1::Suspended {
                        challenge: challenge.clone(),
                        pending: ModelToolLoopPendingV1 {
                            turn,
                            call: call.clone(),
                            challenge,
                            exchanges,
                            activities,
                            input_tokens,
                            output_tokens,
                            attempted_model_turns,
                            settled_tool_calls,
                            total_calls,
                        },
                    });
                }
            }
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        let mut durable_exchanges = exchanges.clone();
        durable_exchanges.push(exchange.clone());
        if serde_json::to_vec(&durable_exchanges)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("durable model/tool history byte limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        exchanges.push(exchange);
    }
    Err(failure(
        ModelToolLoopErrorV1::Budget("model turn limit"),
        input_tokens,
        output_tokens,
        attempted_model_turns,
        settled_tool_calls,
        &exchanges,
        &activities,
    ))
}

/// Resumes a suspended agent loop: the exact original call is settled with
/// the committed decision, its exchange is durably recorded, and the loop
/// continues from the following turn.
pub(crate) fn resume_model_tool_loop_v1(
    gateway: &FrozenModelGateway,
    request: ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    pending: &ModelToolLoopPendingV1,
    approved: bool,
    now_epoch_millis: u64,
    cancellation: &CancellationToken,
) -> Result<ModelToolLoopRunV1, ModelToolLoopFailureV1> {
    validate_limits(&request).map_err(|error| failure(error, 0, 0, 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request.maximum_input_bytes,
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = pending.exchanges.clone();
    let mut activities = pending.activities.clone();
    let mut total_calls = pending.total_calls;
    let mut input_tokens = pending.input_tokens;
    let mut output_tokens = pending.output_tokens;
    let mut attempted_model_turns = pending.attempted_model_turns;
    let mut settled_tool_calls = pending.settled_tool_calls;

    let response = ApprovalResponseV1 {
        invocation_id: StableId::parse(pending.challenge.invocation_id.clone()).map_err(|_| {
            failure(
                ModelToolLoopErrorV1::ToolAuthority("invalid approval invocation identity".into()),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?,
        nonce: StableId::parse(pending.challenge.nonce.clone()).map_err(|_| {
            failure(
                ModelToolLoopErrorV1::ToolAuthority("invalid approval nonce".into()),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?,
        approved,
        now_epoch_millis,
    };
    enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
        failure(
            error,
            input_tokens,
            output_tokens,
            attempted_model_turns,
            settled_tool_calls,
            &exchanges,
            &activities,
        )
    })?;
    let settled = authority
        .resolve(
            request.outer_invocation_id,
            pending.turn,
            &pending.call,
            &response,
            cancellation,
        )
        .map_err(|error| {
            failure(
                ModelToolLoopErrorV1::ToolAuthority(error),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
    let exchange = ModelToolExchangeV1 {
        assistant_content: vec![ModelAssistantContentV1::ToolCall {
            call: pending.call.clone(),
        }],
        results: vec![settled.result.clone()],
    };
    let mut durable_exchanges = exchanges.clone();
    durable_exchanges.push(exchange.clone());
    if serde_json::to_vec(&durable_exchanges)
        .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
    {
        return Err(failure(
            ModelToolLoopErrorV1::Budget("durable model/tool history byte limit"),
            input_tokens,
            output_tokens,
            attempted_model_turns,
            settled_tool_calls,
            &exchanges,
            &activities,
        ));
    }
    authority
        .commit_exchange(request.outer_invocation_id, pending.turn, &exchange)
        .map_err(|error| {
            failure(
                ModelToolLoopErrorV1::ToolAuthority(error),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
    exchanges.push(exchange);
    activities.push(settled.activity);
    settled_tool_calls = settled_tool_calls.saturating_add(1);

    for turn in pending.turn.saturating_add(1)..=request.maximum_turns {
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        attempted_model_turns = attempted_model_turns.saturating_add(1);
        let evidence = gateway
            .execute_tool_turn_cancellable(
                &plan,
                &ModelToolRequestV1 {
                    input: request.input.clone(),
                    parameters: request.parameters.clone(),
                    tools: request.definitions.clone(),
                    exchanges: exchanges.clone(),
                },
                cancellation,
            )
            .map_err(|error| {
                failure(
                    error.into(),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
            failure(
                error,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = normalize_turn(evidence);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        if input_tokens.saturating_add(output_tokens) > request.maximum_tokens {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("token limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        if turn_output.calls.is_empty() {
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                ));
            }
            return Ok(ModelToolLoopRunV1::Completed(ModelToolLoopOutcomeV1 {
                assistant_text,
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                exchanges,
                activities,
            }));
        }
        if turn == request.maximum_turns {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("model turn limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        if turn_output.calls.len() > MAXIMUM_TOOL_CALLS_PER_TURN {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("per-turn tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        total_calls =
            total_calls.saturating_add(u32::try_from(turn_output.calls.len()).unwrap_or(u32::MAX));
        if total_calls > request.maximum_tool_calls {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("tool-call limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        let mut results = Vec::with_capacity(turn_output.calls.len());
        for call in &turn_output.calls {
            enforce_deadline(request.deadline_epoch_millis).map_err(|error| {
                failure(
                    error,
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
            let settled = authority
                .invoke_extended(request.outer_invocation_id, turn, call, cancellation)
                .map_err(|error| {
                    failure(
                        ModelToolLoopErrorV1::ToolAuthority(error),
                        input_tokens,
                        output_tokens,
                        attempted_model_turns,
                        settled_tool_calls,
                        &exchanges,
                        &activities,
                    )
                })?;
            match settled {
                ToolInvokeV1::Settled(settled) => {
                    results.push(settled.result);
                    activities.push(settled.activity);
                    settled_tool_calls = settled_tool_calls.saturating_add(1);
                }
                ToolInvokeV1::Approval(challenge) => {
                    return Ok(ModelToolLoopRunV1::Suspended {
                        challenge: challenge.clone(),
                        pending: ModelToolLoopPendingV1 {
                            turn,
                            call: call.clone(),
                            challenge,
                            exchanges,
                            activities,
                            input_tokens,
                            output_tokens,
                            attempted_model_turns,
                            settled_tool_calls,
                            total_calls,
                        },
                    });
                }
            }
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        let mut durable_exchanges = exchanges.clone();
        durable_exchanges.push(exchange.clone());
        if serde_json::to_vec(&durable_exchanges)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("durable model/tool history byte limit"),
                input_tokens,
                output_tokens,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            ));
        }
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        exchanges.push(exchange);
    }
    Err(failure(
        ModelToolLoopErrorV1::Budget("model turn limit"),
        input_tokens,
        output_tokens,
        attempted_model_turns,
        settled_tool_calls,
        &exchanges,
        &activities,
    ))
}
