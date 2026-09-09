//! Provider/tool orchestration for one frozen Agent invocation.
//!
//! Providers can only request tools. Every request is settled by the injected
//! trusted-core port before the exact result is sent back on the next model
//! turn. This module owns neither workspace authority nor tool execution.

use std::collections::BTreeMap;

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelAssistantContentV1, ModelCandidateV1,
    ModelResolutionPlanV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolDispatchEvidenceV1,
    ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1, ProviderError,
    project_model_tool_events,
};
use aworkit_protocol::StableId;
use aworkit_trusted_core::ApprovalResponseV1;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    repeat_tool_reminder::RepeatToolReminderStateV1,
    tool_loop::{ToolApprovalChallengeV1, WorkflowToolActivityV1},
};

mod approval_turn;

const MAXIMUM_TOOL_CALLS_PER_TURN: usize = 8;
const MAXIMUM_DURABLE_EXCHANGE_BYTES: usize = 512 * 1024;
pub(crate) const PROVIDER_TIMEOUT_RECOVERIES_V1: u32 = 1;
pub(crate) const PROVIDER_TIMEOUT_NOTICE: &str = "Aworkit recovery notice: the previous provider request timed out before a complete response was received. Any partial response from that attempt was discarded. Continue the task using the conversation and completed tool results available here.";

/// Trusted-core boundary used by the provider loop. Implementations must
/// durably settle a call before returning its provider-facing result.
pub(crate) trait ModelToolInvocationPortV1 {
    fn legacy_context_identity(&self) -> bool {
        true
    }
    fn manage_model_context(
        &self,
        _gateway: &FrozenModelGateway,
        _plan: &ModelResolutionPlanV1,
        outer: &StableId,
        through: usize,
        agent: Option<&AgentContextV1>,
        request: &mut ModelToolRequestV1,
        cancellation: &CancellationToken,
        _trigger: super::compaction::Trigger,
    ) -> Result<super::compaction::Preparation, String> {
        self.revise_model_context(request)?;
        if let Some(agent) = agent {
            self.prepare_automatic_context(outer, through, agent, request, cancellation)?;
        }
        Ok(Default::default())
    }
    fn record_context_usage(
        &self,
        _outer: &StableId,
        _agent: Option<&AgentContextV1>,
        _request: &ModelToolRequestV1,
        _input: u64,
        _output: u64,
        _assistant_tokens: u64,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Preserve typed instruction references in ordinary model request history.
    fn record_text_context(&self, _input: &Value, _context: &ModelToolRequestV1) {}
    /// Last Agent preparation step, after any visible-context replacement.
    fn prepare_automatic_context(
        &self,
        _outer: &StableId,
        _after_exchanges: usize,
        _agent: &AgentContextV1,
        _request: &mut ModelToolRequestV1,
        _cancellation: &CancellationToken,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Apply explicitly saved prompt revisions without changing tool authority.
    fn revise_model_context(&self, _request: &mut ModelToolRequestV1) -> Result<(), String> {
        Ok(())
    }
    /// Non-secret project facts from the trusted Run context, separate from
    /// user conversation messages and external services' project identifiers.
    fn project_context(&self) -> Option<Value> {
        None
    }

    /// Refresh and durably record injected context before a provider request.
    fn prepare_context(
        &self,
        _outer: &StableId,
        _after_exchanges: usize,
        _definitions: &[ModelToolDefinitionV1],
        _cancellation: &CancellationToken,
    ) -> Result<Vec<aworkit_capability_host::ModelToolContextV1>, String> {
        Ok(Vec::new())
    }

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
    /// Full assistant turn and already settled results at the approval boundary.
    /// Older checkpoints omitted this and can only restore their single call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_exchange: Option<ModelToolExchangeV1>,
    pub activities: Vec<WorkflowToolActivityV1>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    /// Compatibility sink for approval checkpoints written while aggregate
    /// tool calls were a termination budget. New checkpoints omit it.
    #[serde(default, rename = "totalCalls", skip_serializing)]
    pub _legacy_total_calls: Option<u32>,
    #[serde(default)]
    pub timeout_recoveries: u32,
    #[serde(default)]
    pub repeat_tool_reminder: RepeatToolReminderStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_runtime_notice: Option<String>,
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

#[derive(Clone, Debug)]
pub(crate) struct AgentContextV1 {
    pub node_id: String,
    pub tool_ids: Vec<String>,
    pub child: Option<String>,
}

pub(crate) struct ModelToolLoopRequestV1<'a> {
    pub agent_context: Option<AgentContextV1>,
    pub outer_invocation_id: &'a StableId,
    pub input: Value,
    pub parameters: BTreeMap<String, Value>,
    pub definitions: Vec<ModelToolDefinitionV1>,
    pub binding_id: String,
    pub binding_version_hash: String,
    /// Base input, definitions and injected context. Completed exchanges have
    /// their own durable byte bound and must also fit the provider request.
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
    pub maximum_tool_output_bytes: usize,
    pub maximum_timeout_recoveries: u32,
    pub maximum_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelToolLoopOutcomeV1 {
    pub assistant_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub timeout_recoveries: u32,
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

/// Runs the exact frozen model/tool loop until the model returns a final
/// assistant message or a real authority, context, or token error is reached.
/// There is deliberately no elapsed-time, model-turn, or aggregate tool-call
/// cap. Provider requests and individual tools retain their own timeouts.
pub(crate) fn execute_model_tool_loop_v1(
    gateway: &FrozenModelGateway,
    request: ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    cancellation: &CancellationToken,
) -> Result<ModelToolLoopOutcomeV1, ModelToolLoopFailureV1> {
    validate_limits(&request).map_err(|error| failure(error, 0, 0, 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(MAXIMUM_DURABLE_EXCHANGE_BYTES),
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = Vec::new();
    let mut activities = Vec::new();
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;
    let mut timeout_recoveries = 0_u32;
    let mut repeat_tool_reminder = RepeatToolReminderStateV1::default();
    let mut pending_runtime_notice = None;
    let mut turn = 1_u32;

    loop {
        let evidence = execute_tool_turn_with_timeout_recovery(
            gateway,
            &plan,
            &request,
            authority,
            turn.saturating_sub(1) as usize,
            &mut exchanges,
            pending_runtime_notice.take(),
            cancellation,
            &mut attempted_model_turns,
            &mut timeout_recoveries,
            &mut input_tokens,
            &mut output_tokens,
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
        let turn_output = project_model_tool_events(&evidence.events);
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
                timeout_recoveries,
                exchanges,
                activities,
            });
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
            results.push(model_facing_tool_result(
                &settled.result,
                &call.capability_id,
                request.maximum_tool_output_bytes,
            ));
            activities.push(settled.activity);
            settled_tool_calls = settled_tool_calls.saturating_add(1);
            append_runtime_notices(
                &mut pending_runtime_notice,
                repeat_tool_reminder.observe_calls(std::slice::from_ref(call)),
            );
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        if serde_json::to_vec(&exchange)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("individual model/tool exchange byte limit"),
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
        turn = turn.saturating_add(1);
    }
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
    let automatic_child = request.agent_context.as_ref().is_some_and(|agent| {
        agent.child.is_some()
            && agent
                .tool_ids
                .iter()
                .any(|id| id == "tool.workspace_instructions")
    });
    if (request.definitions.is_empty() && !automatic_child)
        || request.maximum_input_bytes == 0
        || request.maximum_tool_output_bytes == 0
        || request.maximum_timeout_recoveries > PROVIDER_TIMEOUT_RECOVERIES_V1
        || request.maximum_tokens == 0
    {
        return Err(ModelToolLoopErrorV1::Budget("invalid frozen limits"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_tool_turn_with_timeout_recovery(
    gateway: &FrozenModelGateway,
    plan: &ModelResolutionPlanV1,
    request: &ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    through: usize,
    exchanges: &mut Vec<ModelToolExchangeV1>,
    runtime_notice: Option<String>,
    cancellation: &CancellationToken,
    attempted_model_turns: &mut u32,
    timeout_recoveries: &mut u32,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
) -> Result<ModelToolDispatchEvidenceV1, ModelToolLoopErrorV1> {
    let context_messages = authority
        .prepare_context(
            request.outer_invocation_id,
            through,
            &request.definitions,
            cancellation,
        )
        .map_err(ModelToolLoopErrorV1::ToolAuthority)?;
    let mut retry_notice = runtime_notice;
    let mut provider_request = ModelToolRequestV1 {
        context_messages,
        input: request.input.clone(),
        parameters: request.parameters.clone(),
        tools: request.definitions.clone(),
        exchanges: exchanges.clone(),
        retry_notice: retry_notice.clone(),
    };
    let preparation = authority
        .manage_model_context(
            gateway,
            plan,
            request.outer_invocation_id,
            through,
            request.agent_context.as_ref(),
            &mut provider_request,
            cancellation,
            super::compaction::Trigger::Pressure,
        )
        .map_err(ModelToolLoopErrorV1::ToolAuthority)?;
    *input_tokens = input_tokens.saturating_add(preparation.input_tokens);
    *output_tokens = output_tokens.saturating_add(preparation.output_tokens);
    if let Some(error) = preparation.error {
        return Err(ModelToolLoopErrorV1::ToolAuthority(error));
    }
    if preparation.durable {
        *exchanges = provider_request.exchanges.clone();
    }
    let mut overflow_retries = 0;
    loop {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        if input_tokens.saturating_add(*output_tokens) >= request.maximum_tokens {
            return Err(ModelToolLoopErrorV1::Budget("token limit"));
        }
        *attempted_model_turns = attempted_model_turns.saturating_add(1);
        match gateway.execute_tool_turn_cancellable(plan, &provider_request, cancellation) {
            Err(ProviderError::ContextWindowExceeded)
                if overflow_retries < preparation.max_overflow_retries =>
            {
                let recovery = authority
                    .manage_model_context(
                        gateway,
                        plan,
                        request.outer_invocation_id,
                        through,
                        request.agent_context.as_ref(),
                        &mut provider_request,
                        cancellation,
                        super::compaction::Trigger::ContextOverflow,
                    )
                    .map_err(ModelToolLoopErrorV1::ToolAuthority)?;
                *input_tokens = input_tokens.saturating_add(recovery.input_tokens);
                *output_tokens = output_tokens.saturating_add(recovery.output_tokens);
                if let Some(error) = recovery.error {
                    return Err(ModelToolLoopErrorV1::ToolAuthority(error));
                }
                if cancellation.is_cancelled() {
                    return Err(ProviderError::Cancelled.into());
                }
                if !recovery.changed {
                    return Err(ProviderError::ContextWindowExceeded.into());
                }
                if recovery.durable {
                    *exchanges = provider_request.exchanges.clone();
                }
                overflow_retries += 1;
            }
            Err(ProviderError::RequestTimedOut)
                if *timeout_recoveries < request.maximum_timeout_recoveries =>
            {
                *timeout_recoveries = timeout_recoveries.saturating_add(1);
                retry_notice = Some(match retry_notice {
                    Some(notice) => format!("{notice}\n\n{PROVIDER_TIMEOUT_NOTICE}"),
                    None => PROVIDER_TIMEOUT_NOTICE.to_owned(),
                });
                provider_request.retry_notice = retry_notice.clone();
            }
            Err(error) => return Err(error.into()),
            Ok(evidence) => {
                let output = project_model_tool_events(&evidence.events);
                authority
                    .record_context_usage(
                        request.outer_invocation_id,
                        request.agent_context.as_ref(),
                        &provider_request,
                        output.input_tokens,
                        output.output_tokens,
                        super::compaction::Unit::Exchange(ModelToolExchangeV1 {
                            assistant_content: output.assistant_content.clone(),
                            results: Vec::new(),
                        })
                        .tokens(),
                    )
                    .map_err(ModelToolLoopErrorV1::ToolAuthority)?;
                return Ok(evidence);
            }
        }
    }
}

fn append_runtime_notices(target: &mut Option<String>, notices: Vec<String>) {
    for notice in notices {
        match target {
            Some(existing) => {
                existing.push_str("\n\n");
                existing.push_str(&notice);
            }
            None => *target = Some(notice),
        }
    }
}

fn model_facing_tool_result(
    result: &ModelToolResultV1,
    capability_id: &str,
    maximum_bytes: usize,
) -> ModelToolResultV1 {
    let result = super::tool_loop::skills::model_result(result, capability_id);
    let content = super::tool_result_preview::bounded_content(&result.content, maximum_bytes, None);
    ModelToolResultV1 { content: content.unwrap_or_else(|| result.content.clone()), ..result }
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
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(MAXIMUM_DURABLE_EXCHANGE_BYTES),
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = Vec::new();
    let mut activities = Vec::new();
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;
    let mut timeout_recoveries = 0_u32;
    let mut repeat_tool_reminder = RepeatToolReminderStateV1::default();
    let mut pending_runtime_notice = None;
    let mut turn = 1_u32;

    loop {
        let evidence = execute_tool_turn_with_timeout_recovery(
            gateway,
            &plan,
            &request,
            authority,
            turn.saturating_sub(1) as usize,
            &mut exchanges,
            pending_runtime_notice.take(),
            cancellation,
            &mut attempted_model_turns,
            &mut timeout_recoveries,
            &mut input_tokens,
            &mut output_tokens,
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
        let turn_output = project_model_tool_events(&evidence.events);
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
                timeout_recoveries,
                exchanges,
                activities,
            }));
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
        let mut results = Vec::with_capacity(turn_output.calls.len());
        for call in &turn_output.calls {
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
                    results.push(model_facing_tool_result(
                        &settled.result,
                        &call.capability_id,
                        request.maximum_tool_output_bytes,
                    ));
                    activities.push(settled.activity);
                    settled_tool_calls = settled_tool_calls.saturating_add(1);
                    append_runtime_notices(
                        &mut pending_runtime_notice,
                        repeat_tool_reminder.observe_calls(std::slice::from_ref(call)),
                    );
                }
                ToolInvokeV1::Approval(challenge) => {
                    return approval_turn::suspend(ModelToolLoopPendingV1 {
                        turn,
                        call: call.clone(),
                        challenge,
                        exchanges,
                        pending_exchange: Some(ModelToolExchangeV1 {
                            assistant_content: turn_output.assistant_content.clone(),
                            results,
                        }),
                        activities,
                        input_tokens,
                        output_tokens,
                        attempted_model_turns,
                        settled_tool_calls,
                        _legacy_total_calls: None,
                        timeout_recoveries,
                        repeat_tool_reminder,
                        pending_runtime_notice,
                    });
                }
            }
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        if serde_json::to_vec(&exchange)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("individual model/tool exchange byte limit"),
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
        turn = turn.saturating_add(1);
    }
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
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(MAXIMUM_DURABLE_EXCHANGE_BYTES),
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut pending = pending.clone();
    if !approval_turn::resume_pending_turn(
        &request,
        authority,
        &mut pending,
        approved,
        now_epoch_millis,
        cancellation,
    )? {
        return Ok(ModelToolLoopRunV1::Suspended {
            challenge: pending.challenge.clone(),
            pending,
        });
    }
    let mut exchanges = pending.exchanges;
    let mut activities = pending.activities;
    let mut input_tokens = pending.input_tokens;
    let mut output_tokens = pending.output_tokens;
    let mut attempted_model_turns = pending.attempted_model_turns;
    let mut settled_tool_calls = pending.settled_tool_calls;
    let mut timeout_recoveries = pending.timeout_recoveries;
    let mut repeat_tool_reminder = pending.repeat_tool_reminder;
    let mut pending_runtime_notice = pending.pending_runtime_notice;

    let mut turn = pending.turn.saturating_add(1);
    loop {
        let evidence = execute_tool_turn_with_timeout_recovery(
            gateway,
            &plan,
            &request,
            authority,
            turn.saturating_sub(1) as usize,
            &mut exchanges,
            pending_runtime_notice.take(),
            cancellation,
            &mut attempted_model_turns,
            &mut timeout_recoveries,
            &mut input_tokens,
            &mut output_tokens,
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
        let turn_output = project_model_tool_events(&evidence.events);
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
                timeout_recoveries,
                exchanges,
                activities,
            }));
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
        let mut results = Vec::with_capacity(turn_output.calls.len());
        for call in &turn_output.calls {
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
                    results.push(model_facing_tool_result(
                        &settled.result,
                        &call.capability_id,
                        request.maximum_tool_output_bytes,
                    ));
                    activities.push(settled.activity);
                    settled_tool_calls = settled_tool_calls.saturating_add(1);
                    append_runtime_notices(
                        &mut pending_runtime_notice,
                        repeat_tool_reminder.observe_calls(std::slice::from_ref(call)),
                    );
                }
                ToolInvokeV1::Approval(challenge) => {
                    return approval_turn::suspend(ModelToolLoopPendingV1 {
                        turn,
                        call: call.clone(),
                        challenge,
                        exchanges,
                        pending_exchange: Some(ModelToolExchangeV1 {
                            assistant_content: turn_output.assistant_content.clone(),
                            results,
                        }),
                        activities,
                        input_tokens,
                        output_tokens,
                        attempted_model_turns,
                        settled_tool_calls,
                        _legacy_total_calls: None,
                        timeout_recoveries,
                        repeat_tool_reminder,
                        pending_runtime_notice,
                    });
                }
            }
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        if serde_json::to_vec(&exchange)
            .map_or(true, |bytes| bytes.len() > MAXIMUM_DURABLE_EXCHANGE_BYTES)
        {
            return Err(failure(
                ModelToolLoopErrorV1::Budget("individual model/tool exchange byte limit"),
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
        turn = turn.saturating_add(1);
    }
}
