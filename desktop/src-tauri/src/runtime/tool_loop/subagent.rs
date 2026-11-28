//! Frozen delegation scope, child-context lifecycle, and return to the parent.
//!
//! A child is a temporary isolated conversation created from a declared
//! projection of the delegating Agent's frozen selection. A delegation is a
//! job: by default it runs on its own thread as a background job the parent can
//! observe with `job_output`/`job_list`, steer with `job_input` and cancel with
//! `job_stop`, or run inline when the frozen contract or the call asks for it.
//! The child's full transcript is never merged wholesale: it settles, its frame
//! is committed to the Run's operational record store, and only a declared
//! outcome returns to the parent loop.
use super::jobs::{ChildJobHandle, job_owner_scope};
use super::*;
use crate::runtime::model_tool_loop;

pub(crate) mod compatibility;
mod control;
mod external;
mod fork;
mod frames;
mod worker;

pub(crate) use frames::{ChildKindV1, ChildStatusV1, SubagentChildFrameV1};
use worker::{ChildTurnRequestV1, ChildTurnV1, execute_child_turns};

pub(super) const APPROVAL_HANDOFF: &str = "This action requires approval beyond the child's existing permissions. It was not executed. Return it to the parent agent to handle through its normal approval policy; do not retry or use a workaround.";
const SCOPE_EVENT: &str = "context.delegation-scope";
/// Bounded child lifecycle fact the desktop folds into its subagent catalog.
///
/// It carries identity, status and counters only. The child's conversation and
/// evidence stay operational and are never merged into the semantic transcript,
/// so this is deliberately a `context.*` fact rather than a timeline activity.
pub(crate) const CHILD_FACT: &str = "context.subagent-child";
/// One terminal child status delivered to the delegating Agent.
///
/// The delivery marker is committed before dispatch, so a restart re-delivers the
/// report the model had not seen yet instead of replaying the child.
const CHILD_REPORT: &str = "context.subagent-report";
/// How long a live child may produce no event before it is reported as a stall.
/// A delegated child cannot hold a Run open while its delegator hears nothing.
pub(super) const SUBAGENT_STALL_SECONDS: u64 = 300;
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

/// One child scope fully prepared to run, owning everything a thread needs.
struct PreparedChildV1 {
    frame: SubagentChildFrameV1,
    request: ChildTurnRequestV1,
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
    let background = optional_boolean(
        &requested.configuration,
        "runInBackground",
        false,
    )?;
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        for key in [
            "inheritParentTools",
            "maximumDepth",
            "maximumChildren",
            "runInBackground",
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
            background,
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
    let background = optional_boolean(&requested.configuration, "runInBackground", false)?;
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        for key in [
            "forkMaximumItems",
            "forkMaximumBytes",
            "maximumDepth",
            "maximumChildren",
            "runInBackground",
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
            background,
        },
    ))
}

/// Freezes one `tool.subagent_*` control. The operation, not the model, owns
/// the exact argument surface and whether the control itself needs approval.
pub(super) fn freeze_control(
    capability_id: &str,
    requested: &WorkflowToolBindingV1,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let (operation, provider, description, requires_approval, background) = match capability_id {
        SUBAGENT_LIST_CAPABILITY_ID => (
            "list",
            "list_subagents",
            "List the subagents this Agent created in this Chat, with their durable ids, live status and outcome. Use it to recall which children exist and whether one is still running.",
            false,
            false,
        ),
        SUBAGENT_MESSAGE_CAPABILITY_ID => (
            "message",
            "message_subagent",
            "Send a follow-up message to a subagent this Agent created. A running child is steered at its next step boundary; a settled child resumes its own conversation for one more bounded turn-set. The call returns a job id you observe with job_output.",
            true,
            optional_boolean(&requested.configuration, "runInBackground", false)?,
        ),
        SUBAGENT_CANCEL_CAPABILITY_ID => (
            "cancel",
            "cancel_subagent",
            "Cancel a subagent this Agent created, close its scope and stop the background jobs it still owns. Cancelling cannot undo effects the child already performed. Repeating it for the same child is a no-op.",
            false,
            false,
        ),
        _ => return Err(invalid_tool("unknown subagent control tool")),
    };
    let mut configuration = requested.configuration.clone();
    if let Some(object) = configuration.as_object_mut() {
        object.remove("runInBackground");
    }
    freeze_configuration(
        &configuration,
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
            background,
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

/// Optional frozen boolean configuration with a documented default.
fn optional_boolean(
    configuration: &Value,
    name: &str,
    default: bool,
) -> Result<bool, WorkflowPipelineError> {
    match configuration.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid_tool("tool configuration boolean is invalid")),
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
        "message" => (
            &["childId", "message", "runInBackground"],
            &["childId", "message"],
        ),
        "cancel" => (&["childId"], &["childId"]),
        _ => return Err(invalid_tool("unknown subagent control operation")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(invalid_tool("invalid subagent control argument keys"));
    }
    if object
        .get("runInBackground")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid_tool("runInBackground must be boolean"));
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

/// A frame whose live job is gone is reported as interrupted, so a restart never
/// leaves a child that claims to be running.
///
/// Only a job-owned child can be judged by its job: a fresh or forked child runs
/// as a parent-owned job, while a one-shot external delegation runs inside the
/// invocation that made it and owns no job at all. Judging that one by jobs would
/// report every live delegation as interrupted the moment it starts, so its own
/// durable frame is authoritative.
pub(super) fn effective_status(
    frame: &SubagentChildFrameV1,
    jobs: &jobs::JobRegistry,
    chat_id: &str,
) -> ChildStatusV1 {
    let job_owned = matches!(frame.kind, ChildKindV1::Fresh | ChildKindV1::Fork);
    if job_owned
        && frame.status == ChildStatusV1::Running
        && !jobs.child_running(chat_id, &frame.child_id)
    {
        ChildStatusV1::Interrupted
    } else {
        frame.status
    }
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
            .filter(|id| !is_owner_only_tool(id))
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
            // A delegated child keeps the frozen Chat compaction policy.
            compaction: None,
        };
        context.node_id = stable(&agent.node_id).map_err(|e| e.to_string())?;
        context.delegation = Some(child_id.clone());
        Ok((context, agent))
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

    /// Whether a delegation should run as a background job. The frozen
    /// contract decides the default; one call may override it.
    fn background(&self, limit: &StoredFileToolLimitV1) -> bool {
        let frozen = match limit {
            StoredFileToolLimitV1::Subagent { background, .. }
            | StoredFileToolLimitV1::SubagentFork { background, .. }
            | StoredFileToolLimitV1::SubagentControl { background, .. } => *background,
            _ => false,
        };
        self.record
            .call
            .arguments
            .get("runInBackground")
            .and_then(Value::as_bool)
            .unwrap_or(frozen)
    }

    /// Builds the child frame and the owned turn request without running it.
    fn prepare_child(
        &self,
        spawn: ChildSpawnV1,
        admission: ChildAdmissionV1,
    ) -> Result<PreparedChildV1, String> {
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
        let gateway = self
            .context
            .model_gateway
            .clone()
            .ok_or_else(|| "subagent execution requires a frozen model gateway".to_owned())?;
        let binding_id = self
            .context
            .model_binding_id
            .clone()
            .ok_or_else(|| "subagent execution requires the frozen model binding".to_owned())?;
        let binding_version_hash = self
            .context
            .model_version_hash
            .clone()
            .ok_or_else(|| "subagent execution requires the frozen model version".to_owned())?;
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
        let now = crate::runtime::history::now_label();
        let frame = SubagentChildFrameV1 {
            child_id: spawn.child_id,
            chat_id: self.context.chat_id.clone(),
            run_id: run_id.to_string(),
            node_id: spawn.node_id,
            parent_invocation_id: self.record.outer_invocation_id.to_string(),
            parent_call_id: self.record.call.call_id.clone(),
            parent_child_id: None,
            depth: spawn.depth,
            kind: spawn.kind,
            projection_hash: spawn.projection_hash,
            projection_items: spawn.projection_items,
            projection_dropped: spawn.projection_dropped,
            inherited_tool_ids: spawn.inherited_tool_ids,
            read_only: spawn.read_only,
            head_revision: 0,
            status: ChildStatusV1::Running,
            task,
            context_text,
            input: spawn.input.clone(),
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
        let request = ChildTurnRequestV1 {
            runtime: self.runtime.clone(),
            context,
            agent,
            run_events: self.run_events.clone(),
            gateway,
            binding_id,
            binding_version_hash,
            maximum_tool_output_bytes: self.context.maximum_tool_output_bytes,
            input: spawn.input,
            initial_exchanges: Vec::new(),
            continuation: None,
            job: None,
        };
        Ok(PreparedChildV1 { frame, request })
    }

    /// Runs a fresh or forked child inline and commits its settled frame.
    ///
    /// The child still owns its evidence root: an inline child commits into the
    /// same Run history through its own detached stream, so its activity can be
    /// attributed to its child identity exactly like a background child's.
    fn run_child_now(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        spawn: ChildSpawnV1,
        admission: ChildAdmissionV1,
        cancellation: &CancellationToken,
    ) -> Result<SubagentChildFrameV1, String> {
        let mut prepared = self.prepare_child(spawn, admission)?;
        prepared.request.run_events = self.run_events.detached_child(
            format!(
                "{}.child.{}",
                envelope.invocation_id.as_str(),
                prepared.frame.child_id
            ),
            prepared.frame.child_id.clone(),
        );
        let turn = execute_child_turns(prepared.request, &envelope.invocation_id, cancellation)?;
        let error = turn.error.clone();
        let frame = apply_turn(prepared.frame, turn);
        self.persist_child(&frame)?;
        publish_child_fact(&self.run_events, &frame, frame.status)?;
        // This terminal status returns to the model as the tool result itself, so
        // the pending-report scan must not file it a second time.
        let _ = mark_child_reported(&self.run_events, &frame, frame.status);
        if let Some(error) = error {
            return Err(error);
        }
        Ok(frame)
    }

    /// Starts a fresh or forked child as a background job and returns its
    /// identity immediately. The child frame is committed before the job so a
    /// restart can report it interrupted rather than claiming it is running.
    fn start_child_job(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        spawn: ChildSpawnV1,
        admission: ChildAdmissionV1,
    ) -> Result<Value, String> {
        let prepared = self.prepare_child(spawn, admission)?;
        self.persist_child(&prepared.frame)?;
        publish_child_fact(&self.run_events, &prepared.frame, ChildStatusV1::Running)?;
        let child_id = prepared.frame.child_id.clone();
        let job_child_id = child_id.clone();
        let chat_id = prepared.frame.chat_id.clone();
        let run_id = prepared.frame.run_id.clone();
        let kind = prepared.frame.kind;
        let frame_base = prepared.frame;
        let mut request = prepared.request;
        // The child owns its evidence root so its activity can outlive this pass.
        request.run_events = self.run_events.detached_child(
            format!("{}.child.{}", envelope.invocation_id.as_str(), child_id),
            child_id.clone(),
        );
        let runtime = request.runtime.clone();
        let fact_stream = self.run_events.clone();
        let outer = envelope.invocation_id.clone();
        let job_id = self.runtime.jobs.clone().start_child_scoped(
            &self.context.chat_id,
            envelope.invocation_id.as_str(),
            Some(job_owner_scope(
                self.context.delegation.as_ref(),
                self.record.outer_invocation_id.as_str(),
            )),
            &child_id,
            self.context.cancellation.clone(),
            move |handle: Arc<ChildJobHandle>, child_token| {
                request.job = Some(handle);
                let turn = execute_child_turns(request, &outer, &child_token)?;
                let error = turn.error.clone();
                let frame = apply_turn(frame_base, turn);
                runtime
                    .records
                    .record_subagent_child(&frame)
                    .map_err(|problem| problem.to_string())?;
                let _ = publish_child_fact(&fact_stream, &frame, frame.status);
                if let Some(error) = error {
                    return Err(error);
                }
                let jobs = runtime
                    .jobs
                    .list_scoped(&chat_id, Some(&job_child_id))
                    .map_err(|problem| problem.to_string())?;
                let _ = &run_id;
                Ok(frame.outcome(jobs["jobs"].clone()))
            },
        )?;
        Ok(json!({
            "childId": child_id,
            "jobId": job_id,
            "kind": kind,
            "status": "running",
            "running": true,
        }))
    }

    /// Starts a settled child's continuation as a background job.
    pub(super) fn start_continuation_job(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        frame: SubagentChildFrameV1,
        message: String,
    ) -> Result<Value, String> {
        let child_id = frame.child_id.clone();
        let job_child_id = child_id.clone();
        let chat_id = frame.chat_id.clone();
        let mut request = self.child_continuation_request(&frame, Some(message))?;
        request.run_events = self.run_events.detached_child(
            format!("{}.child.{}", envelope.invocation_id.as_str(), child_id),
            child_id.clone(),
        );
        let runtime = request.runtime.clone();
        let fact_stream = self.run_events.clone();
        publish_child_fact(&fact_stream, &frame, ChildStatusV1::Running)?;
        let outer = envelope.invocation_id.clone();
        let job_id = self.runtime.jobs.clone().start_child_scoped(
            &self.context.chat_id,
            envelope.invocation_id.as_str(),
            Some(job_owner_scope(
                self.context.delegation.as_ref(),
                self.record.outer_invocation_id.as_str(),
            )),
            &child_id,
            self.context.cancellation.clone(),
            move |handle: Arc<ChildJobHandle>, child_token| {
                let mut request = request;
                request.job = Some(handle);
                let turn = execute_child_turns(request, &outer, &child_token)?;
                let error = turn.error.clone();
                let frame = apply_turn(frame, turn);
                runtime
                    .records
                    .record_subagent_child(&frame)
                    .map_err(|problem| problem.to_string())?;
                let _ = publish_child_fact(&fact_stream, &frame, frame.status);
                if let Some(error) = error {
                    return Err(error);
                }
                let jobs = runtime
                    .jobs
                    .list_scoped(&chat_id, Some(&job_child_id))
                    .map_err(|problem| problem.to_string())?;
                Ok(frame.outcome(jobs["jobs"].clone()))
            },
        )?;
        Ok(json!({
            "childId": child_id,
            "jobId": job_id,
            "kind": "fresh",
            "status": "running",
            "running": true,
        }))
    }

    /// Resumes a settled child inline, returning its new outcome.
    pub(super) fn run_continuation_now(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        frame: SubagentChildFrameV1,
        message: String,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let child_id = frame.child_id.clone();
        let mut request = self.child_continuation_request(&frame, Some(message))?;
        request.run_events = self.run_events.detached_child(
            format!("{}.child.{}", envelope.invocation_id.as_str(), child_id),
            child_id,
        );
        let turn = execute_child_turns(request, &envelope.invocation_id, cancellation)?;
        let error = turn.error.clone();
        let updated = apply_turn(frame, turn);
        self.persist_child(&updated)?;
        publish_child_fact(&self.run_events, &updated, updated.status)?;
        let _ = mark_child_reported(&self.run_events, &updated, updated.status);
        if let Some(error) = error {
            return Err(error);
        }
        Ok((self.child_result(&updated)?, self.child_summary(&updated)))
    }

    /// Owned request for one continuation turn-set of an existing child.
    fn child_continuation_request(
        &self,
        frame: &SubagentChildFrameV1,
        message: Option<String>,
    ) -> Result<ChildTurnRequestV1, String> {
        let child_id = stable(&frame.child_id).map_err(|e| e.to_string())?;
        let (context, agent) = self.child_scope_for(
            &child_id,
            &frame.node_id,
            &frame.inherited_tool_ids,
            frame.read_only,
        )?;
        let gateway = self
            .context
            .model_gateway
            .clone()
            .ok_or_else(|| "subagent execution requires a frozen model gateway".to_owned())?;
        let binding_id = self
            .context
            .model_binding_id
            .clone()
            .ok_or_else(|| "subagent execution requires the frozen model binding".to_owned())?;
        let binding_version_hash = self
            .context
            .model_version_hash
            .clone()
            .ok_or_else(|| "subagent execution requires the frozen model version".to_owned())?;
        Ok(ChildTurnRequestV1 {
            runtime: self.runtime.clone(),
            context,
            agent,
            run_events: self.run_events.clone(),
            gateway,
            binding_id,
            binding_version_hash,
            maximum_tool_output_bytes: self.context.maximum_tool_output_bytes,
            input: frame.input.clone(),
            initial_exchanges: frame.exchanges.clone(),
            continuation: message,
            job: None,
        })
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
        let background = self.background(&self.record.binding.limit);
        let (node_id, tool_ids) = self.parent_selection(inherit)?;
        let spawn = self.child_spawn_spec(ChildKindV1::Fresh, node_id, tool_ids, None)?;
        let spawned_child = spawn.child_id.clone();
        if background {
            let value = self.start_child_job(envelope, spawn, admission)?;
            return Ok((
                value,
                format!("Subagent {spawned_child} runs in the background; observe it with job_output or job_list."),
            ));
        }
        let frame = self.run_child_now(envelope, spawn, admission, cancellation)?;
        Ok((self.child_result(&frame)?, self.child_summary(&frame)))
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
        let background = self.background(&self.record.binding.limit);
        let (node_id, tool_ids) = self.parent_selection(true)?;
        let spawn =
            self.child_spawn_spec(ChildKindV1::Fork, node_id, tool_ids, Some(projection))?;
        let spawned_child = spawn.child_id.clone();
        if background {
            let value = self.start_child_job(envelope, spawn, admission)?;
            return Ok((
                value,
                format!("Forked subagent {spawned_child} runs in the background; observe it with job_output or job_list."),
            ));
        }
        let frame = self.run_child_now(envelope, spawn, admission, cancellation)?;
        Ok((self.child_result(&frame)?, self.child_summary(&frame)))
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
            .filter(|id| !is_owner_only_tool(id))
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
        background: bool,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        control::execute(self, operation, background, envelope, cancellation)
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

/// Files one terminal child status with the delegating Agent.
///
/// A delegation is a tool call, so its outcome is a tool result: every terminal
/// child status - completed, failed, cancelled, or interrupted by a restart - is
/// delivered once as a bounded report. The delivery marker is committed first, so
/// a restart re-delivers the report instead of replaying the child. A live child
/// that stopped producing events is reported as a stall with its last committed
/// sequence and the age of that sequence, so silence is never read as progress.
impl BoundFileToolAuthorityV1 {
    pub(super) fn pending_child_reports(&self) -> Result<Option<String>, String> {
        let frames = self
            .runtime
            .records
            .subagent_children(&self.context.run_id)
            .map_err(|error| error.to_string())?;
        let delivered = reported_child_keys(&self.run_events)?;
        let plan = plan_child_reports(
            frames
                .iter()
                .filter(|frame| frame.chat_id == self.context.chat_id),
            &delivered,
            |frame| {
                (
                    effective_status(frame, &self.runtime.jobs, &self.context.chat_id),
                    self.runtime
                        .jobs
                        .child_live(&self.context.chat_id, &frame.child_id),
                )
            },
        );
        if plan.is_empty() {
            return Ok(None);
        }
        // Durable before dispatch: a restart re-delivers this report rather than
        // replaying the child or losing its outcome.
        for (key, child_id) in plan.markers {
            self.run_events.context_event(
                CHILD_REPORT,
                json!({
                    "key": key,
                    "childId": child_id,
                    "chatId": self.context.chat_id,
                }),
            )?;
        }
        Ok(Some(render_child_reports(&plan.settled, &plan.stalled)))
    }

    /// The pass that owns these children is terminating: cancel every live child
    /// it created and commit its terminal status in the same pass. Best effort by
    /// design - the durable frame is what a restart reads.
    pub(super) fn cancel_live_children(&self) {
        let Ok(frames) = self.runtime.records.subagent_children(&self.context.run_id) else {
            return;
        };
        let node = self.context.node_id.as_str();
        for frame in frames.iter().filter(|frame| {
            frame.chat_id == self.context.chat_id
                && frame.node_id == node
                && effective_status(frame, &self.runtime.jobs, &self.context.chat_id)
                    == ChildStatusV1::Running
        }) {
            self.runtime
                .jobs
                .stop_unkept_scoped(&self.context.chat_id, Some(&frame.child_id));
            let updated = cancelled_frame(&self.runtime, frame);
            let _ = self
                .runtime
                .records
                .record_subagent_child(&updated);
            let _ = publish_child_fact(&self.run_events, &updated, ChildStatusV1::Cancelled);
        }
    }
}

/// Delivery keys already committed for this Run's children.
fn reported_child_keys(
    run_events: &RunEventStream,
) -> Result<std::collections::BTreeSet<String>, String> {
    let events = run_events.context_events_shared()?;
    Ok(events
        .iter()
        .filter(|event| event.kind == CHILD_REPORT)
        .filter_map(|event| event.payload["key"].as_str().map(str::to_owned))
        .collect())
}

fn child_report_key(child_id: &str, sequence: u64, status: &str) -> String {
    format!("{child_id}:{sequence}:{status}")
}

/// The child statuses this pass still has to tell its delegating Agent about.
#[derive(Default)]
pub(super) struct ChildReportPlanV1 {
    pub settled: Vec<String>,
    pub stalled: Vec<String>,
    /// Delivery keys committed before dispatch, with the child they belong to.
    pub markers: Vec<(String, String)>,
}

impl ChildReportPlanV1 {
    pub(super) fn is_empty(&self) -> bool {
        self.settled.is_empty() && self.stalled.is_empty()
    }
}

/// Decide which child statuses are due, from durable frames, live counters and
/// the keys already delivered.
///
/// A terminal frame is reported once per revision and status, so re-entering the
/// loop does not repeat a status the model already saw, while a resumed child
/// (a new revision) reports again. A live child whose last committed sequence is
/// older than [`SUBAGENT_STALL_SECONDS`] is a stall, reported once per sequence.
pub(super) fn plan_child_reports<'a>(
    frames: impl Iterator<Item = &'a SubagentChildFrameV1>,
    delivered: &std::collections::BTreeSet<String>,
    observe: impl Fn(&SubagentChildFrameV1) -> (ChildStatusV1, Option<jobs::ChildLiveV1>),
) -> ChildReportPlanV1 {
    let mut plan = ChildReportPlanV1::default();
    for frame in frames {
        let (status, live) = observe(frame);
        if status == ChildStatusV1::Running {
            let Some(live) = live else {
                continue;
            };
            let sequence = frame.head_revision.max(live.last_sequence);
            let key = child_report_key(&frame.child_id, sequence, "stall");
            if live.age_seconds >= SUBAGENT_STALL_SECONDS && !delivered.contains(&key) {
                plan.stalled.push(stall_line(frame, sequence, live.age_seconds));
                plan.markers.push((key, frame.child_id.clone()));
            }
            continue;
        }
        let key = child_report_key(&frame.child_id, frame.head_revision, status.as_str());
        if delivered.contains(&key) {
            continue;
        }
        plan.settled.push(settlement_line(frame, status));
        plan.markers.push((key, frame.child_id.clone()));
    }
    plan
}

/// The frame a cancelling parent commits: the real counters the child reported,
/// never the zeros of the frame it started from.
pub(super) fn cancelled_frame(
    runtime: &FileToolAuthorityRuntimeV1,
    frame: &SubagentChildFrameV1,
) -> SubagentChildFrameV1 {
    let live = runtime.jobs.child_live(&frame.chat_id, &frame.child_id);
    SubagentChildFrameV1 {
        head_revision: frame.head_revision.saturating_add(1),
        status: ChildStatusV1::Cancelled,
        final_text: if frame.final_text.trim().is_empty() {
            "Subagent cancelled by its parent before it settled.".to_owned()
        } else {
            frame.final_text.clone()
        },
        model_turns: frame.model_turns.max(live.map_or(0, |live| live.model_turns)),
        input_tokens: frame
            .input_tokens
            .max(live.map_or(0, |live| live.input_tokens)),
        output_tokens: frame
            .output_tokens
            .max(live.map_or(0, |live| live.output_tokens)),
        updated_at: crate::runtime::history::now_label(),
        ..frame.clone()
    }
}

/// One settled child, as the delegating model reads it: identity, status, the
/// work it really did, and the outcome text that decides what happens next.
fn settlement_line(frame: &SubagentChildFrameV1, status: ChildStatusV1) -> String {
    let outcome = bounded_excerpt(&frame.final_text, 240);
    let outcome = if outcome.is_empty() {
        String::new()
    } else {
        format!(" Outcome: {outcome}")
    };
    format!(
        "child {} {}{} ({} model turn(s), {} tool call(s), {} input / {} output tokens).{}",
        frame.child_id,
        status.as_str(),
        if frame.kind == ChildKindV1::Fork {
            " [fork]"
        } else if frame.kind == ChildKindV1::External {
            " [external]"
        } else {
            ""
        },
        frame.model_turns,
        frame.tool_calls,
        frame.input_tokens,
        frame.output_tokens,
        outcome
    )
}

fn stall_line(frame: &SubagentChildFrameV1, sequence: u64, age_seconds: u64) -> String {
    format!(
        "child {} is stalled: its last committed sequence is {}, {}s ago, and it has \
         produced no event since. Call list_subagents to inspect it, message it to steer \
         it, or cancel it.",
        frame.child_id, sequence, age_seconds
    )
}

/// Byte budget for one delegation report. The dispatch bound covers the whole
/// notice, so the report is summarised rather than transcribed.
const MAXIMUM_CHILD_REPORT_BYTES: usize = 2048;

/// Render one bounded delegation report. Pure so the bound is testable without a
/// child process: a report of any size states how much it omitted.
pub(super) fn render_child_reports(settled: &[String], stalled: &[String]) -> String {
    let mut lines: Vec<&str> = settled.iter().map(String::as_str).collect();
    lines.extend(stalled.iter().map(String::as_str));
    let mut listed = Vec::new();
    let mut used = 0usize;
    let mut omitted = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let separator = if index == 0 { 0 } else { 2 };
        if used.saturating_add(separator).saturating_add(line.len()) > MAXIMUM_CHILD_REPORT_BYTES {
            omitted = lines.len().saturating_sub(index);
            break;
        }
        used = used.saturating_add(separator).saturating_add(line.len());
        listed.push(*line);
    }
    let mut report = if stalled.is_empty() {
        format!(
            "{} delegated child/children settled. {}",
            settled.len(),
            listed.join("; ")
        )
    } else {
        format!(
            "{} settled, {} stalled. {}",
            settled.len(),
            stalled.len(),
            listed.join("; ")
        )
    };
    if omitted > 0 {
        report.push_str(&format!(
            " {omitted} further child report(s) are not listed; call list_subagents to see them all."
        ));
    }
    report
}

/// One bounded single-line excerpt for a report.
fn bounded_excerpt(text: &str, maximum: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= maximum {
        return collapsed;
    }
    let mut end = maximum;
    while !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &collapsed[..end])
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

/// Bounded child lifecycle fact. Later facts for one child supersede earlier
/// ones, so the desktop folds the newest status per childId.
fn child_fact(frame: &SubagentChildFrameV1, status: ChildStatusV1) -> Value {
    json!({
        "schemaVersion": 1,
        "childId": frame.child_id,
        "chatId": frame.chat_id,
        "runId": frame.run_id,
        "nodeId": frame.node_id,
        "parentInvocationId": frame.parent_invocation_id,
        "parentCallId": frame.parent_call_id,
        "kind": frame.kind,
        "status": status.as_str(),
        "depth": frame.depth,
        "task": frame.task,
        "modelTurns": frame.model_turns,
        "toolCalls": frame.tool_calls,
        "inputTokens": frame.input_tokens,
        "outputTokens": frame.output_tokens,
        "headRevision": frame.head_revision,
        "createdAt": frame.created_at,
        "updatedAt": frame.updated_at,
    })
}

/// Commits one child lifecycle fact so the desktop catalog updates without a
/// model turn. A caller that cannot commit leaves the durable frame as the
/// authoritative record and reports the failure.
pub(crate) fn publish_child_fact(
    run_events: &RunEventStream,
    frame: &SubagentChildFrameV1,
    status: ChildStatusV1,
) -> Result<(), String> {
    run_events
        .context_event(CHILD_FACT, child_fact(frame, status))
        .map(|_| ())
}

/// Record that the delegating Agent already received this child's terminal status
/// as a direct tool result, so it is not reported to it a second time.
pub(super) fn mark_child_reported(
    run_events: &RunEventStream,
    frame: &SubagentChildFrameV1,
    status: ChildStatusV1,
) -> Result<(), String> {
    run_events
        .context_event(
            CHILD_REPORT,
            json!({
                "key": child_report_key(&frame.child_id, frame.head_revision, status.as_str()),
                "childId": frame.child_id,
                "chatId": frame.chat_id,
            }),
        )
        .map(|_| ())
}

impl FileToolAuthorityRuntimeV1 {
    /// Authoritative catalog of one Chat/Run's delegated children.
    ///
    /// Durable frames are the source of truth: they are committed before a
    /// child starts, so a restart that loses the live job reports the child as
    /// interrupted instead of leaving a phantom running child. Live children
    /// sort first, then settled children oldest-first.
    pub(crate) fn subagent_catalog(
        &self,
        chat_id: &str,
        run_id: &StableId,
    ) -> Result<Vec<crate::runtime::dto::SubagentChildSummaryDto>, String> {
        let frames = self
            .records
            .subagent_children(run_id)
            .map_err(|error| error.to_string())?;
        let mut catalog = frames
            .into_iter()
            .filter(|frame| frame.chat_id == chat_id)
            .map(|frame| {
                let status = effective_status(&frame, &self.jobs, chat_id);
                crate::runtime::dto::SubagentChildSummaryDto {
                    child_id: frame.child_id,
                    kind: match frame.kind {
                        ChildKindV1::Fresh => "fresh".to_owned(),
                        ChildKindV1::Fork => "fork".to_owned(),
                        ChildKindV1::External => "external".to_owned(),
                    },
                    status: status.as_str().to_owned(),
                    running: status == ChildStatusV1::Running,
                    depth: frame.depth,
                    node_id: frame.node_id,
                    parent_invocation_id: frame.parent_invocation_id,
                    parent_call_id: frame.parent_call_id,
                    parent_child_id: frame.parent_child_id,
                    task: frame.task,
                    context_text: frame.context_text,
                    final_text: frame.final_text,
                    model_turns: frame.model_turns,
                    tool_calls: frame.tool_calls,
                    input_tokens: frame.input_tokens,
                    output_tokens: frame.output_tokens,
                    head_revision: frame.head_revision,
                    created_at: frame.created_at,
                    updated_at: frame.updated_at,
                }
            })
            .collect::<Vec<_>>();
        catalog.sort_by(|left, right| {
            right
                .running
                .cmp(&left.running)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.child_id.cmp(&right.child_id))
        });
        Ok(catalog)
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn a_settled_report_names_the_child_its_status_and_its_real_work() {
        let report = render_child_reports(
            &[settlement_line(
                &frame("child.abc", ChildKindV1::Fresh, "", 7, 3, 200, 40),
                ChildStatusV1::Failed,
            )],
            &[],
        );
        assert!(report.contains("child.abc"));
        assert!(report.contains("failed"));
        assert!(report.contains("7 model turn(s)"));
        assert!(report.contains("200 input / 40 output tokens"));
        assert!(report.contains("1 delegated child/children settled"));
    }

    #[test]
    fn a_stall_names_the_last_sequence_and_its_age() {
        let report = render_child_reports(
            &[],
            &[stall_line(
                &frame("child.stuck", ChildKindV1::Fresh, "", 4, 0, 0, 0),
                11,
                900,
            )],
        );
        assert!(report.contains("child.stuck is stalled"));
        assert!(
            report.contains("last committed sequence is 11"),
            "the sequence is what a reader resumes from: {report}"
        );
        assert!(report.contains("900s ago"), "the age is stated: {report}");
    }

    #[test]
    fn a_report_of_any_size_states_what_it_omitted() {
        // Hundreds of settled children must not grow the notice with the job
        // count: it is a report, not a transcription.
        let settled = (0..400)
            .map(|index| {
                settlement_line(
                    &frame(&format!("child.{index}"), ChildKindV1::Fresh, "", 1, 0, 0, 0),
                    ChildStatusV1::Completed,
                )
            })
            .collect::<Vec<_>>();
        let report = render_child_reports(&settled, &[]);
        assert!(
            report.len() < MAXIMUM_CHILD_REPORT_BYTES + 512,
            "the inventory is capped: {} bytes",
            report.len()
        );
        assert!(
            report.contains("further child report(s) are not listed"),
            "the omission is explicit: {report}"
        );
        assert!(report.contains("400 delegated child/children settled"));
    }

    #[test]
    fn each_terminal_child_is_reported_once_and_a_resumed_child_reports_again() {
        let mut failed = frame("child.failed", ChildKindV1::Fresh, "provider refused", 2, 0, 0, 0);
        failed.status = ChildStatusV1::Failed;
        let delivered = std::collections::BTreeSet::new();
        let plan = plan_child_reports([&failed].into_iter(), &delivered, |_| {
            (ChildStatusV1::Failed, None)
        });
        assert_eq!(plan.settled.len(), 1);
        assert_eq!(plan.markers.len(), 1, "the delivery is recorded once");

        // The next turn finds that key already delivered and says nothing again.
        let delivered: std::collections::BTreeSet<String> =
            plan.markers.iter().map(|(key, _)| key.clone()).collect();
        let repeat = plan_child_reports([&failed].into_iter(), &delivered, |_| {
            (ChildStatusV1::Failed, None)
        });
        assert!(repeat.is_empty(), "a delivered status is not repeated");

        // A continuation advances the revision, so its new outcome is reported.
        let mut resumed = failed.clone();
        resumed.head_revision = failed.head_revision + 1;
        resumed.status = ChildStatusV1::Completed;
        let after = plan_child_reports([&resumed].into_iter(), &delivered, |_| {
            (ChildStatusV1::Completed, None)
        });
        assert_eq!(after.settled.len(), 1);
        assert!(after.settled[0].contains("completed"));
    }

    #[test]
    fn a_live_child_that_stopped_producing_events_is_reported_as_a_stall() {
        let running = frame("child.quiet", ChildKindV1::Fresh, "", 3, 0, 0, 0);
        let live = crate::runtime::tool_loop::jobs::ChildLiveV1 {
            running: true,
            last_sequence: 9,
            age_seconds: SUBAGENT_STALL_SECONDS + 60,
            model_turns: 3,
            input_tokens: 0,
            output_tokens: 0,
        };
        let plan = plan_child_reports(
            [&running].into_iter(),
            &std::collections::BTreeSet::new(),
            |_| (ChildStatusV1::Running, Some(live)),
        );
        assert_eq!(plan.settled.len(), 0);
        assert_eq!(plan.stalled.len(), 1);
        assert!(plan.stalled[0].contains("child.quiet"));
        assert!(
            plan.stalled[0].contains("last committed sequence is 9"),
            "the report names the sequence, not just the child: {}",
            plan.stalled[0]
        );

        // A child that is merely working is not a stall.
        let working = crate::runtime::tool_loop::jobs::ChildLiveV1 {
            age_seconds: 5,
            ..live
        };
        let quiet = plan_child_reports(
            [&running].into_iter(),
            &std::collections::BTreeSet::new(),
            |_| (ChildStatusV1::Running, Some(working)),
        );
        assert!(quiet.is_empty(), "progress is not silence");
    }

    #[test]
    fn a_restart_interrupted_child_is_reported_as_a_terminal_status() {
        let running = frame("child.interrupted", ChildKindV1::Fresh, "", 1, 0, 0, 0);
        let plan = plan_child_reports(
            [&running].into_iter(),
            &std::collections::BTreeSet::new(),
            |_| (ChildStatusV1::Interrupted, None),
        );
        assert_eq!(plan.settled.len(), 1);
        assert!(
            plan.settled[0].contains("interrupted"),
            "an interrupted child is a terminal status, not a phantom runner: {}",
            plan.settled[0]
        );
    }

    #[test]
    fn a_long_outcome_is_bounded_to_one_excerpt() {
        let long = "x".repeat(10_000);
        let excerpt = bounded_excerpt(&long, 240);
        assert!(
            excerpt.len() <= 240 + '…'.len_utf8(),
            "one bounded excerpt: {} bytes",
            excerpt.len()
        );
        assert!(excerpt.ends_with('…'));
    }

    fn frame(
        child_id: &str,
        kind: ChildKindV1,
        final_text: &str,
        model_turns: u32,
        tool_calls: u32,
        input_tokens: u64,
        output_tokens: u64,
    ) -> SubagentChildFrameV1 {
        SubagentChildFrameV1 {
            child_id: child_id.to_owned(),
            chat_id: "chat".into(),
            run_id: "run".into(),
            node_id: "node".into(),
            parent_invocation_id: "invoke".into(),
            parent_call_id: "call".into(),
            parent_child_id: None,
            depth: 1,
            kind,
            projection_hash: None,
            projection_items: 0,
            projection_dropped: 0,
            inherited_tool_ids: Vec::new(),
            read_only: false,
            head_revision: 1,
            status: ChildStatusV1::Running,
            task: "task".into(),
            context_text: String::new(),
            input: serde_json::Value::Null,
            exchanges: Vec::new(),
            final_text: final_text.to_owned(),
            blocked_actions: Vec::new(),
            model_turns,
            tool_calls,
            input_tokens,
            output_tokens,
            created_at: "0".into(),
            updated_at: "0".into(),
        }
    }
}
