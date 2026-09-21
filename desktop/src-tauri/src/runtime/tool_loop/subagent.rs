//! Frozen delegation scope, child-context lifecycle, and return to the parent.
//!
//! A child is a temporary isolated conversation created from a declared
//! projection of the delegating Agent's frozen selection. The child's full
//! transcript is never merged wholesale: it settles, its frame is committed to
//! the Run's machine-local record store, and only a declared outcome returns to
//! the parent loop. A settled child stays resumable inside the Run through the
//! owner-isolated control tools.
use super::*;
use crate::runtime::{compaction, model_tool_loop};

pub(crate) mod compatibility;
mod control;
mod fork;
mod frames;

pub(crate) use frames::{ChildKindV1, ChildStatusV1, SubagentChildFrameV1};

pub(super) const APPROVAL_HANDOFF: &str = "This action requires approval beyond the child's existing permissions. It was not executed. Return it to the parent agent to handle through its normal approval policy; do not retry or use a workaround.";
const SCOPE_EVENT: &str = "context.delegation-scope";
/// Role instructions of every child conversation. The child is always isolated
/// from the parent transcript unless a fork declares a bounded projection.
const CHILD_ROLE_INSTRUCTIONS: &str = "You are a delegated subagent. The supplied definitions are your exact tools; they do not describe the parent agent's tools. Execute the assigned work using those tools and report observed changes and test results concisely. Your permissions are fixed at delegation. If a required operation needs approval or an unavailable tool, return the specific blocker to the parent promptly; do not retry it through another tool or emit a large ready-to-paste implementation. Background jobs belong to this Chat, but you may control only jobs you started. Resolve them before finishing.";

/// Structural delegation bounds admitted before any child is spawned. Bounds
/// are frozen per Run and charged to the parent scope, never improvised at
/// dispatch time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChildAdmissionV1 {
    pub maximum_depth: u32,
    pub maximum_children: u32,
}

impl Default for ChildAdmissionV1 {
    fn default() -> Self {
        Self {
            maximum_depth: SUBAGENT_DEFAULT_MAXIMUM_DEPTH,
            maximum_children: SUBAGENT_DEFAULT_MAXIMUM_CHILDREN,
        }
    }
}

impl ChildAdmissionV1 {
    /// Exact structural bounds frozen on the selected delegation tool.
    fn for_limit(limit: &StoredFileToolLimitV1) -> Self {
        match limit {
            StoredFileToolLimitV1::Subagent {
                maximum_depth,
                maximum_children,
                ..
            }
            | StoredFileToolLimitV1::SubagentFork {
                maximum_depth,
                maximum_children,
                ..
            } => Self {
                maximum_depth: *maximum_depth,
                maximum_children: *maximum_children,
            },
            _ => Self::default(),
        }
    }
}

/// One settled child turn-set as the lifecycle sees it. A failed loop keeps its
/// exact committed prefix so recovery never re-runs an acknowledged effect.
struct ChildTurnV1 {
    status: ChildStatusV1,
    exchanges: Vec<ModelToolExchangeV1>,
    final_text: String,
    blocked: Vec<ModelToolCallV1>,
    model_turns: u32,
    tool_calls: u32,
    input_tokens: u64,
    output_tokens: u64,
    error: Option<String>,
}

/// Declared identity and inherited projection of a child about to be created.
struct ChildSpawnV1 {
    kind: ChildKindV1,
    child_id: String,
    node_id: String,
    depth: u32,
    read_only: bool,
    inherited_tool_ids: Vec<String>,
    input: Value,
    projection_hash: Option<String>,
    projection_items: usize,
    projection_dropped: usize,
}

/// Freezes `tool.subagent`: the inherited-tool delegation contract plus the
/// structural admission bounds admitted before a child is created.
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
    let maximum_depth = optional_unsigned(
        &requested.configuration,
        "maximumDepth",
        0,
        8,
        SUBAGENT_DEFAULT_MAXIMUM_DEPTH,
    )?;
    let maximum_children = optional_unsigned(
        &requested.configuration,
        "maximumChildren",
        1,
        256,
        SUBAGENT_DEFAULT_MAXIMUM_CHILDREN,
    )?;
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        for key in ["inheritParentTools", "maximumDepth", "maximumChildren"] {
            object.remove(key);
        }
    }
    freeze_configuration(
        &configuration,
        &[
            ("authorityMode", json!("run_subagent")),
            ("requiresApproval", json!(true)),
        ],
        &[],
    )?;
    let mut schema = native_tool_schema(SUBAGENT_CAPABILITY_ID);
    if !inherit {
        schema["properties"]
            .as_object_mut()
            .unwrap()
            .remove("readOnly");
    }
    Ok((
        SUBAGENT_PROVIDER_NAME.into(),
        if inherit {
            "Delegate a task within the parent's frozen tool and permission scope; use readOnly for research."
        } else {
            "Delegate one read-only subtask to a fresh subagent context; follows the selected approval mode."
        }
        .into(),
        schema,
        StoredFileToolLimitV1::Subagent {
            inherit_parent_tools: inherit,
            legacy_maximum_turns: None,
            maximum_depth,
            maximum_children,
        },
    ))
}

/// Freezes `tool.subagent_fork`: a child context created from a declared,
/// bounded projection of the parent conversation plus the same admission
/// bounds as ordinary delegation.
pub(super) fn freeze_fork(
    requested: &WorkflowToolBindingV1,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let maximum_items = optional_unsigned(
        &requested.configuration,
        "forkMaximumItems",
        1,
        256,
        SUBAGENT_DEFAULT_FORK_ITEMS,
    )?;
    let maximum_bytes = optional_unsigned(
        &requested.configuration,
        "forkMaximumBytes",
        256,
        1024 * 1024,
        SUBAGENT_DEFAULT_FORK_BYTES,
    )?;
    let maximum_depth = optional_unsigned(
        &requested.configuration,
        "maximumDepth",
        0,
        8,
        SUBAGENT_DEFAULT_MAXIMUM_DEPTH,
    )?;
    let maximum_children = optional_unsigned(
        &requested.configuration,
        "maximumChildren",
        1,
        256,
        SUBAGENT_DEFAULT_MAXIMUM_CHILDREN,
    )?;
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        for key in [
            "forkMaximumItems",
            "forkMaximumBytes",
            "maximumDepth",
            "maximumChildren",
        ] {
            object.remove(key);
        }
    }
    freeze_configuration(
        &configuration,
        &[
            ("authorityMode", json!("run_subagent")),
            ("requiresApproval", json!(true)),
        ],
        &[],
    )?;
    Ok((
        SUBAGENT_FORK_PROVIDER_NAME.into(),
        "Delegate a task to a child that inherits a bounded projection of this conversation. Use it when the child needs the established problem context instead of a standalone brief."
            .into(),
        native_tool_schema(SUBAGENT_FORK_CAPABILITY_ID),
        StoredFileToolLimitV1::SubagentFork {
            maximum_items: maximum_items as usize,
            maximum_bytes: maximum_bytes as usize,
            maximum_depth,
            maximum_children,
        },
    ))
}

/// Freezes one `tool.subagent_*` control. The operation, not the model, owns
/// the exact argument surface and whether the control itself needs approval.
pub(super) fn freeze_control(
    capability_id: &str,
    requested: &WorkflowToolBindingV1,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let (operation, provider, description, requires_approval) = match capability_id {
        SUBAGENT_LIST_CAPABILITY_ID => (
            "list",
            "list_subagents",
            "List the continuable subagents this Agent created in this Chat, with their durable ids, status and outcome. Use it to recall which children exist, not to poll them.",
            false,
        ),
        SUBAGENT_MESSAGE_CAPABILITY_ID => (
            "message",
            "message_subagent",
            "Send a follow-up message to a subagent this Agent created. The child resumes its own conversation for one more bounded turn-set and returns a new outcome. Use it to steer or extend delegated work instead of starting a new child.",
            true,
        ),
        SUBAGENT_CANCEL_CAPABILITY_ID => (
            "cancel",
            "cancel_subagent",
            "Cancel a subagent this Agent created, close its scope and stop the background jobs it still owns. Cancelling cannot undo effects the child already performed. Repeating it for the same child is a no-op.",
            false,
        ),
        _ => return Err(invalid_tool("unknown subagent control tool")),
    };
    freeze_configuration(
        &requested.configuration,
        &[
            ("authorityMode", json!("run_subagent")),
            ("requiresApproval", json!(requires_approval)),
        ],
        &[],
    )?;
    Ok((
        provider.into(),
        description.into(),
        native_tool_schema(capability_id),
        StoredFileToolLimitV1::SubagentControl {
            operation: operation.into(),
        },
    ))
}

/// Optional frozen numeric configuration with a documented default.
fn optional_unsigned(
    configuration: &Value,
    name: &str,
    minimum: u64,
    maximum: u64,
    default: u32,
) -> Result<u32, WorkflowPipelineError> {
    match configuration.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|value| (minimum..=maximum).contains(value))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| invalid_tool("tool configuration limit is invalid")),
    }
}

/// Validates one control call's exact declared argument surface.
pub(crate) fn validate_control(
    operation: &str,
    arguments: &Value,
) -> Result<(), WorkflowPipelineError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid_tool("subagent control arguments must be an object"))?;
    let (allowed, required): (&[&str], &[&str]) = match operation {
        "list" => (&[], &[]),
        "message" => (&["childId", "message"], &["childId", "message"]),
        "cancel" => (&["childId"], &["childId"]),
        _ => return Err(invalid_tool("unknown subagent control operation")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(invalid_tool("invalid subagent control argument keys"));
    }
    for (key, maximum, textual) in [("childId", 96, false), ("message", 262_144, true)] {
        if let Some(value) = object.get(key)
            && !value.as_str().is_some_and(|text| {
                !text.is_empty()
                    && text.len() <= maximum
                    && (!textual || !text.trim().is_empty())
                    && !text.contains('\0')
            })
        {
            return Err(invalid_tool("invalid subagent control text argument"));
        }
    }
    Ok(())
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
            .any(|t| is_subagent_tool(&t.capability_id))
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
    /// The delegating Agent's frozen selection for this invocation, resolved
    /// from the identity scope it committed once per invocation. This is the
    /// declared parent projection a child is created from.
    fn parent_selection(&self, inherit: bool) -> Result<(String, Vec<String>), String> {
        if !inherit {
            let authority = BoundFileToolAuthorityV1 {
                runtime: self.runtime.clone(),
                context: self.context.clone(),
                run_events: self.run_events.clone(),
                steering_node: None,
            };
            let agent = authority.child_instruction_context(&self.record.outer_invocation_id)?;
            return Ok((agent.node_id, agent.tool_ids));
        }
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
        Ok((node_id, ids))
    }

    /// Builds the isolated child authority and Agent identity from explicit
    /// inherited identities. Continuation reuses exactly the identities the
    /// child was created with, so its prompt prefix stays stable.
    fn child_scope_for(
        &self,
        child_id: &StableId,
        node_id: &str,
        tool_ids: &[String],
        read_only_child: bool,
    ) -> Result<
        (
            FrozenFileToolAuthorityContextV1,
            model_tool_loop::AgentContextV1,
        ),
        String,
    > {
        let mut context = self.context.clone();
        // Preserve the parent's order and each exact frozen binding, but never
        // hand a child a delegation or control tool.
        context.bindings = tool_ids
            .iter()
            .filter(|id| !is_subagent_tool(id))
            .map(|id| {
                self.context
                    .bindings
                    .iter()
                    .find(|b| &b.capability_id == id)
                    .cloned()
                    .ok_or_else(|| format!("Frozen parent binding {id} is unavailable"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        context.bindings.retain(|b| !read_only_child || read_only(b));
        let agent = model_tool_loop::AgentContextV1 {
            node_id: node_id.to_owned(),
            tool_ids: context
                .bindings
                .iter()
                .map(|b| b.capability_id.clone())
                .collect(),
            child: Some(child_id.to_string()),
        };
        context.node_id = stable(&agent.node_id).map_err(|e| e.to_string())?;
        context.delegation = Some(child_id.clone());
        Ok((context, agent))
    }

    /// Runs one child turn-set over the same frozen gateway and selected tool
    /// authority. The child cannot delegate further: every delegation and
    /// control tool is excluded from its definitions and port.
    fn run_child_turns(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        context: FrozenFileToolAuthorityContextV1,
        agent: model_tool_loop::AgentContextV1,
        input: Value,
        initial_exchanges: Vec<ModelToolExchangeV1>,
        continuation: Option<String>,
        cancellation: &CancellationToken,
    ) -> Result<ChildTurnV1, String> {
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
        let definitions = context
            .bindings
            .iter()
            .filter(|binding| binding.is_callable())
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let child_authority = SubagentToolPortV1 {
            blocked: Mutex::new(Vec::new()),
            inner: BoundFileToolAuthorityV1 {
                runtime: self.runtime.clone(),
                context,
                run_events: self.run_events.clone(),
                steering_node: None,
            },
        };
        let initial_context = continuation
            .map(|message| {
                vec![ModelToolContextV1 {
                    after_exchanges: initial_exchanges.len(),
                    after_input_messages: None,
                    instruction_event_id: None,
                    content: message,
                    role: None,
                    images: Vec::new(),
                }]
            })
            .unwrap_or_default();
        match execute_model_tool_loop_v1(
            gateway,
            model_tool_loop::ModelToolLoopRequestV1 {
                agent_context: Some(agent),
                outer_invocation_id: &envelope.invocation_id,
                input,
                initial_context,
                initial_exchanges,
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
            Err(failure) => Ok(ChildTurnV1 {
                status: ChildStatusV1::Failed,
                exchanges: failure.exchanges,
                final_text: failure.error.to_string(),
                blocked: Vec::new(),
                model_turns: failure.attempted_model_turns,
                tool_calls: failure.settled_tool_calls,
                input_tokens: failure.input_tokens,
                output_tokens: failure.output_tokens,
                error: Some(format!("subagent failed: {}", failure.error)),
            }),
        }
    }

    /// Performs the deterministic admission every child must pass before its
    /// context exists: inherited depth and the parent scope's fan-out ledger.
    fn admit_child(
        &self,
        admission: ChildAdmissionV1,
        depth: u32,
        children: &[SubagentChildFrameV1],
    ) -> Result<(), String> {
        if depth > admission.maximum_depth {
            return Err(format!(
                "delegation depth {depth} exceeds the frozen maximum of {}",
                admission.maximum_depth
            ));
        }
        let live = children
            .iter()
            .filter(|frame| frame.status != ChildStatusV1::Cancelled)
            .count();
        if live >= admission.maximum_children as usize {
            return Err(format!(
                "delegation fan-out {live} exceeds the frozen maximum of {}",
                admission.maximum_children
            ));
        }
        Ok(())
    }

    /// Runs a fresh or forked child and commits its first frame revision.
    fn spawn_child(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        spawn: ChildSpawnV1,
        admission: ChildAdmissionV1,
        cancellation: &CancellationToken,
    ) -> Result<SubagentChildFrameV1, String> {
        let run_id = self.record.proposal.run_id.clone();
        let children = self
            .runtime
            .records
            .subagent_children(&run_id)
            .map_err(|error| error.to_string())?;
        self.admit_child(admission, spawn.depth, &children)?;
        let child_id = stable(&spawn.child_id).map_err(|e| e.to_string())?;
        let (context, agent) = self.child_scope_for(
            &child_id,
            &spawn.node_id,
            &spawn.inherited_tool_ids,
            spawn.read_only,
        )?;
        let task = self.record.call.arguments["task"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let context_text = self
            .record
            .call
            .arguments
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let base_input = spawn.input.clone();
        let turn = self.run_child_turns(
            envelope,
            context,
            agent,
            spawn.input,
            Vec::new(),
            None,
            cancellation,
        )?;
        let error = turn.error.clone();
        let now = crate::runtime::history::now_label();
        let frame = SubagentChildFrameV1 {
            child_id: spawn.child_id,
            chat_id: self.context.chat_id.clone(),
            run_id: run_id.to_string(),
            node_id: spawn.node_id,
            parent_invocation_id: self.record.outer_invocation_id.to_string(),
            parent_child_id: None,
            depth: spawn.depth,
            kind: spawn.kind,
            projection_hash: spawn.projection_hash,
            projection_items: spawn.projection_items,
            projection_dropped: spawn.projection_dropped,
            inherited_tool_ids: spawn.inherited_tool_ids,
            read_only: spawn.read_only,
            head_revision: 0,
            status: turn.status,
            task,
            context_text,
            input: base_input,
            exchanges: Vec::new(),
            final_text: String::new(),
            blocked_actions: Vec::new(),
            model_turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            created_at: now.clone(),
            updated_at: now,
        };
        let frame = apply_turn(frame, turn);
        self.persist_child(&frame)?;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(frame)
    }

    /// Commits one immutable frame revision.
    fn persist_child(&self, frame: &SubagentChildFrameV1) -> Result<(), String> {
        self.runtime
            .records
            .record_subagent_child(frame)
            .map_err(|error| error.to_string())
    }

    /// The model-facing job inventory one child owns.
    fn child_jobs(&self, child_id: &str) -> Result<Value, String> {
        Ok(self
            .runtime
            .jobs
            .list_scoped(&self.context.chat_id, Some(child_id))?["jobs"]
            .clone())
    }

    /// Runs `tool.subagent` within the inherited-tool contract.
    pub(super) fn run_subagent(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let inherit = matches!(
            self.record.binding.limit,
            StoredFileToolLimitV1::Subagent {
                inherit_parent_tools: true,
                ..
            }
        );
        let admission = ChildAdmissionV1::for_limit(&self.record.binding.limit);
        let (node_id, tool_ids) = self.parent_selection(inherit)?;
        let child = self.spawn_child(
            envelope,
            self.child_spawn_spec(ChildKindV1::Fresh, node_id, tool_ids, None)?,
            admission,
            cancellation,
        )?;
        Ok((self.child_result(&child)?, self.child_summary(&child)))
    }

    /// Runs `tool.subagent_fork`: the child inherits a declared, bounded
    /// projection of the delegating Agent's committed conversation.
    pub(super) fn run_subagent_fork(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let (maximum_items, maximum_bytes) = match &self.record.binding.limit {
            StoredFileToolLimitV1::SubagentFork {
                maximum_items,
                maximum_bytes,
                ..
            } => (*maximum_items, *maximum_bytes),
            _ => return Err("fork delegation requires the frozen fork contract".into()),
        };
        let conversation = self
            .run_events
            .parent_conversation()
            .ok_or("fork delegation requires the delegating Agent's conversation")?;
        let projection = fork::project(&conversation, maximum_items, maximum_bytes)?;
        let admission = ChildAdmissionV1::for_limit(&self.record.binding.limit);
        let (node_id, tool_ids) = self.parent_selection(true)?;
        let child = self.spawn_child(
            envelope,
            self.child_spawn_spec(ChildKindV1::Fork, node_id, tool_ids, Some(projection))?,
            admission,
            cancellation,
        )?;
        Ok((self.child_result(&child)?, self.child_summary(&child)))
    }

    /// Builds the declared spawn identity and immutable prompt prefix.
    fn child_spawn_spec(
        &self,
        kind: ChildKindV1,
        node_id: String,
        tool_ids: Vec<String>,
        projection: Option<fork::ForkProjectionV1>,
    ) -> Result<ChildSpawnV1, String> {
        let child_id = digest_id(
            "child.subagent",
            &format!(
                "{}:{}:{}",
                self.context.chat_id,
                self.record.outer_invocation_id.as_str(),
                self.record.call.call_id
            ),
        )
        .map_err(|error| error.to_string())?
        .to_string();
        let read_only_child = self.record.call.arguments["readOnly"] == true;
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
        // Guidance and shell facts describe exactly the child's inherited
        // selection, not the Run's whole binding catalog.
        let child_bindings: Vec<&StoredFileToolBindingV1> = tool_ids
            .iter()
            .filter(|id| !is_subagent_tool(id))
            .filter_map(|id| {
                self.context
                    .bindings
                    .iter()
                    .find(|binding| &binding.capability_id == id)
            })
            .collect();
        let shell_context = child_bindings
            .iter()
            .find(|b| b.capability_id == SHELL_CAPABILITY_ID)
            .and_then(|b| b.options.executable.as_deref())
            .map(|p| aworkit_capability_host::shell::context(Path::new(p)))
            .unwrap_or_default();
        let guidance = super::super::tool_registry::instruction_block(
            child_bindings
                .iter()
                .map(|binding| (binding.capability_id.as_str(), &binding.options)),
        );
        let (input, projection_hash, projection_items, projection_dropped) = match projection {
            Some(projection) => (
                fork_child_input(task, context_text, &shell_context, &guidance, &projection),
                Some(projection.hash),
                projection.items,
                projection.dropped,
            ),
            None => (
                fresh_child_input(task, context_text, &shell_context, &guidance),
                None,
                0,
                0,
            ),
        };
        Ok(ChildSpawnV1 {
            kind,
            child_id,
            node_id,
            depth: 1,
            read_only: read_only_child,
            inherited_tool_ids: tool_ids,
            input,
            projection_hash,
            projection_items,
            projection_dropped,
        })
    }

    /// A model-facing child outcome carrying the child's own job inventory.
    fn child_result(&self, frame: &SubagentChildFrameV1) -> Result<Value, String> {
        Ok(frame.outcome(self.child_jobs(&frame.child_id)?))
    }

    fn child_summary(&self, frame: &SubagentChildFrameV1) -> String {
        format!(
            "Subagent {} after {} model turn(s) with {} tool call(s) (child {}).",
            frame.status.as_str(),
            frame.model_turns,
            frame.tool_calls,
            frame.child_id
        )
    }

    /// Executes one `tool.subagent_*` control under owner isolation.
    pub(super) fn run_subagent_control(
        &self,
        operation: &str,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        control::execute(self, operation, envelope, cancellation)
    }
}

/// Immutable prompt prefix of a fresh child conversation.
fn fresh_child_input(
    task: &str,
    context_text: &str,
    shell_context: &str,
    guidance: &str,
) -> Value {
    child_messages(
        format!("{task}\n\nRelevant context:\n{context_text}"),
        shell_context,
        guidance,
    )
}

/// Immutable prompt prefix of a forked child conversation. The inherited
/// transcript is the declared projection, never the parent's live state.
fn fork_child_input(
    task: &str,
    context_text: &str,
    shell_context: &str,
    guidance: &str,
    projection: &fork::ForkProjectionV1,
) -> Value {
    let inherited = if projection.transcript.trim().is_empty() {
        "The parent conversation prefix declared for this fork was empty.".to_owned()
    } else {
        format!(
            "Inherited parent conversation ({} item(s) kept, {} older item(s) omitted):\n\n{}",
            projection.items, projection.dropped, projection.transcript
        )
    };
    child_messages(
        format!("{inherited}\n\nAssigned fork task:\n{task}\n\nAdditional context:\n{context_text}"),
        shell_context,
        guidance,
    )
}

/// Shared message shape of a child conversation: the fixed role instructions,
/// the child's own tool guidance and one user turn carrying the assignment.
fn child_messages(assignment: String, shell_context: &str, guidance: &str) -> Value {
    let mut messages = vec![json!({
        "role":"system",
        "content":format!("{CHILD_ROLE_INSTRUCTIONS}\n\n{shell_context}")
    })];
    if !guidance.trim().is_empty() {
        messages.push(json!({"role":"system","content":guidance}));
    }
    messages.push(json!({"role":"user","content":assignment}));
    json!({"messages": messages})
}

/// Applies one settled child turn-set to a frame, advancing its revision.
fn apply_turn(mut frame: SubagentChildFrameV1, turn: ChildTurnV1) -> SubagentChildFrameV1 {
    frame.head_revision = frame.head_revision.saturating_add(1);
    frame.status = turn.status;
    frame.exchanges = turn.exchanges;
    frame.final_text = turn.final_text;
    frame.blocked_actions = turn.blocked;
    frame.model_turns = frame.model_turns.saturating_add(turn.model_turns);
    frame.tool_calls = frame.tool_calls.saturating_add(turn.tool_calls);
    frame.input_tokens = frame.input_tokens.saturating_add(turn.input_tokens);
    frame.output_tokens = frame.output_tokens.saturating_add(turn.output_tokens);
    frame.updated_at = crate::runtime::history::now_label();
    frame
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
