//! Provider/tool orchestration for one frozen Agent invocation.
//!
//! Providers can only request tools. Every request is settled by the injected
//! trusted-core port before the exact result is sent back on the next model
//! turn. This module owns neither workspace authority nor tool execution.
//! Token totals are usage accounting only, including after approval resume and
//! inside subagents; they impose no cumulative run limit.

use std::collections::BTreeMap;

use aworkit_capability_host::{
    CancellationToken, FrozenModelGateway, ModelAssistantContentV1, ModelCacheTotalsV1,
    ModelCandidateV1, ModelResolutionPlanV1, ModelToolCallV1, ModelToolDefinitionV1,
    ModelToolDispatchEvidenceV1, ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1,
    ProviderError, project_model_tool_events,
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
mod job_completion;
mod provider_recovery;

pub(crate) use provider_recovery::provider_recovery_notice;

// Headroom for context compaction, not a limit on persisted exchanges.
const TOOL_CONTEXT_HEADROOM_BYTES: usize = 512 * 1024;
/// Transient provider failures (request timeout, stream interruption) retry the
/// same frozen request instead of aborting the Agent node. A single retry is too
/// brittle for flaky transports, so allow a generous bounded budget; the pass has
/// no aggregate deadline, so the user remains the only hard stop.
pub(crate) const PROVIDER_TIMEOUT_RECOVERIES_V1: u32 = 32;
/// How many provider failures are reported in full before the notice switches to
/// a compact repeat reminder. This is a wording threshold, never a stop: the
/// Agent node ends only on a final answer, cancellation, or an unrecoverable
/// authority denial, so a repeating failure keeps being reported to the model.
const MAXIMUM_ERROR_RECOVERIES: u32 = 32;

/// The provider-failure report ledger for one Agent invocation.
///
/// Every failure produces a model-visible notice, whether or not it repeats. A
/// failure that repeats *identically* still produces one: the notice says the
/// request has not changed, which is the fact the model needs to decide to do
/// something different. The ledger never gates the loop, because a report is
/// always available and an unavailable report is never a reason to end a node.
#[derive(Default)]
struct ProviderRecoveryBudget {
    reports: u32,
    reported: std::collections::BTreeSet<String>,
}

impl ProviderRecoveryBudget {
    /// The model-visible notice for one provider failure, written onto the frozen
    /// request. Bounded like every other advisory, so it can never make the
    /// request invalid and can never end the turn.
    fn note(
        &mut self,
        error: &ProviderError,
        provider_request: &mut ModelToolRequestV1,
    ) -> String {
        self.reports = self.reports.saturating_add(1);
        // The ledger exists only to tell a repeat from a new condition. It is
        // bounded so a long run cannot grow it without limit; forgetting an old
        // key costs one extra full notice, never a stop.
        const MAXIMUM_REMEMBERED_FAILURES: usize = 256;
        if self.reported.len() >= MAXIMUM_REMEMBERED_FAILURES {
            self.reported.clear();
        }
        let repeated = !self.reported.insert(error.to_string());
        let notice = if repeated {
            format!(
                "Aworkit recovery notice: the same provider failure repeated ({error}). The request \
                 has not changed since the previous attempt, so the same outcome is expected. Take \
                 a different next action, or tell the user plainly what could not be completed."
            )
        } else {
            provider_report_notice(error, self.reports)
        };
        let merged = match provider_request.retry_notice.take() {
            Some(pending) => format!("{pending}\n\n{notice}"),
            None => notice,
        };
        let bounded = aworkit_capability_host::bound_model_notice(&merged);
        provider_request.retry_notice = Some(bounded.clone());
        bounded
    }
}
pub(crate) const PROVIDER_TIMEOUT_NOTICE: &str = "Aworkit recovery notice: the previous provider request timed out before a complete response was received. Any partial response from that attempt was discarded. Continue the task using the conversation and completed tool results available here.";

/// The model-visible notice for a reported provider failure. The escalation is
/// advice, never a stop: the model decides when the task is done.
fn provider_report_notice(error: &ProviderError, recovery: u32) -> String {
    let notice = provider_recovery_notice(error).unwrap_or_else(|| format!("{error}"));
    if recovery <= MAXIMUM_ERROR_RECOVERIES {
        return notice;
    }
    format!(
        "{notice}\n\nThis failure has repeated {recovery} times. The provider has not accepted this \
         request; nothing was executed and no history was discarded. Continue with a different next \
         step, or tell the user plainly what is blocking."
    )
}

/// Reports one provider or context condition on the frozen request as
/// model-visible context.
///
/// Every condition the authority owns is reported here: the provider rejecting
/// the acting request, a rejected auxiliary compaction request, and a condition
/// raised by context preparation. Reporting is unconditional and never returns
/// whether it was "granted", because no provider, transport, contract, budget or
/// context condition may end an Agent node.
fn report_provider_failure(
    recovery: &mut ProviderRecoveryBudget,
    error: &ProviderError,
    provider_request: &mut ModelToolRequestV1,
) {
    recovery.note(error, provider_request);
}

/// Report a context-preparation condition on the acting request.
///
/// Context preparation owns no termination: the model is told, the frozen
/// request is dispatched as it stands, and the loop continues. The notice is
/// bounded like every other advisory.
fn report_context_condition(request: &mut ModelToolRequestV1, error: &str) {
    append_runtime_notices(
        &mut request.retry_notice,
        vec![format!(
            "Aworkit context notice: this turn's context could not be fully prepared ({error}). No \
             history was discarded and no tool was replayed. Continue from the conversation and the \
             completed tool results available here; if a result you need is missing, ask for it again."
        )],
    );
}

/// Clamp every advisory field before dispatch. A bound may shorten what the model
/// is told; it can never fail the request or end the node.
fn bound_request_notices(request: &mut ModelToolRequestV1) {
    let bounded = request
        .retry_notice
        .as_deref()
        .map(aworkit_capability_host::bound_model_notice);
    if bounded.is_some() {
        request.retry_notice = bounded;
    }
}

/// Trusted-core boundary used by the provider loop. Implementations must
/// durably settle a call before returning its provider-facing result.
pub(crate) trait ModelToolInvocationPortV1 {
    /// A delegated invocation can return an unmet permission to its parent
    /// after committing the current exchange, without another provider call.
    fn handoff_notice(&self) -> Option<String> { None }
    /// Runtime facts; implementations must not accept model claims as job settlement.
    fn outstanding_jobs(&self) -> Result<Option<String>, String> { Ok(None) }
    fn stop_unkept_jobs(&self) {}
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
    /// Records the exact conversation prefix the delegating Agent had committed
    /// when this turn's tools became callable. A forked child builds its
    /// declared projection from that snapshot, so the same fork always yields
    /// the same child prefix. Delegated children deliberately do not forward
    /// this: only their delegating parent records it.
    fn observe_parent_turn(&self, _input: &Value, _exchanges: &[ModelToolExchangeV1]) {}
    /// Publishes one delegated child turn's progress to its background job, so
    /// the delegating Agent can read live state through `job_output`. The
    /// top-level Agent authority does not forward this.
    fn note_child_turn(&self, _turn: u32, _assistant_text: &str, _calls: &[ModelToolCallV1]) {}
    /// Evidence observer for this invocation. A background child returns its
    /// own stream so its live model evidence is not committed under the
    /// delegating pass's span tree.
    fn model_observer(
        &self,
    ) -> Option<std::sync::Arc<dyn aworkit_capability_host::ModelEventObserverV1>> {
        None
    }
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
    /// Provider-reported cache counters for the turns this prefix already
    /// billed. Checkpoints written before the aggregate existed restore them as
    /// unknown rather than as a fabricated zero.
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_miss_input_tokens: Option<u64>,
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

impl ModelToolLoopPendingV1 {
    /// Cache counters accumulated by the turns captured in this suspension.
    pub(crate) fn cache_totals(&self) -> ModelCacheTotalsV1 {
        ModelCacheTotalsV1 {
            cached_input_tokens: self.cached_input_tokens,
            cache_miss_input_tokens: self.cache_miss_input_tokens,
        }
    }
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
    /// The Agent node's compaction overlay. Absent on a Chat-level context and
    /// on delegated child scopes, which keep the frozen Chat policy.
    pub compaction: Option<crate::runtime::compaction::Overlay>,
}

pub(crate) struct ModelToolLoopRequestV1<'a> {
    pub agent_context: Option<AgentContextV1>,
    pub outer_invocation_id: &'a StableId,
    pub input: Value,
    /// Frozen context at local exchange boundary zero. Durable restoration
    /// rebases it after prior history and imports it only once per invocation.
    pub initial_context: Vec<aworkit_capability_host::ModelToolContextV1>,
    /// Exchanges this invocation continues from. Empty for a fresh invocation;
    /// a resumed child conversation supplies its committed prefix so the model
    /// sees the same durable history without replaying any settled effect.
    pub initial_exchanges: Vec<ModelToolExchangeV1>,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelToolLoopOutcomeV1 {
    pub assistant_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Whole-invocation provider cache counters. Absent when no turn reported
    /// them, so a consumer never shows a fabricated zero.
    pub cached_input_tokens: Option<u64>,
    pub cache_miss_input_tokens: Option<u64>,
    pub attempted_model_turns: u32,
    pub settled_tool_calls: u32,
    pub timeout_recoveries: u32,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub activities: Vec<WorkflowToolActivityV1>,
}

impl ModelToolLoopOutcomeV1 {
    /// Cache counters this invocation's turns reported, for a parent aggregate.
    pub(crate) fn cache_totals(&self) -> ModelCacheTotalsV1 {
        ModelCacheTotalsV1 {
            cached_input_tokens: self.cached_input_tokens,
            cache_miss_input_tokens: self.cache_miss_input_tokens,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ModelToolLoopErrorV1 {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The trusted core refused to settle or authorise a requested capability.
    #[error("tool authority rejected the provider request: {0}")]
    ToolAuthority(String),
    #[error("Agent model/tool budget is exhausted: {0}")]
    Budget(&'static str),
    #[error("provider accepted the Agent turn but returned no final assistant text")]
    MissingAssistantOutput,
}

impl ModelToolLoopFailureV1 {
    /// Cache counters the turns before the failure reported.
    pub(crate) fn cache_totals(&self) -> ModelCacheTotalsV1 {
        ModelCacheTotalsV1 {
            cached_input_tokens: self.cached_input_tokens,
            cache_miss_input_tokens: self.cache_miss_input_tokens,
        }
    }
}

#[derive(Debug, Error)]
#[error("{error}")]
pub(crate) struct ModelToolLoopFailureV1 {
    pub error: ModelToolLoopErrorV1,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub cache_miss_input_tokens: Option<u64>,
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
    validate_limits(&request)
        .map_err(|error| failure(error, 0, 0, ModelCacheTotalsV1::default(), 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(TOOL_CONTEXT_HEADROOM_BYTES),
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = request.initial_exchanges.clone();
    let mut activities = Vec::new();
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut cache = ModelCacheTotalsV1::default();
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;
    let mut timeout_recoveries = 0_u32;
    let mut repeat_tool_reminder = RepeatToolReminderStateV1::default();
    let mut pending_runtime_notice = None;
    let mut job_completion = job_completion::CompletionGuard::default();
    let mut recovery = ProviderRecoveryBudget::default();
    let mut turn = u32::try_from(exchanges.len())
        .unwrap_or(u32::MAX)
        .saturating_add(1);

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
            &mut recovery,
            &mut input_tokens,
            &mut output_tokens,
        )
        .map_err(|error| {
            failure(
                error.into(),
                input_tokens,
                output_tokens,
                cache,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = project_model_tool_events(&evidence.events);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        cache.add(turn_output.cache);

        if turn_output.calls.is_empty() {
            if job_completion.defer(authority, request.outer_invocation_id, turn, &turn_output.assistant_content, &mut exchanges, &mut pending_runtime_notice) {
                turn = turn.saturating_add(1);
                continue;
            }
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    cache,
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
                cached_input_tokens: cache.cached_input_tokens,
                cache_miss_input_tokens: cache.cache_miss_input_tokens,
                attempted_model_turns,
                settled_tool_calls,
                timeout_recoveries,
                exchanges,
                activities,
            });
        }
        let mut results = Vec::with_capacity(turn_output.calls.len());
        job_completion.progressed();
        for (index, call) in turn_output.calls.iter().enumerate() {
            let settled = match authority.invoke(
                request.outer_invocation_id,
                turn,
                call,
                cancellation,
            ) {
                Ok(settled) => settled,
                Err(error) => {
                    // A tool that cannot settle is reported to the model as an
                    // error result it can act on; it never ends the node.
                    let result = tool_failure_result(call, &error);
                    results.push(model_facing_tool_result(
                        &result,
                        &call.capability_id,
                        request.maximum_tool_output_bytes,
                    ));
                    append_runtime_notices(
                        &mut pending_runtime_notice,
                        vec![tool_failure_notice(call, &error)],
                    );
                    continue;
                }
            };
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
            if authority.handoff_notice().is_some() {
                // Balance the provider transcript without executing remaining
                // proposals after the child has returned its scope to its parent.
                results.extend(turn_output.calls[index + 1..].iter().map(|pending| ModelToolResultV1 {
                    call_id: pending.call_id.clone(), images: Vec::new(), is_error: true,
                    content: serde_json::json!({"error":"not_executed","detail":"Child returned to parent for approval before this call executed."}),
                }));
                break;
            }
        }
        let exchange = ModelToolExchangeV1 {
            assistant_content: turn_output.assistant_content,
            results,
        };
        // A failed durable commit is the trusted core refusing to record the
        // exchange: that is an unrecoverable authority denial, one of the three
        // conditions allowed to end the node. A tool error never lands here.
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    cache,
                    attempted_model_turns,
                    settled_tool_calls,
                    &exchanges,
                    &activities,
                )
            })?;
        exchanges.push(exchange);
        if let Some(assistant_text) = authority.handoff_notice() {
            return Ok(ModelToolLoopOutcomeV1 {
                assistant_text,
                input_tokens,
                output_tokens,
                cached_input_tokens: cache.cached_input_tokens,
                cache_miss_input_tokens: cache.cache_miss_input_tokens,
                attempted_model_turns,
                settled_tool_calls,
                timeout_recoveries,
                exchanges,
                activities,
            });
        }
        turn = turn.saturating_add(1);
    }
}

fn failure(
    error: ModelToolLoopErrorV1,
    input_tokens: u64,
    output_tokens: u64,
    cache: ModelCacheTotalsV1,
    attempted_model_turns: u32,
    settled_tool_calls: u32,
    exchanges: &[ModelToolExchangeV1],
    activities: &[WorkflowToolActivityV1],
) -> ModelToolLoopFailureV1 {
    ModelToolLoopFailureV1 {
        error,
        input_tokens,
        output_tokens,
        cached_input_tokens: cache.cached_input_tokens,
        cache_miss_input_tokens: cache.cache_miss_input_tokens,
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
    _timeout_recoveries: &mut u32,
    recovery: &mut ProviderRecoveryBudget,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
) -> Result<ModelToolDispatchEvidenceV1, ModelToolLoopErrorV1> {
    let mut retry_notice = runtime_notice;
    // Context assembly and the job inventory are reports, not gates: a failure
    // here becomes a model-visible notice and the frozen request still dispatches.
    let context_messages = match authority.prepare_context(
        request.outer_invocation_id,
        through,
        &request.definitions,
        cancellation,
    ) {
        Ok(messages) => messages,
        Err(error) => {
            append_runtime_notices(
                &mut retry_notice,
                vec![format!(
                    "Aworkit context notice: this turn's instruction context could not be prepared \
                     ({error}). Continue with the context already in this conversation."
                )],
            );
            Vec::new()
        }
    };
    match authority.outstanding_jobs() {
        Ok(Some(jobs)) => append_runtime_notices(
            &mut retry_notice,
            vec![format!("Current shell jobs (other work may continue while they run): {jobs}")],
        ),
        Ok(None) => {}
        Err(error) => append_runtime_notices(
            &mut retry_notice,
            vec![format!(
                "Aworkit job notice: the background-job inventory could not be read ({error}). Do \
                 not assume there is no background work; call job_list if it matters."
            )],
        ),
    }
    let mut provider_request = ModelToolRequestV1 {
        context_messages: request
            .initial_context
            .iter()
            .cloned()
            .chain(context_messages)
            .collect(),
        input: request.input.clone(),
        parameters: request.parameters.clone(),
        tools: request.definitions.clone(),
        exchanges: exchanges.clone(),
        retry_notice: retry_notice.clone(),
    };
    let preparation = match authority.manage_model_context(
        gateway,
        plan,
        request.outer_invocation_id,
        through,
        request.agent_context.as_ref(),
        &mut provider_request,
        cancellation,
        super::compaction::Trigger::Pressure,
    ) {
        Ok(preparation) => preparation,
        Err(error) => {
            // Context preparation owns no termination: report it and dispatch the
            // frozen request as it stands.
            report_context_condition(&mut provider_request, &error);
            super::compaction::Preparation::default()
        }
    };
    *input_tokens = input_tokens.saturating_add(preparation.input_tokens);
    *output_tokens = output_tokens.saturating_add(preparation.output_tokens);
    // A provider failure inside the auxiliary compaction request is reported on
    // this turn's frozen request. It is not an authority rejection and must not
    // end the node: the model, not a limit, decides how to continue.
    if let Some(error) = &preparation.provider_error {
        report_provider_failure(recovery, error, &mut provider_request);
    }
    if let Some(error) = preparation.error {
        report_context_condition(&mut provider_request, &error);
    }
    bound_request_notices(&mut provider_request);
    if preparation.durable {
        *exchanges = provider_request.exchanges.clone();
    }
    // The delegating Agent publishes the exact prefix its tools can fork from.
    // This is captured before dispatch, so every call in the turn sees the same
    // deterministic projection input.
    authority.observe_parent_turn(&provider_request.input, &provider_request.exchanges);
    let mut overflow_retries = 0;
    loop {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        *attempted_model_turns = attempted_model_turns.saturating_add(1);
        // A background child supplies its own evidence observer; every other
        // invocation keeps the gateway's frozen observer.
        let turn = match authority.model_observer() {
            Some(observer) => gateway.execute_tool_turn_with_observer(
                plan,
                &provider_request,
                cancellation,
                Some(observer.as_ref()),
            ),
            None => gateway.execute_tool_turn_cancellable(plan, &provider_request, cancellation),
        };
        match turn {
            Err(error) if is_context_overflow(&error)
                && overflow_retries < preparation.max_overflow_retries =>
            {
                let reduction = match authority.manage_model_context(
                    gateway,
                    plan,
                    request.outer_invocation_id,
                    through,
                    request.agent_context.as_ref(),
                    &mut provider_request,
                    cancellation,
                    super::compaction::Trigger::ContextOverflow,
                ) {
                    Ok(reduction) => reduction,
                    Err(error) => {
                        report_context_condition(&mut provider_request, &error);
                        super::compaction::Preparation::default()
                    }
                };
                *input_tokens = input_tokens.saturating_add(reduction.input_tokens);
                *output_tokens = output_tokens.saturating_add(reduction.output_tokens);
                if let Some(error) = &reduction.provider_error {
                    report_provider_failure(recovery, error, &mut provider_request);
                }
                if let Some(error) = reduction.error {
                    report_context_condition(&mut provider_request, &error);
                }
                bound_request_notices(&mut provider_request);
                if cancellation.is_cancelled() {
                    return Err(ProviderError::Cancelled.into());
                }
                if !reduction.changed {
                    // The selection is already as small as the authority can make
                    // it. Report the condition to the model; a context condition
                    // never ends the node.
                    report_provider_failure(recovery, &error, &mut provider_request);
                    continue;
                }
                if reduction.durable {
                    *exchanges = provider_request.exchanges.clone();
                }
                overflow_retries += 1;
            }
            Err(error) if provider_recovery_notice(&error).is_some() => {
                // Every provider failure that is not cancellation becomes the
                // model's next turn: the exact failure is reported on the same
                // frozen route and the Agent decides what to do about it. There is
                // no report budget that can end the node.
                report_provider_failure(recovery, &error, &mut provider_request);
            }
            Err(error) => return Err(error.into()),
            Ok(evidence) => {
                let output = project_model_tool_events(&evidence.events);
                if let Err(error) = authority.record_context_usage(
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
                ) {
                    // Usage accounting is telemetry: a failure to record it is
                    // reported and the dispatch result is still returned.
                    report_context_condition(&mut provider_request, &error);
                }
                authority.note_child_turn(
                    through.saturating_add(1) as u32,
                    &output.assistant_text,
                    &output.calls,
                );
                return Ok(evidence);
            }
        }
    }
}

/// A provider turn can reject the request because the model's context window
/// overflowed or because the exact request no longer fits the frozen plan's
/// input bound. Both are context conditions: the authority reduces the durable
/// selection and retries the same frozen route instead of failing the node.
pub(crate) fn is_context_overflow(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::ContextWindowExceeded | ProviderError::InputBoundExceeded { .. }
    )
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
    // Advisory text is bounded here as well as at its producer, so no sequence of
    // notices can make the frozen request structurally invalid.
    let bounded = target
        .as_deref()
        .map(aworkit_capability_host::bound_model_notice);
    if bounded.is_some() {
        *target = bounded;
    }
}

fn model_facing_tool_result(
    result: &ModelToolResultV1,
    capability_id: &str,
    maximum_bytes: usize,
) -> ModelToolResultV1 {
    let result = super::tool_loop::skills::model_result(result, capability_id);
    let content = super::tool_result_preview::bounded_content(&result.content, maximum_bytes, None);
    ModelToolResultV1 {
        content: content.unwrap_or_else(|| result.content.clone()),
        ..result
    }
}

/// The model-visible error result for a tool call the authority could not settle.
///
/// A failed call is a result the model reads and reacts to, never a node
/// termination: the call id stays paired so the provider transcript is balanced.
fn tool_failure_result(call: &ModelToolCallV1, error: &str) -> ModelToolResultV1 {
    ModelToolResultV1 {
        call_id: call.call_id.clone(),
        content: serde_json::json!({"error": error}),
        is_error: true,
        images: Vec::new(),
    }
}

/// The advisory that accompanies a tool call reported as an error result.
fn tool_failure_notice(call: &ModelToolCallV1, error: &str) -> String {
    format!(
        "Aworkit tool notice: {} could not be settled ({error}). The call is reported to you as an \
         error result; choose another action or tell the user what is blocked.",
        call.name
    )
}

/// Runs the frozen model/tool loop with approval awareness. A PerInvocation
/// tool binding suspends the loop with a durable prefix instead of failing.
pub(crate) fn execute_model_tool_loop_approval_v1(
    gateway: &FrozenModelGateway,
    request: ModelToolLoopRequestV1<'_>,
    authority: &dyn ModelToolInvocationPortV1,
    cancellation: &CancellationToken,
) -> Result<ModelToolLoopRunV1, ModelToolLoopFailureV1> {
    validate_limits(&request)
        .map_err(|error| failure(error, 0, 0, ModelCacheTotalsV1::default(), 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(TOOL_CONTEXT_HEADROOM_BYTES),
        maximum_output_bytes: request.maximum_output_bytes,
    };
    let mut exchanges = request.initial_exchanges.clone();
    let mut activities = Vec::new();
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut cache = ModelCacheTotalsV1::default();
    let mut attempted_model_turns = 0_u32;
    let mut settled_tool_calls = 0_u32;
    let mut timeout_recoveries = 0_u32;
    let mut repeat_tool_reminder = RepeatToolReminderStateV1::default();
    let mut pending_runtime_notice = None;
    let mut job_completion = job_completion::CompletionGuard::default();
    let mut recovery = ProviderRecoveryBudget::default();
    let mut turn = u32::try_from(exchanges.len())
        .unwrap_or(u32::MAX)
        .saturating_add(1);

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
            &mut recovery,
            &mut input_tokens,
            &mut output_tokens,
        )
        .map_err(|error| {
            failure(
                error.into(),
                input_tokens,
                output_tokens,
                cache,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = project_model_tool_events(&evidence.events);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        cache.add(turn_output.cache);
        if turn_output.calls.is_empty() {
            if job_completion.defer(authority, request.outer_invocation_id, turn, &turn_output.assistant_content, &mut exchanges, &mut pending_runtime_notice) {
                turn = turn.saturating_add(1);
                continue;
            }
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    cache,
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
                cached_input_tokens: cache.cached_input_tokens,
                cache_miss_input_tokens: cache.cache_miss_input_tokens,
                attempted_model_turns,
                settled_tool_calls,
                timeout_recoveries,
                exchanges,
                activities,
            }));
        }
        let mut results = Vec::with_capacity(turn_output.calls.len());
        job_completion.progressed();
        for call in &turn_output.calls {
            if cancellation.is_cancelled() {
                results.push(approval_turn::not_executed_after_stop(call));
                continue;
            }
            let settled = match authority.invoke_extended(
                request.outer_invocation_id,
                turn,
                call,
                cancellation,
            ) {
                Ok(settled) => settled,
                Err(error) => {
                    // A tool that cannot settle is reported to the model as an
                    // error result it can act on; it never ends the node.
                    let result = tool_failure_result(call, &error);
                    results.push(model_facing_tool_result(
                        &result,
                        &call.capability_id,
                        request.maximum_tool_output_bytes,
                    ));
                    append_runtime_notices(
                        &mut pending_runtime_notice,
                        vec![tool_failure_notice(call, &error)],
                    );
                    continue;
                }
            };
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
                        cached_input_tokens: cache.cached_input_tokens,
                        cache_miss_input_tokens: cache.cache_miss_input_tokens,
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
        // A failed durable commit is the trusted core refusing to record the
        // exchange: that is an unrecoverable authority denial, one of the three
        // conditions allowed to end the node. A tool error never lands here.
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    cache,
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
    validate_limits(&request)
        .map_err(|error| failure(error, 0, 0, ModelCacheTotalsV1::default(), 0, 0, &[], &[]))?;
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: request.binding_id.clone(),
            version_hash: request.binding_version_hash.clone(),
        }],
        maximum_input_bytes: request
            .maximum_input_bytes
            .saturating_add(TOOL_CONTEXT_HEADROOM_BYTES),
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
    // Read the aggregate before the prefix fields are moved out of `pending`.
    let mut cache = pending.cache_totals();
    let mut exchanges = pending.exchanges;
    let mut activities = pending.activities;
    let mut input_tokens = pending.input_tokens;
    let mut output_tokens = pending.output_tokens;
    let mut attempted_model_turns = pending.attempted_model_turns;
    let mut settled_tool_calls = pending.settled_tool_calls;
    let mut timeout_recoveries = pending.timeout_recoveries;
    let mut repeat_tool_reminder = pending.repeat_tool_reminder;
    let mut pending_runtime_notice = pending.pending_runtime_notice;
    let mut job_completion = job_completion::CompletionGuard::default();
    let mut recovery = ProviderRecoveryBudget::default();

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
            &mut recovery,
            &mut input_tokens,
            &mut output_tokens,
        )
        .map_err(|error| {
            failure(
                error.into(),
                input_tokens,
                output_tokens,
                cache,
                attempted_model_turns,
                settled_tool_calls,
                &exchanges,
                &activities,
            )
        })?;
        let turn_output = project_model_tool_events(&evidence.events);
        input_tokens = input_tokens.saturating_add(turn_output.input_tokens);
        output_tokens = output_tokens.saturating_add(turn_output.output_tokens);
        cache.add(turn_output.cache);
        if turn_output.calls.is_empty() {
            if job_completion.defer(authority, request.outer_invocation_id, turn, &turn_output.assistant_content, &mut exchanges, &mut pending_runtime_notice) {
                turn = turn.saturating_add(1);
                continue;
            }
            let assistant_text = turn_output.assistant_text.trim().to_owned();
            if assistant_text.is_empty() {
                return Err(failure(
                    ModelToolLoopErrorV1::MissingAssistantOutput,
                    input_tokens,
                    output_tokens,
                    cache,
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
                cached_input_tokens: cache.cached_input_tokens,
                cache_miss_input_tokens: cache.cache_miss_input_tokens,
                attempted_model_turns,
                settled_tool_calls,
                timeout_recoveries,
                exchanges,
                activities,
            }));
        }
        let mut results = Vec::with_capacity(turn_output.calls.len());
        job_completion.progressed();
        for call in &turn_output.calls {
            if cancellation.is_cancelled() {
                results.push(approval_turn::not_executed_after_stop(call));
                continue;
            }
            let settled = match authority.invoke_extended(
                request.outer_invocation_id,
                turn,
                call,
                cancellation,
            ) {
                Ok(settled) => settled,
                Err(error) => {
                    // A tool that cannot settle is reported to the model as an
                    // error result it can act on; it never ends the node.
                    let result = tool_failure_result(call, &error);
                    results.push(model_facing_tool_result(
                        &result,
                        &call.capability_id,
                        request.maximum_tool_output_bytes,
                    ));
                    append_runtime_notices(
                        &mut pending_runtime_notice,
                        vec![tool_failure_notice(call, &error)],
                    );
                    continue;
                }
            };
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
                        cached_input_tokens: cache.cached_input_tokens,
                        cache_miss_input_tokens: cache.cache_miss_input_tokens,
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
        // A failed durable commit is the trusted core refusing to record the
        // exchange: that is an unrecoverable authority denial, one of the three
        // conditions allowed to end the node. A tool error never lands here.
        authority
            .commit_exchange(request.outer_invocation_id, turn, &exchange)
            .map_err(|error| {
                failure(
                    ModelToolLoopErrorV1::ToolAuthority(error),
                    input_tokens,
                    output_tokens,
                    cache,
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
