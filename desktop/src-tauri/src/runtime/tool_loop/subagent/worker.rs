//! Owned execution of one delegated child turn-set.
//!
//! The delegating dispatcher builds a [`ChildTurnRequestV1`] that owns every
//! value the child loop needs, so the exact same execution runs inline for a
//! foreground delegation and on a private thread for a background child job.
use super::jobs::ChildJobHandle;
use super::*;
use crate::runtime::{compaction, model_tool_loop};

/// One settled child turn-set as the lifecycle sees it. A failed loop keeps its
/// exact committed prefix so recovery never re-runs an acknowledged effect.
pub(super) struct ChildTurnV1 {
    pub status: ChildStatusV1,
    pub exchanges: Vec<ModelToolExchangeV1>,
    pub final_text: String,
    pub blocked: Vec<ModelToolCallV1>,
    pub model_turns: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub error: Option<String>,
}

/// Everything one child turn-set needs, owned so it can move to a background
/// thread without borrowing the delegating dispatcher.
pub(super) struct ChildTurnRequestV1 {
    pub runtime: FileToolAuthorityRuntimeV1,
    pub context: FrozenFileToolAuthorityContextV1,
    pub agent: model_tool_loop::AgentContextV1,
    pub run_events: Arc<RunEventStream>,
    pub gateway: Arc<aworkit_capability_host::FrozenModelGateway>,
    pub binding_id: String,
    pub binding_version_hash: String,
    pub maximum_tool_output_bytes: usize,
    pub input: Value,
    pub initial_exchanges: Vec<ModelToolExchangeV1>,
    pub continuation: Option<String>,
    /// Present only for a background child job: receives progress and steering.
    pub job: Option<Arc<ChildJobHandle>>,
}

/// Runs one child turn-set over the same frozen gateway and selected tool
/// authority. The child cannot delegate further: every delegation and control
/// tool is excluded from its definitions and port.
pub(super) fn execute_child_turns(
    request: ChildTurnRequestV1,
    outer_invocation_id: &StableId,
    cancellation: &CancellationToken,
) -> Result<ChildTurnV1, String> {
    let definitions = request
        .context
        .bindings
        .iter()
        .filter(|binding| binding.is_callable())
        .map(StoredFileToolBindingV1::definition)
        .collect::<Vec<_>>();
    // Every child owns its evidence stream, so every child owns the observer
    // that writes its model spans into that stream. Handing a child the
    // delegating pass's frozen observer would attribute its activity to the
    // parent rather than to the child.
    let observer = Some(Arc::new(
        crate::runtime::run_events::ModelRunEventObserver::new(request.run_events.clone()),
    ) as Arc<dyn aworkit_capability_host::ModelEventObserverV1>);
    let child_authority = SubagentToolPortV1 {
        blocked: Mutex::new(Vec::new()),
        inner: BoundFileToolAuthorityV1 {
            runtime: request.runtime,
            context: request.context,
            run_events: request.run_events,
            steering_node: None,
        },
        job: request.job,
        observer,
    };
    let initial_context = request
        .continuation
        .map(|message| {
            vec![ModelToolContextV1 {
                after_exchanges: request.initial_exchanges.len(),
                after_input_messages: None,
                instruction_event_id: None,
                content: message,
                role: None,
                images: Vec::new(),
            }]
        })
        .unwrap_or_default();
    match execute_model_tool_loop_v1(
        &request.gateway,
        model_tool_loop::ModelToolLoopRequestV1 {
            agent_context: Some(request.agent),
            outer_invocation_id,
            input: request.input,
            initial_context,
            initial_exchanges: request.initial_exchanges,
            parameters: BTreeMap::new(),
            definitions,
            binding_id: request.binding_id,
            binding_version_hash: request.binding_version_hash,
            maximum_input_bytes: SUBAGENT_MAXIMUM_INPUT_BYTES,
            maximum_output_bytes: SUBAGENT_MAXIMUM_OUTPUT_BYTES,
            maximum_tool_output_bytes: request.maximum_tool_output_bytes,
            maximum_timeout_recoveries: PROVIDER_TIMEOUT_RECOVERIES_V1,
        },
        &child_authority,
        cancellation,
    ) {
        Ok(completed) => {
            let blocked = child_authority
                .blocked
                .lock()
                .map_err(|_| "child handoff lock poisoned")?
                .clone();
            Ok(ChildTurnV1 {
                status: if blocked.is_empty() {
                    ChildStatusV1::Completed
                } else {
                    ChildStatusV1::ParentApprovalRequired
                },
                exchanges: completed.exchanges,
                final_text: completed.assistant_text,
                blocked,
                model_turns: completed.attempted_model_turns,
                tool_calls: completed.settled_tool_calls,
                input_tokens: completed.input_tokens,
                output_tokens: completed.output_tokens,
                error: None,
            })
        }
        Err(failure) => {
            let cancelled = cancellation.is_cancelled();
            Ok(ChildTurnV1 {
                status: if cancelled {
                    ChildStatusV1::Cancelled
                } else {
                    ChildStatusV1::Failed
                },
                exchanges: failure.exchanges,
                final_text: if cancelled {
                    "Subagent cancelled before it settled.".to_owned()
                } else {
                    failure.error.to_string()
                },
                blocked: Vec::new(),
                model_turns: failure.attempted_model_turns,
                tool_calls: failure.settled_tool_calls,
                input_tokens: failure.input_tokens,
                output_tokens: failure.output_tokens,
                error: if cancelled {
                    None
                } else {
                    Some(format!("subagent failed: {}", failure.error))
                },
            })
        }
    }
}

/// Uses the same broker and existing grants as the parent, but cannot open a
/// new approval or delegate again. A blocked action returns immediately.
pub(super) struct SubagentToolPortV1 {
    inner: BoundFileToolAuthorityV1,
    blocked: Mutex<Vec<ModelToolCallV1>>,
    job: Option<Arc<ChildJobHandle>>,
    observer: Option<Arc<dyn aworkit_capability_host::ModelEventObserverV1>>,
}

impl ModelToolInvocationPortV1 for SubagentToolPortV1 {
    fn project_context(&self) -> Option<Value> {
        self.inner.project_context()
    }
    fn handoff_notice(&self) -> Option<String> {
        self.blocked
            .lock()
            .ok()
            .filter(|b| !b.is_empty())
            .map(|_| APPROVAL_HANDOFF.to_owned())
    }
    fn outstanding_jobs(&self) -> Result<Option<String>, String> {
        self.inner.runtime.jobs.completion_notice_scoped(
            &self.inner.context.chat_id,
            self.inner.context.delegation.as_ref().map(StableId::as_str),
        )
    }
    fn stop_unkept_jobs(&self) {
        self.inner.runtime.jobs.stop_unkept_scoped(
            &self.inner.context.chat_id,
            self.inner.context.delegation.as_ref().map(StableId::as_str),
        );
    }
    fn legacy_context_identity(&self) -> bool {
        self.inner.legacy_context_identity()
    }
    fn model_observer(
        &self,
    ) -> Option<Arc<dyn aworkit_capability_host::ModelEventObserverV1>> {
        self.observer.clone()
    }
    fn manage_model_context(
        &self,
        gateway: &aworkit_capability_host::FrozenModelGateway,
        plan: &aworkit_capability_host::ModelResolutionPlanV1,
        outer: &StableId,
        through: usize,
        agent: Option<&model_tool_loop::AgentContextV1>,
        request: &mut aworkit_capability_host::ModelToolRequestV1,
        cancellation: &CancellationToken,
        trigger: compaction::Trigger,
    ) -> Result<compaction::Preparation, String> {
        self.inner.manage_context(
            gateway,
            plan,
            outer,
            through,
            agent,
            request,
            cancellation,
            trigger,
        )
    }
    fn record_context_usage(
        &self,
        outer: &StableId,
        agent: Option<&model_tool_loop::AgentContextV1>,
        request: &aworkit_capability_host::ModelToolRequestV1,
        input: u64,
        output: u64,
        assistant_tokens: u64,
    ) -> Result<(), String> {
        self.inner
            .context_usage(outer, agent, request, input, output, assistant_tokens)
    }
    fn prepare_automatic_context(
        &self,
        outer: &StableId,
        after_exchanges: usize,
        agent: &model_tool_loop::AgentContextV1,
        request: &mut aworkit_capability_host::ModelToolRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        self.inner
            .workspace_context(outer, after_exchanges, agent, request, cancellation)
    }
    fn prepare_context(
        &self,
        outer: &StableId,
        after_exchanges: usize,
        definitions: &[ModelToolDefinitionV1],
        cancellation: &CancellationToken,
    ) -> Result<Vec<aworkit_capability_host::ModelToolContextV1>, String> {
        let mut messages =
            self.inner
                .skill_context(outer, after_exchanges, definitions, false, cancellation)?;
        // Parent steering queued through job_input lands at the next step
        // boundary, exactly where durable injected context belongs.
        if let Some(job) = &self.job {
            for text in job.drain_steering() {
                messages.push(ModelToolContextV1 {
                    after_exchanges,
                    content: format!(
                        "Steering message from the delegating Agent; adjust the work accordingly:\n{text}"
                    ),
                    role: None,
                    ..Default::default()
                });
            }
        }
        Ok(messages)
    }

    fn note_child_turn(&self, turn: u32, assistant_text: &str, calls: &[ModelToolCallV1]) {        let Some(job) = &self.job else {
            return;
        };
        let tools = calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let line = if tools.is_empty() {
            format!("turn {turn}: {assistant_text}")
        } else {
            format!("turn {turn}: {tools}")
        };
        job.progress(
            &line,
            json!({
                "childId": self.inner.context.delegation,
                "status": "running",
                "turn": turn,
                "assistantText": assistant_text,
                "tools": tools,
            }),
        );
    }

    fn invoke(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        self.guard(call)?;
        let settled = self
            .inner
            .invoke_v1_scoped(outer_invocation_id, turn, call, cancellation)
            .map_err(|error| error.to_string())?;
        if settled.result.content["error"] == "parent_approval_required" {
            self.blocked
                .lock()
                .map_err(|_| "child handoff lock poisoned")?
                .push(call.clone());
        }
        Ok(settled)
    }

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

    fn commit_exchange(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), String> {
        self.inner
            .commit_exchange(outer_invocation_id, turn, exchange)
    }
}

impl SubagentToolPortV1 {
    fn guard(&self, call: &ModelToolCallV1) -> Result<(), String> {
        if is_subagent_tool(&call.capability_id) {
            return Err("tool is not available to subagent children".to_owned());
        }
        if !self
            .inner
            .context
            .bindings
            .iter()
            .any(|b| b.is_callable() && b.capability_id == call.capability_id)
        {
            return Err("tool is not available to subagent children".to_owned());
        }
        Ok(())
    }
}
