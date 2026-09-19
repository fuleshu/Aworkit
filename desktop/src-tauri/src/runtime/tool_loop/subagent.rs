//! Frozen delegation scope, child execution, and return to the parent.
use super::*;
use crate::runtime::{compaction, model_tool_loop};

pub(super) const APPROVAL_HANDOFF: &str = "This action requires approval beyond the child's existing permissions. It was not executed. Return it to the parent agent to handle through its normal approval policy; do not retry or use a workaround.";
const SCOPE_EVENT: &str = "context.delegation-scope";

pub(super) fn freeze(
    requested: &WorkflowToolBindingV1,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let inherit = requested
        .configuration
        .get("inheritParentTools")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| invalid_tool("inheritParentTools must be boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        object.remove("inheritParentTools");
    }
    freeze_configuration(
        &configuration,
        &[
            ("authorityMode", json!("run_subagent")),
            ("requiresApproval", json!(true)),
        ],
        &[],
    )?;
    let mut schema = subagent_schema();
    if !inherit {
        schema["properties"]
            .as_object_mut()
            .unwrap()
            .remove("readOnly");
    }
    Ok((SUBAGENT_PROVIDER_NAME.into(), if inherit {
        "Delegate a task within the parent's frozen tool and permission scope; use readOnly for research."
    } else {
        "Delegate one read-only subtask to a fresh subagent context; follows the selected approval mode."
    }.into(), schema, StoredFileToolLimitV1::Subagent { inherit_parent_tools: inherit, legacy_maximum_turns: None }))
}

impl BoundFileToolAuthorityV1 {
    /// Commit only the selecting Agent's identities, once per invocation.
    /// This is independent of optional compression/workspace instructions.
    pub(super) fn register_delegation_scope(
        &self,
        outer: &StableId,
        agent: &model_tool_loop::AgentContextV1,
        request: &aworkit_capability_host::ModelToolRequestV1,
    ) -> Result<(), String> {
        if !request
            .tools
            .iter()
            .any(|t| t.capability_id == SUBAGENT_CAPABILITY_ID)
        {
            return Ok(());
        }
        let scope = json!({"outer":outer,"nodeId":agent.node_id,"toolIds":agent.tool_ids});
        let events = self.run_events.context_events_shared()?;
        if let Some(prior) = events
            .iter()
            .rev()
            .find(|e| e.kind == SCOPE_EVENT && e.payload["outer"] == outer.as_str())
        {
            if prior.payload["nodeId"] != scope["nodeId"]
                || prior.payload["toolIds"] != scope["toolIds"]
            {
                return Err("Delegating Agent's frozen tool selection changed".into());
            }
        } else {
            self.run_events.context_event(SCOPE_EVENT, scope)?;
        }
        Ok(())
    }
}

/// Explicit research mode never trusts MCP names or auto-approve overrides.
fn read_only(binding: &StoredFileToolBindingV1) -> bool {
    SUBAGENT_CHILD_TOOL_IDS.contains(&binding.capability_id.as_str())
        || (binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX)
            && mcp_approval::annotations(&binding.configuration)
                .ok()
                .flatten()
                .is_some_and(|hints| hints.permits_approval_free_call()))
}

impl FileToolDispatcherV1 {
    fn child_scope(
        &self,
        invocation: &StableId,
    ) -> Result<
        (
            FrozenFileToolAuthorityContextV1,
            model_tool_loop::AgentContextV1,
        ),
        String,
    > {
        let inherit = matches!(
            self.record.binding.limit,
            StoredFileToolLimitV1::Subagent {
                inherit_parent_tools: true,
                ..
            }
        );
        let mut context = self.context.clone();
        let agent = if inherit {
            let events = self.run_events.context_events_shared()?;
            let scope = events
                .iter()
                .rev()
                .find(|e| {
                    e.kind == SCOPE_EVENT
                        && e.payload["outer"] == self.record.outer_invocation_id.as_str()
                })
                .ok_or("Delegating Agent's frozen tool selection is unavailable")?;
            let ids: Vec<String> = serde_json::from_value(scope.payload["toolIds"].clone())
                .map_err(|e| e.to_string())?;
            let node_id = scope.payload["nodeId"]
                .as_str()
                .ok_or("Delegation node is unavailable")?
                .to_owned();
            let research = self.record.call.arguments["readOnly"] == true;
            // Preserve the parent's order and each exact frozen binding.
            context.bindings = ids
                .iter()
                .filter(|id| id.as_str() != SUBAGENT_CAPABILITY_ID)
                .map(|id| {
                    self.context
                        .bindings
                        .iter()
                        .find(|b| &b.capability_id == id)
                        .cloned()
                        .ok_or_else(|| format!("Frozen parent binding {id} is unavailable"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            context.bindings.retain(|b| !research || read_only(b));
            model_tool_loop::AgentContextV1 {
                node_id,
                tool_ids: context
                    .bindings
                    .iter()
                    .map(|b| b.capability_id.clone())
                    .collect(),
                child: Some(invocation.to_string()),
            }
        } else {
            let authority = BoundFileToolAuthorityV1 {
                runtime: self.runtime.clone(),
                context: context.clone(),
                run_events: self.run_events.clone(),
                steering_node: None,
            };
            let agent = authority.child_instruction_context(invocation)?;
            context
                .bindings
                .retain(|b| agent.tool_ids.contains(&b.capability_id));
            agent
        };
        context.node_id = stable(&agent.node_id).map_err(|e| e.to_string())?;
        context.delegation = Some(invocation.clone());
        Ok((context, agent))
    }
}

impl FileToolDispatcherV1 {
    /// Runs a subagent child loop: a fresh model/tool conversation
    /// over the same frozen gateway and selected tool authority.
    /// The child cannot delegate further (the subagent tool is
    /// excluded from its definitions and port). The child's own tool calls
    /// still settle through the durable broker; its model turns are covered by
    /// the parent pass-level settlement like any other model work.
    pub(super) fn run_subagent(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let task = self.record.call.arguments["task"]
            .as_str()
            .ok_or_else(|| "subagent task is invalid".to_owned())?;
        let context_text = self
            .record
            .call
            .arguments
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("");
        let gateway = self
            .context
            .model_gateway
            .as_ref()
            .ok_or_else(|| "subagent execution requires a frozen model gateway".to_owned())?;
        let binding_id = self
            .context
            .model_binding_id
            .as_ref()
            .ok_or_else(|| "subagent execution requires the frozen model binding".to_owned())?;
        let version_hash = self
            .context
            .model_version_hash
            .as_ref()
            .ok_or_else(|| "subagent execution requires the frozen model version".to_owned())?;
        let (context, agent) = self.child_scope(&envelope.invocation_id)?;
        let definitions = context
            .bindings
            .iter()
            .filter(|binding| binding.is_callable())
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let guidance = super::super::tool_registry::instruction_block(
            context
                .bindings
                .iter()
                .map(|binding| (binding.capability_id.as_str(), &binding.options)),
        );
        let shell_context = context
            .bindings
            .iter()
            .find(|b| b.capability_id == SHELL_CAPABILITY_ID)
            .and_then(|b| b.options.executable.as_deref())
            .map(|p| aworkit_capability_host::shell::context(Path::new(p)))
            .unwrap_or_default();
        let child_authority = SubagentToolPortV1 {
            blocked: Mutex::new(Vec::new()),
            inner: BoundFileToolAuthorityV1 {
                runtime: self.runtime.clone(),
                context,
                run_events: self.run_events.clone(),
                steering_node: None,
            },
        };
        let mut messages = vec![json!({"role":"system","content":format!(
            "You are a delegated subagent. The supplied definitions are your exact tools; they do not describe the parent agent's tools. Execute the assigned work using those tools and report observed changes and test results concisely. Your permissions are fixed at delegation. If a required operation needs approval or an unavailable tool, return the specific blocker to the parent promptly; do not retry it through another tool or emit a large ready-to-paste implementation. Background jobs belong to this Chat, but you may control only jobs you started. Resolve them before finishing.\n\n{shell_context}")})];
        if !guidance.is_empty() {
            messages.push(json!({"role":"system","content":guidance}));
        }
        messages.push(
            json!({"role":"user","content":format!("{task}\n\nRelevant context:\n{context_text}")}),
        );
        let child_input = json!({"messages":messages});
        match execute_model_tool_loop_v1(
            gateway,
            ModelToolLoopRequestV1 {
                agent_context: Some(agent),
                outer_invocation_id: &envelope.invocation_id,
                input: child_input,
                initial_context: Vec::new(),
                parameters: BTreeMap::new(),
                definitions,
                binding_id: binding_id.clone(),
                binding_version_hash: version_hash.clone(),
                maximum_input_bytes: SUBAGENT_MAXIMUM_INPUT_BYTES,
                maximum_output_bytes: SUBAGENT_MAXIMUM_OUTPUT_BYTES,
                maximum_tool_output_bytes: self.context.maximum_tool_output_bytes,
                maximum_timeout_recoveries: PROVIDER_TIMEOUT_RECOVERIES_V1,
            },
            &child_authority,
            cancellation,
        ) {
            Ok(completed) => {
                let blocked = child_authority
                    .blocked
                    .lock()
                    .map_err(|_| "child handoff lock poisoned")?;
                let value = json!({
                    "status": if blocked.is_empty() { "completed" } else { "parent_approval_required" },
                    "blockedActions": *blocked,
                    "jobs": self.runtime.jobs.list_scoped(&self.context.chat_id, Some(envelope.invocation_id.as_str()))?["jobs"],
                    "finalText": completed.assistant_text,
                    "modelTurns": completed.attempted_model_turns,
                    "toolCalls": completed.settled_tool_calls,
                    "inputTokens": completed.input_tokens,
                    "outputTokens": completed.output_tokens,
                });
                Ok((
                    value,
                    format!(
                        "Subagent {} after {} model turn(s) with {} tool call(s).",
                        if blocked.is_empty() {
                            "completed"
                        } else {
                            "returned to parent for approval"
                        },
                        completed.attempted_model_turns,
                        completed.settled_tool_calls
                    ),
                ))
            }
            Err(failure) => Err(format!("subagent failed: {}", failure.error)),
        }
    }
}

/// Uses the same broker and existing grants as the parent, but cannot open a
/// new approval or delegate again. A blocked action returns immediately.
struct SubagentToolPortV1 {
    inner: BoundFileToolAuthorityV1,
    blocked: Mutex<Vec<ModelToolCallV1>>,
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
        self.inner
            .skill_context(outer, after_exchanges, definitions, false, cancellation)
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
