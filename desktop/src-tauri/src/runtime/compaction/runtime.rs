//! Trusted admission/persistence adapter for context compaction.
use super::*;
use crate::runtime::{
    compaction as c, context_inspection::ContextDocument, model_tool_loop::AgentContextV1,
};
use aworkit_capability_host::{
    FrozenModelGateway, ModelResolutionPlanV1, ModelToolContextV1, ModelToolRequestV1,
    project_model_tool_events,
};

#[derive(Default)]
struct SummaryCapture(Mutex<Vec<aworkit_capability_host::ModelToolEventV1>>);
impl aworkit_capability_host::ModelEventObserverV1 for SummaryCapture {
    fn model_tool_event(&self, event: &aworkit_capability_host::ModelToolEventV1) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.clone());
    }
}

impl BoundFileToolAuthorityV1 {
    pub(crate) fn compact_existing(
        &self,
        gateway: &FrozenModelGateway,
        plan: &ModelResolutionPlanV1,
        outer: &StableId,
        node: &crate::runtime::graph_pass::CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> crate::runtime::graph_pass::GraphPassOutcomeV1 {
        use crate::runtime::graph_pass::{GraphPassOutcomeV1, GraphPassStatusV1};
        let owner = AgentContextV1 {
            node_id: node.id.clone(),
            child: None,
            tool_ids: node
                .tool_bindings
                .iter()
                .map(|b| b.capability_id.clone())
                .collect(),
        };
        let result = (|| {
            let restore = node.node_type == "agent" && self.context_snapshot(&owner)?.is_some();
            // Older Chats already have immutable provider requests, even before
            // they have a compaction checkpoint. Include the final node answer.
            let mut request = crate::runtime::context_inspection::select_context(
                &self.run_events.context_events()?,
                &owner.node_id,
            )?
            .document
            .request();
            let surface = c::units(&request)?;
            if c::select_prefix(&surface, 0).is_none() {
                return Ok(c::Preparation::default());
            }
            self.manage_selected_context(
                gateway,
                plan,
                outer,
                0,
                Some(&owner),
                &mut request,
                cancellation,
                c::Trigger::Manual,
                restore,
            )
        })();
        let (status, assistant_text, error, input_units, output_units) = match result {
            Ok(result) if result.error.is_some() => (
                GraphPassStatusV1::Failed,
                None,
                result.error,
                result.input_tokens,
                result.output_tokens,
            ),
            Ok(result) => (
                GraphPassStatusV1::Succeeded,
                Some(
                    if result.changed {
                        "Context compacted."
                    } else {
                        "No useful context reduction is available."
                    }
                    .into(),
                ),
                None,
                result.input_tokens,
                result.output_tokens,
            ),
            Err(error) => (GraphPassStatusV1::Failed, None, Some(error), 0, 0),
        };
        GraphPassOutcomeV1 {
            status,
            assistant_text,
            error,
            input_units,
            output_units,
            approval: None,
            pending_state: None,
            activity: Vec::new(),
            tool_activity: Vec::new(),
            exchanges: Vec::new(),
            attempted_model_turns: 0,
            settled_tool_calls: 0,
        }
    }
    fn context_owner(&self, agent: Option<&AgentContextV1>) -> AgentContextV1 {
        agent.cloned().unwrap_or_else(|| AgentContextV1 {
            node_id: self
                .run_events
                .active_context_node()
                .unwrap_or_else(|| self.context.node_id.to_string()),
            child: None,
            tool_ids: Vec::new(),
        })
    }

    fn context_key(&self) -> String {
        c::hash(&json!({"chat":self.context.chat_id,"branch":self.context.project_branch}))
    }

    fn selection_generation(&self, owner: &AgentContextV1) -> Result<u64, String> {
        Ok(self
            .run_events
            .context_events()?
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    e.kind.as_str(),
                    "context.checkpoint" | "context.edited" | "context.compacted"
                ) && e.payload["nodeId"] == owner.node_id
                    && e.payload["child"] == json!(owner.child)
                    && e.payload
                        .get("ownerKey")
                        .is_none_or(|key| key == &json!(self.context_key()))
            })
            .map_or(0, |e| e.sequence))
    }

    fn context_snapshot(
        &self,
        owner: &AgentContextV1,
    ) -> Result<Option<(u64, c::Snapshot)>, String> {
        self.run_events
            .context_events()?
            .iter()
            .rev()
            .find(|e| {
                e.kind == "context.checkpoint"
                    && e.payload["ownerKey"] == self.context_key()
                    && e.payload["nodeId"] == owner.node_id
                    && e.payload["child"] == json!(owner.child)
            })
            .map(|e| {
                serde_json::from_value(e.payload["snapshot"].clone())
                    .map(|s| (e.sequence, s))
                    .map_err(|e| e.to_string())
            })
            .transpose()
    }

    fn append_completed_exchanges(
        &self,
        request: &mut ModelToolRequestV1,
        outer: &str,
        after: usize,
        through: Option<usize>,
    ) -> Result<(), String> {
        let mut exchanges: Vec<_> = self
            .runtime
            .records
            .events("pipeline.model-tool-exchange")
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|e| e["outerInvocationId"] == outer)
            .filter(|e| {
                e["turn"].as_u64().is_some_and(|t| {
                    t as usize > after && through.is_none_or(|end| t as usize <= end)
                })
            })
            .collect();
        exchanges.sort_by_key(|e| e["turn"].as_u64());
        for entry in exchanges {
            request.exchanges.push(
                serde_json::from_value(entry["exchange"].clone()).map_err(|e| e.to_string())?,
            );
        }
        Ok(())
    }

    /// Restore only committed context, importing each conversation message and
    /// authority-settled exchange once. No historical invocation is dispatched.
    fn restore_context(
        &self,
        owner: &AgentContextV1,
        outer: &StableId,
        through: usize,
        request: &mut ModelToolRequestV1,
        conversation_context: bool,
    ) -> Result<Option<c::Anchor>, String> {
        let events: Vec<_> = self
            .run_events
            .context_events()?
            .into_iter()
            .filter(|e| {
                e.payload
                    .get("ownerKey")
                    .is_none_or(|key| key == &json!(self.context_key()))
            })
            .collect();
        let previous = self.context_snapshot(owner)?;
        let edit = owner
            .child
            .is_none()
            .then(|| {
                events
                    .iter()
                    .rev()
                    .find(|e| e.kind == "context.edited" && e.payload["nodeId"] == owner.node_id)
            })
            .flatten();
        if edit.is_some_and(|edit| {
            previous
                .as_ref()
                .is_none_or(|(seq, _)| edit.sequence > *seq)
        }) {
            crate::runtime::context_inspection::apply_edit(&events, &owner.node_id, request)?;
            return Ok(None);
        }
        let Some((sequence, snapshot)) = previous else {
            let conversation: Vec<_> = events.iter().filter_map(|e| {
                let role = match e.kind.as_str() { "message.user" => "user", "message.assistant" => "assistant", _ => return None };
                Some(json!({"role":role,"content":e.payload["body"],"images":e.payload["attachments"].as_array().cloned().unwrap_or_default()}))
            }).collect();
            if conversation_context && owner.child.is_none() && !conversation.is_empty() {
                let messages = request.input["messages"]
                    .as_array_mut()
                    .ok_or("Context requires messages")?;
                messages.retain(|m| m["role"] == "system");
                messages.extend(conversation);
            }
            return Ok(None);
        };
        if snapshot.document.tools != request.tools {
            return Err("Context checkpoint tools differ from the frozen Agent selection".into());
        }
        let mut restored = snapshot.document.request();
        restored.parameters = request.parameters.clone();
        // System layers are current workflow facts. Changing them invalidates an
        // anchor but cannot resurrect shadowed conversation or tool results.
        let current_system: Vec<_> = request.input["messages"]
            .as_array()
            .ok_or("Context requires messages")?
            .iter()
            .take_while(|m| m["role"] == "system")
            .cloned()
            .collect();
        let messages = restored.input["messages"]
            .as_array_mut()
            .ok_or("Invalid context checkpoint messages")?;
        let systems = messages
            .iter()
            .take_while(|m| m["role"] == "system")
            .count();
        if edit.is_none() {
            messages.splice(0..systems, current_system);
        }
        let same = snapshot.outer == outer.as_str();
        if same && through < snapshot.through {
            return Err("Context exchange cursor moved backwards".into());
        }
        self.append_completed_exchanges(
            &mut restored,
            &snapshot.outer,
            snapshot.through,
            same.then_some(through),
        )?;
        if !same {
            if !conversation_context {
                for message in request.input["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| m["role"] != "system")
                {
                    restored.context_messages.push(ModelToolContextV1 {
                        after_exchanges: restored.exchanges.len(),
                        content: message["content"].as_str().unwrap_or_default().into(),
                        role: message["role"].as_str().map(str::to_owned),
                        images: message["images"].as_array().cloned().unwrap_or_default(),
                        ..Default::default()
                    });
                }
            } else if owner.child.is_none() {
                let canonical: Vec<crate::runtime::pipeline::WorkflowMessageV1> = if let Some(
                    after,
                ) =
                    snapshot.conversation_sequence
                {
                    events.iter().filter(|e|e.sequence>after).filter_map(|e| {
                        let role = match e.kind.as_str() { "message.user" => "user", "message.assistant" => "assistant", _ => return None };
                        Some(serde_json::from_value(json!({"role":role,"content":e.payload["body"],"images":e.payload["attachments"].as_array().cloned().unwrap_or_default()})).map_err(|e|e.to_string()))
                    }).collect::<Result<_,_>>()?
                } else {
                    if snapshot.conversation_cursor > self.context.review_messages.len() {
                        return Err(
                            "Conversation cursor no longer identifies the committed history".into(),
                        );
                    }
                    self.context
                        .review_messages
                        .iter()
                        .skip(snapshot.conversation_cursor)
                        .cloned()
                        .collect()
                };
                for message in canonical.iter().filter(|m| m.role != "system") {
                    restored.context_messages.push(ModelToolContextV1 {
                        after_exchanges: restored.exchanges.len(),
                        content: message.content.clone(),
                        role: Some(message.role.clone()),
                        images: message
                            .images
                            .iter()
                            .map(|v| serde_json::to_value(v).expect("image metadata"))
                            .collect(),
                        ..Default::default()
                    });
                }
            }
            self.append_completed_exchanges(&mut restored, outer.as_str(), 0, Some(through))?;
        }
        // Catalog/direct Skill additions are frozen at each step. Their old
        // copies already belong to the checkpoint, including any shadowing.
        let local_start = if same {
            snapshot.through.saturating_add(1)
        } else {
            0
        };
        for context in request.context_messages.iter().filter(|m| {
            m.instruction_event_id.is_none()
                && m.after_input_messages.is_none()
                && m.after_exchanges >= local_start
        }) {
            let mut context = context.clone();
            context.after_exchanges = restored
                .exchanges
                .len()
                .saturating_sub(through.saturating_sub(context.after_exchanges));
            restored.context_messages.push(context);
        }
        restored.retry_notice = request.retry_notice.clone();
        let anchor = events
            .iter()
            .rev()
            .find(|e| {
                e.kind == "context.usage"
                    && e.sequence > sequence
                    && e.payload["nodeId"] == owner.node_id
                    && e.payload["child"] == json!(owner.child)
            })
            .map(|e| serde_json::from_value(e.payload["anchor"].clone()).map_err(|e| e.to_string()))
            .transpose()?
            .or(snapshot.anchor);
        *request = restored;
        Ok(anchor)
    }

    fn save_context(
        &self,
        owner: &AgentContextV1,
        outer: &StableId,
        through: usize,
        request: &ModelToolRequestV1,
        anchor: Option<c::Anchor>,
    ) -> Result<(), String> {
        let payload = self.snapshot_payload(owner, outer, through, request, anchor)?;
        self.run_events
            .context_event("context.checkpoint", payload)?;
        Ok(())
    }
    fn snapshot_payload(
        &self,
        owner: &AgentContextV1,
        outer: &StableId,
        through: usize,
        request: &ModelToolRequestV1,
        anchor: Option<c::Anchor>,
    ) -> Result<Value, String> {
        let snapshot = c::Snapshot {
            node_id: owner.node_id.clone(),
            child: owner.child.clone(),
            outer: outer.to_string(),
            through,
            conversation_cursor: self.context.review_messages.len(),
            document: ContextDocument::from_request(request),
            anchor,
            conversation_sequence: self
                .run_events
                .context_events()?
                .iter()
                .rev()
                .find(|e| matches!(e.kind.as_str(), "message.user" | "message.assistant"))
                .map(|e| e.sequence),
        };
        snapshot.document.validate()?;
        Ok(
            json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"pressureTokens":c::pressure(request,snapshot.anchor.as_ref())?,"pressureReported":snapshot.anchor.as_ref().is_some_and(|a|a.header_hash==c::header_hash(request)&&a.reported>=a.estimated),"snapshot":snapshot}),
        )
    }

    pub(super) fn context_usage(
        &self,
        outer: &StableId,
        agent: Option<&AgentContextV1>,
        request: &ModelToolRequestV1,
        input: u64,
        output: u64,
        assistant_tokens: u64,
    ) -> Result<(), String> {
        let owner = self.context_owner(agent);
        // Anchor the completed provider turn, including assistant output.
        // Settled tool results and future messages are then repriced as deltas.
        let anchor = c::Anchor {
            header_hash: c::header_hash(request),
            estimated: c::estimate(request)?.saturating_add(assistant_tokens),
            reported: input.saturating_add(output),
        };
        self.run_events.context_event("context.usage", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"outer":outer,"anchor":anchor,"inputTokens":input,"outputTokens":output}))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn manage_context(
        &self,
        gateway: &FrozenModelGateway,
        plan: &ModelResolutionPlanV1,
        outer: &StableId,
        through: usize,
        agent: Option<&AgentContextV1>,
        request: &mut ModelToolRequestV1,
        cancellation: &CancellationToken,
        trigger: c::Trigger,
    ) -> Result<c::Preparation, String> {
        self.manage_selected_context(
            gateway,
            plan,
            outer,
            through,
            agent,
            request,
            cancellation,
            trigger,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn manage_selected_context(
        &self,
        gateway: &FrozenModelGateway,
        plan: &ModelResolutionPlanV1,
        outer: &StableId,
        through: usize,
        agent: Option<&AgentContextV1>,
        request: &mut ModelToolRequestV1,
        cancellation: &CancellationToken,
        trigger: c::Trigger,
        restore: bool,
    ) -> Result<c::Preparation, String> {
        let owner = self.context_owner(agent);
        let metadata: c::Metadata = serde_json::from_value(self.context.model_context.clone())
            .map_err(|e| e.to_string())?;
        let lock = {
            let mut locks = self
                .runtime
                .records
                .instruction_locks
                .lock()
                .map_err(|_| "Context lock poisoned")?;
            let key = format!(
                "compaction:{}:{}:{:?}",
                self.context_key(),
                owner.node_id,
                owner.child
            );
            match locks.get(&key).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let _guard = lock.lock().map_err(|_| "Context lock poisoned")?;
        // A process interrupted between start and end never committed a partial
        // replacement. Close its maintenance marker before another attempt.
        let lifecycle = self.run_events.context_events()?;
        for event in lifecycle.iter().filter(|e| {
            e.kind == "context.compaction-started"
                && e.payload["ownerKey"] == self.context_key()
                && e.payload["nodeId"] == owner.node_id
                && e.payload["child"] == json!(owner.child)
        }) {
            if !lifecycle.iter().any(|e| {
                e.kind == "context.compaction-ended"
                    && e.payload["compactionId"] == event.payload["compactionId"]
            }) {
                let committed = lifecycle.iter().find(|e| {
                    e.kind == "context.compacted"
                        && e.payload["compactionId"] == event.payload["compactionId"]
                });
                self.run_events.context_event("context.compaction-ended",json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":event.payload["compactionId"],"error":if committed.is_some(){None}else{Some("Compaction interrupted before settlement")},"auxiliary":committed.map(|e|&e.payload["auxiliary"]),"recovered":true}))?;
            }
        }
        metadata.policy.validate(metadata.context_window)?;
        let mut outcome = c::Preparation {
            durable: true,
            max_overflow_retries: metadata.policy.max_overflow_retries,
            ..Default::default()
        };
        let anchor = if !restore {
            None
        } else if trigger == c::Trigger::ContextOverflow {
            self.context_snapshot(&owner)?.and_then(|(_, s)| s.anchor)
        } else {
            self.restore_context(&owner, outer, through, request, agent.is_some())?
        };
        if let Some(agent) = agent {
            self.workspace_context(outer, through, agent, request, cancellation)?;
        }
        if cancellation.is_cancelled() {
            return Err("Context preparation cancelled".into());
        }
        let bytes = serde_json::to_vec(request)
            .map_err(|e| e.to_string())?
            .len();
        let pressure = c::pressure(request, anchor.as_ref())?;
        // Leave room for one newly settled exchange in approval/recovery
        // checkpoints while preserving the original event-store hard bound.
        let byte_limit = plan.maximum_input_bytes.min(512 * 1024);
        let byte_pressure = bytes > byte_limit;
        let qualifies = trigger != c::Trigger::Pressure
            || (metadata.policy.auto
                && (byte_pressure
                    || metadata
                        .context_window
                        .is_some_and(|capacity| pressure >= metadata.policy.threshold(capacity))));
        if metadata.policy.auto
            && metadata.context_window.is_none()
            && trigger == c::Trigger::Pressure
            && self.context_snapshot(&owner)?.is_none()
        {
            self.run_events.context_event("context.compaction-warning", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"body":"Automatic token-pressure compaction needs this model's context window in Settings. Provider overflow recovery and durable byte-pressure compaction remain available."}))?;
        }
        if qualifies {
            let effective_trigger = if byte_pressure && trigger == c::Trigger::Pressure {
                c::Trigger::BytePressure
            } else {
                trigger
            };
            if metadata.policy.prune_tool_results && trigger != c::Trigger::Manual {
                let before = request.clone();
                let pruned = c::prune(request, &metadata.policy);
                if !pruned.is_empty() {
                    let checkpoint =
                        self.snapshot_payload(&owner, outer, through, request, anchor.clone())?;
                    if cancellation.is_cancelled() {
                        return Err("Context compaction cancelled".into());
                    }
                    self.run_events.context_batch(vec![("context.compacted", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"trigger":effective_trigger,"strategy":"tool-result-pruning","beforeHash":c::hash(&before),"afterHash":c::hash(request),"pruned":pruned,"document":ContextDocument::from_request(request),"body":"Large tool results compacted; original results remain in Run details."})),("context.checkpoint",checkpoint)])?;
                    outcome.changed = true;
                }
            }
            let attempts = if matches!(
                effective_trigger,
                c::Trigger::Manual | c::Trigger::ContextOverflow
            ) {
                1
            } else {
                metadata.policy.compaction_retries + 1
            };
            for _ in 0..attempts {
                if cancellation.is_cancelled() {
                    return Err("Context compaction cancelled".into());
                }
                let under_tokens = metadata.context_window.is_none_or(|capacity| {
                    c::pressure(request, anchor.as_ref()).unwrap_or(u64::MAX)
                        < metadata.policy.threshold(capacity)
                });
                let under_bytes = serde_json::to_vec(request)
                    .map_err(|e| e.to_string())?
                    .len()
                    <= byte_limit;
                if matches!(
                    effective_trigger,
                    c::Trigger::Pressure | c::Trigger::BytePressure
                ) && under_tokens
                    && under_bytes
                {
                    break;
                }
                let surface = c::units(request)?;
                let retain = if matches!(
                    effective_trigger,
                    c::Trigger::Manual | c::Trigger::ContextOverflow | c::Trigger::BytePressure
                ) {
                    0
                } else {
                    metadata
                        .policy
                        .retention(metadata.context_window.unwrap_or_default())
                };
                let Some(cut) = c::select_prefix(&surface, retain) else {
                    break;
                };
                let before_hash = c::hash(request);
                let source_generation = self.selection_generation(&owner)?;
                let id = format!(
                    "compact.{}.{}",
                    self.context.run_id,
                    self.run_events
                        .context_events()?
                        .last()
                        .map_or(1, |e| e.sequence + 1)
                );
                let mut summary_request = request.clone();
                let mut summary_surface = surface[..cut].to_vec();
                summary_surface.push(c::Unit::Message(ModelToolContextV1 {
                    content: c::INSTRUCTION.trim_end().into(),
                    ..Default::default()
                }));
                c::replace_units(&mut summary_request, &summary_surface)?;
                summary_request.retry_notice = None;
                summary_request
                    .parameters
                    .insert("maxOutputTokens".into(), json!(metadata.policy.max_tokens));
                let source_events: Vec<_> = self
                    .run_events
                    .context_events()?
                    .into_iter()
                    .filter(|e| {
                        e.sequence == source_generation
                            || (matches!(e.kind.as_str(), "message.user" | "message.assistant")
                                && e.sequence > source_generation)
                    })
                    .map(|e| e.event_id)
                    .collect();
                let start = self.run_events.context_event("context.compaction-started", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"trigger":effective_trigger,"beforeHash":before_hash,"sourceGeneration":source_generation,"sourceEventIds":source_events,"outer":outer,"through":through,"sourceUnits":surface[..cut].iter().map(c::hash).collect::<Vec<_>>(),"sourceInstructionIds":surface[..cut].iter().filter_map(|u|match u{c::Unit::Message(m)=>m.instruction_event_id.as_ref(),_=>None}).collect::<Vec<_>>(),"body":"Compacting context…"}))?;
                let mut summary_plan = plan.clone();
                // Auxiliary work consumes the selected source before reduction,
                // so the acting node's smaller admission limit cannot price it.
                summary_plan.maximum_input_bytes =
                    crate::runtime::context_inspection::MAX_CONTEXT_BYTES;
                summary_plan.maximum_output_bytes = 128 * 1024;
                if let Some(target) = &metadata.summary_target {
                    summary_plan.candidates = vec![aworkit_capability_host::ModelCandidateV1 {
                        binding_id: c::SUMMARY_BINDING.into(),
                        version_hash: c::hash(target),
                    }];
                    summary_request.parameters = target.model.parameters.clone();
                    summary_request
                        .parameters
                        .insert("maxOutputTokens".into(), json!(metadata.policy.max_tokens));
                }
                let capture = SummaryCapture::default();
                let result = gateway.execute_compaction_cancellable(
                    &summary_plan,
                    &summary_request,
                    cancellation,
                    &capture,
                );
                let raw = capture.0.into_inner().unwrap_or_else(|p| p.into_inner());
                let output = project_model_tool_events(&raw);
                outcome.input_tokens = outcome.input_tokens.saturating_add(output.input_tokens);
                outcome.output_tokens = outcome.output_tokens.saturating_add(output.output_tokens);
                let auxiliary = json!({"selectedBinding":result.as_ref().ok().map(|e|&e.selected_binding),"maxTokens":metadata.policy.max_tokens,"rawOutput":raw,"inputTokens":output.input_tokens,"outputTokens":output.output_tokens});
                let result = result.map_err(|e|e.to_string()).and_then(|_evidence| {
                    if cancellation.is_cancelled() { return Err("Context compaction cancelled".into()); }
                    // Match Harness text projection. Auxiliary tool requests
                    // remain raw evidence only; this path has no tool dispatcher.
                    if output.assistant_text.trim().is_empty() { return Err("Compaction produced no text summary".into()); }
                    let summary = c::Unit::Message(ModelToolContextV1 { content:c::frame_summary(output.assistant_text.trim()), ..Default::default() });
                    let shadowed_tokens: u64 = surface[..cut].iter().map(c::Unit::tokens).sum();
                    if summary.tokens() >= shadowed_tokens { return Err("Compaction summary is not smaller than the selected history including checkpoint framing".into()); }
                    if c::hash(request) != before_hash || self.selection_generation(&owner)? != source_generation { return Err("Context changed during compaction".into()); }
                    let mut replacement = request.clone();
                    let mut next = vec![summary]; next.extend_from_slice(&surface[cut..]);
                    c::replace_units(&mut replacement,&next)?;
                    let checkpoint=self.snapshot_payload(&owner,outer,through,&replacement,anchor.clone())?;
                    if cancellation.is_cancelled() { return Err("Context compaction cancelled".into()); }
                    self.run_events.context_batch(vec![("context.compacted", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"startSequence":start.sequence,"trigger":effective_trigger,"strategy":"summary","beforeHash":before_hash,"afterHash":c::hash(&replacement),"shadowedUnits":cut,"shadowedTokenCount":shadowed_tokens,"document":ContextDocument::from_request(&replacement),"auxiliary":auxiliary,"body":"Context compacted. Earlier history remains available in this Chat."})),("context.checkpoint",checkpoint)])?;
                    *request = replacement;
                    Ok(())
                });
                self.run_events.context_event("context.compaction-ended", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"error":result.as_ref().err(),"auxiliary":auxiliary}))?;
                match result {
                    Ok(()) => outcome.changed = true,
                    Err(error) => {
                        if cancellation.is_cancelled() || trigger == c::Trigger::Manual {
                            outcome.error = Some(error);
                            return Ok(outcome);
                        }
                        self.run_events.context_event("context.compaction-warning",json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"body":error}))?;
                        break;
                    }
                }
            }
        }
        if outcome.changed {
            if let Some(agent) = agent {
                self.workspace_context(outer, through, agent, request, cancellation)?;
            }
        }
        if qualifies
            && metadata.context_window.is_some_and(|capacity| {
                c::pressure(request, anchor.as_ref()).unwrap_or(u64::MAX)
                    >= metadata.policy.threshold(capacity)
            })
        {
            self.run_events.context_event("context.compaction-warning",json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"body":"Context is still above the configured pressure threshold after the permitted reductions. The latest context is preserved; provider overflow recovery remains bounded by the configured retry policy."}))?;
        }
        if cancellation.is_cancelled() {
            return Err("Context preparation cancelled".into());
        }
        self.save_context(&owner, outer, through, request, anchor)?;
        Ok(outcome)
    }
}
