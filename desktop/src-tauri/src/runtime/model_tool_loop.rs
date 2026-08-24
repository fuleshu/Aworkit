//! Bounded provider/tool orchestration for one frozen Agent invocation.
//!
//! Providers can only request tools. Every request is settled by the injected
//! trusted-core port before the exact result is sent back on the next model
//! turn. This module owns neither workspace authority nor tool execution.

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelAssistantContentV1, ModelCandidateV1,
    ModelResolutionPlanV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolDispatchEvidenceV1,
    ModelToolEventV1, ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1, ProviderError,
};
use aworkit_protocol::StableId;
use serde_json::Value;
use thiserror::Error;

use super::tool_loop::SimpleChatToolActivityV1;

const MAXIMUM_MODEL_TURNS: u32 = 8;
const MAXIMUM_TOOL_CALLS: u32 = 32;
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettledModelToolCallV1 {
    pub result: ModelToolResultV1,
    pub activity: SimpleChatToolActivityV1,
}

pub(crate) struct ModelToolLoopRequestV1<'a> {
    pub outer_invocation_id: &'a StableId,
    pub input: Value,
    pub definitions: Vec<ModelToolDefinitionV1>,
    pub binding_id: String,
    pub binding_version_hash: String,
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
    pub maximum_turns: u32,
    pub maximum_tool_calls: u32,
    pub maximum_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelToolLoopOutcomeV1 {
    pub assistant_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub activities: Vec<SimpleChatToolActivityV1>,
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
    pub activities: Vec<SimpleChatToolActivityV1>,
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
        attempted_model_turns = attempted_model_turns.saturating_add(1);
        let evidence = gateway
            .execute_tool_turn_cancellable(
                &plan,
                &ModelToolRequestV1 {
                    input: request.input.clone(),
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
    activities: &[SimpleChatToolActivityV1],
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
