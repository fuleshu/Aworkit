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

/// What restoring a recorded projection left in the pass's own request.
///
/// A pass may only offer the tools it selects now, so recorded history that
/// calls a capability this pass cannot offer has no representable projection.
enum RestoredProjection {
    /// The recorded context is carried, with any anchor pressure may report
    /// against.
    Carried(Option<c::Anchor>),
    /// The recorded context cannot be projected under this pass's selection, so
    /// the pass keeps the context it already had.
    Declined,
}

impl BoundFileToolAuthorityV1 {
    /// A steered text-only model consumes the new canonical user message,
    /// without appending its original graph input a second time.
    pub(crate) fn with_steering_node(mut self, node: Option<String>) -> Self {
        self.steering_node = node;
        self
    }
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
            compaction: c::Overlay::from_node_configuration(&node.configuration),
        };
        let result = (|| {
            let restore = node.node_type == "agent" && self.context_snapshot(&owner)?.is_some();
            // Older Chats already have immutable provider requests, even before
            // they have a compaction checkpoint. Include the final node answer.
            let mut request = crate::runtime::context_inspection::select_context(
                &self.run_events.context_events_shared()?,
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
            // Compaction preparation makes one auxiliary summary call whose
            // usage is published separately as context.compaction-ended, so the
            // pass-level cache aggregate has nothing to add here.
            cached_input_units: None,
            uncached_input_units: None,
            approval: None,
            pending_state: None,
            stopped_state: None,
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
            // A Chat-level owner has no node configuration to overlay.
            compaction: None,
        })
    }

    fn context_key(&self) -> String {
        c::hash(&json!({"chat":self.context.chat_id,"branch":self.context.project_branch}))
    }

    fn selection_generation(&self, owner: &AgentContextV1) -> Result<u64, String> {
        Ok(self
            .run_events
            .context_events_shared()?
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
            .context_events_shared()?
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

    /// Recorded history is preserved evidence, never a claim on a pass's tool
    /// selection. When a checkpoint or a saved revision calls a capability this
    /// pass does not select, its recorded projection is declined and the
    /// condition is committed so the Chat explains why a pass ran on fresh
    /// context instead of ending the Agent node.
    fn record_declined_selection(
        &self,
        owner: &AgentContextV1,
        source: &str,
        capabilities: &[String],
    ) -> Result<(), String> {
        self.run_events.context_event(
            "context.selection-declined",
            json!({
                "ownerKey": self.context_key(),
                "nodeId": owner.node_id,
                "child": owner.child,
                "source": source,
                "capabilities": capabilities,
                "body": format!(
                    "Recorded context was not restored: it calls {} that this pass does not select. \
                     Committed evidence is unchanged; this pass continues on its current context.",
                    capabilities.join(", ")
                ),
            }),
        )?;
        Ok(())
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
            .events_matching("pipeline.model-tool-exchange", |e| {
                e["outerInvocationId"] == outer
                    && e["turn"].as_u64().is_some_and(|t| {
                        t as usize > after && through.is_none_or(|end| t as usize <= end)
                    })
            })
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
    ) -> Result<RestoredProjection, String> {
        let stream = self.run_events.context_events_shared()?;
        let events: Vec<_> = stream
            .iter()
            .filter(|e| {
                e.payload
                    .get("ownerKey")
                    .is_none_or(|key| key == &json!(self.context_key()))
            })
            .cloned()
            .collect();
        let previous = self.context_snapshot(owner)?;
        let mut edit = owner
            .child
            .is_none()
            .then(|| crate::runtime::context_inspection::saved_edit(&events, &owner.node_id))
            .transpose()?
            .flatten();
        if edit.as_ref().is_some_and(|edit| {
            previous
                .as_ref()
                .is_none_or(|(seq, _)| edit.sequence > *seq)
        }) {
            // An edit owns context already admitted for this invocation. Do not
            // re-add its initial graph context (or revive a deliberately removed
            // copy); only context from later exchange boundaries is new.
            //
            // The selection is this pass's interface; the revision supplies
            // history. A revision whose recorded calls name a capability this
            // pass no longer selects cannot be projected at all, so the pass
            // keeps its own fresh context and the next checkpoint replaces the
            // unrepresentable one.
            let mut edit = edit.take().expect("checked edit");
            if let crate::runtime::context_inspection::ContextAdmissionV1::Unavailable(
                capabilities,
            ) = crate::runtime::context_inspection::admit_edit(&mut edit, &request.tools)
            {
                self.record_declined_selection(owner, "edit", &capabilities)?;
                return Ok(RestoredProjection::Declined);
            }
            if let Some((_, snapshot)) =
                previous.as_ref().filter(|(_, s)| s.outer == outer.as_str())
            {
                request
                    .context_messages
                    .retain(|m| m.after_exchanges > snapshot.through);
            }
            crate::runtime::context_inspection::apply_edit(&events, &edit, request)?;
            return Ok(RestoredProjection::Carried(None));
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
            return Ok(RestoredProjection::Carried(None));
        };
        // A checkpoint records the selection of the pass that wrote it. The
        // acting node's frozen selection is this pass's interface, so it is
        // adopted here rather than compared: a refreshed description, schema or
        // provider alias never invalidates a Chat's committed history.
        let mut restored = snapshot.document.request();
        restored.tools = request.tools.clone();
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
            if !conversation_context
                && self.steering_node.as_deref() != Some(owner.node_id.as_str())
            {
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
        // Every recorded call the restored selection will carry must be
        // representable under this pass's selection: the checkpoint's own
        // exchanges and the exchanges appended from committed records alike.
        // Recorded calls are re-pointed at the provider name this pass offers for
        // the same capability id; a call whose capability this pass no longer
        // selects cannot be projected at all, so the projection is declined and
        // the pass keeps its own fresh context. The checkpoint saved at the end of
        // this turn then replaces the unrepresentable one. Committed evidence is
        // never rewritten, and no tool interface change ends the Agent node.
        if let crate::runtime::context_inspection::ContextAdmissionV1::Unavailable(capabilities) =
            crate::runtime::context_inspection::admit_current_tools(
                &mut restored.exchanges,
                &request.tools,
            )
        {
            self.record_declined_selection(owner, "checkpoint", &capabilities)?;
            return Ok(RestoredProjection::Declined);
        }
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
        Ok(RestoredProjection::Carried(anchor))
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
                .context_events_shared()?
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
        let mut metadata: c::Metadata = serde_json::from_value(self.context.model_context.clone())
            .map_err(|e| e.to_string())?;
        // A node's overlay narrows the frozen Chat policy for this context only.
        if let Some(overlay) = agent.and_then(|agent| agent.compaction.as_ref()) {
            overlay.apply(&mut metadata.policy);
        }
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
        // The preparation chain below emits no events while it runs, so a turn
        // that stalls here is invisible in the store. Record its phase timings so
        // the cost can be attributed instead of inferred.
        let prepare_started = std::time::Instant::now();
        let mut timings: Vec<(&'static str, u128)> = Vec::new();
        let mark = |name: &'static str,
                    since: &mut std::time::Instant,
                    timings: &mut Vec<(&'static str, u128)>| {
            timings.push((name, since.elapsed().as_millis()));
            *since = std::time::Instant::now();
        };
        // A process interrupted between start and end never committed a partial
        // replacement. Close its maintenance marker before another attempt.
        let lifecycle = self.run_events.context_events_shared()?;
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
        let mut since = prepare_started;
        mark("lifecycle", &mut since, &mut timings);
        self.register_compression_scope(&owner, outer, request)?;
        mark("compression-scope", &mut since, &mut timings);
        let mut outcome = c::Preparation {
            durable: true,
            max_overflow_retries: metadata.policy.max_overflow_retries,
            ..Default::default()
        };
        // A declined projection carries none of the recorded exchanges, so every
        // later step that positions context against them (instructions, the
        // checkpoint this turn saves, compaction bookkeeping) uses the cursor the
        // pass really has — the next checkpoint then replaces the projection that
        // could not be represented.
        let mut through = through;
        let generated_state = |request: &ModelToolRequestV1| {
            request
                .context_messages
                .iter()
                .filter(|message| c::is_generated_state(&message.content))
                .count()
        };
        let state_before_restore = generated_state(request);
        let anchor = if !restore {
            None
        } else if trigger == c::Trigger::ContextOverflow {
            self.context_snapshot(&owner)?.and_then(|(_, s)| s.anchor)
        } else {
            match self.restore_context(&owner, outer, through, request, agent.is_some())? {
                RestoredProjection::Carried(anchor) => anchor,
                RestoredProjection::Declined => {
                    through = request.exchanges.len();
                    None
                }
            }
        };
        // A restored checkpoint already carries the state block of the pass that
        // wrote it, and this turn's own goal injection sits beside it. Refresh
        // the block from its records so the model reads one current copy instead
        // of the same state twice, once of them stale.
        if restore && generated_state(request) > state_before_restore {
            self.state_context(request)
                .map_err(|error| error.to_string())?;
        }
        mark("restore", &mut since, &mut timings);
        if let Some(agent) = agent {
            self.workspace_context(outer, through, agent, request, cancellation)?;
        }
        mark("instructions", &mut since, &mut timings);
        if cancellation.is_cancelled() {
            return Err("Context preparation cancelled".into());
        }
        let bytes = serde_json::to_vec(request)
            .map_err(|e| e.to_string())?
            .len();
        let pressure = c::pressure(request, anchor.as_ref())?;
        mark("pressure", &mut since, &mut timings);
        // Every budget is a fraction of the window the provider leaves for
        // input, so the model's own output reservation comes out first.
        let window = metadata.context_window.map(|capacity| {
            metadata
                .policy
                .effective_window(capacity, metadata.max_output_tokens)
        });
        // Automatic compaction always runs when a window is declared. A window
        // too small for the target is reported once and still compacts with the
        // minimum replacement, because a run that ignores its own declared
        // window is worse than one that compacts badly; only a fixed context
        // that already reaches the trigger disables it, and there is then
        // genuinely nothing to gain. A Chat that declares no window keeps the
        // legacy 512 KiB byte guard: there is no ratio to judge, and the guard
        // is model-free.
        let fixed = c::fixed_tokens(request).unwrap_or(u64::MAX);
        let automatic = metadata.policy.auto
            && match window {
                None => true,
                Some(_) if fixed == u64::MAX => false,
                Some(window) => {
                    let advisory = metadata.policy.compaction_advisory(window, fixed);
                    let _ = self.run_events.context_advisory_once(
                        advisory.as_deref(),
                        json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child}),
                    );
                    metadata.policy.can_reduce(window, fixed)
                }
            };
        // What one compaction replaces, from the same measured fixed context.
        let replacement = window.map(|window| metadata.policy.replacement_plan(window, fixed));
        // Align the byte-pressure trigger with the token threshold so a token
        // count below the configured 80% threshold cannot trip byte pressure
        // first. JSON context averages about four bytes per token; without a
        // configured context window the legacy 512 KiB floor applies.
        let byte_limit = window
            .map(|window| metadata.policy.threshold(window).saturating_mul(4) as usize)
            .unwrap_or(512 * 1024);
        let byte_pressure = bytes > byte_limit;
        let qualifies = trigger != c::Trigger::Pressure
            || (automatic
                && (byte_pressure
                    || window.is_some_and(|window| pressure >= metadata.policy.threshold(window))));
        if metadata.policy.auto
            && metadata.context_window.is_none()
            && trigger == c::Trigger::Pressure
            && self.context_snapshot(&owner)?.is_none()
        {
            self.run_events.context_advisory_once(Some("Automatic token-pressure compaction needs this model's context window in Settings. Provider overflow recovery and durable byte-pressure compaction remain available."), json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child}))?;
        }
        if qualifies {
            let effective_trigger = if byte_pressure && trigger == c::Trigger::Pressure {
                c::Trigger::BytePressure
            } else {
                trigger
            };
            // The tail a compaction retains, and with it the boundary between
            // results the model is still working with and results old enough to
            // reduce. Pruning and summarising must agree on it.
            let retain = if matches!(
                effective_trigger,
                c::Trigger::Manual | c::Trigger::ContextOverflow | c::Trigger::BytePressure
            ) {
                0
            } else {
                replacement.map_or_else(
                    || metadata.policy.retention(window.unwrap_or_default()),
                    |plan| plan.retain,
                )
            };
            if metadata.policy.prune_tool_results && trigger != c::Trigger::Manual {
                let before = request.clone();
                let surface = c::units(request)?;
                let retained = match c::select_prefix(&surface, retain) {
                    // No prefix can be shadowed, so every result is inside the
                    // retained tail and none of them is a pruning candidate.
                    None => request.exchanges.len(),
                    Some(cut) => surface[cut..]
                        .iter()
                        .filter(|unit| matches!(unit, c::Unit::Exchange(_)))
                        .count()
                        .max(1),
                }
                .min(request.exchanges.len());
                let pruned = c::prune(request, &metadata.policy, retained);
                if !pruned.is_empty() {
                    let removed_chars: usize =
                        pruned.iter().map(|p| p.chars_before - p.chars_after).sum();
                    let removed_tokens: u64 = pruned
                        .iter()
                        .map(|p| p.tokens_before - p.tokens_after)
                        .sum();
                    let checkpoint =
                        self.snapshot_payload(&owner, outer, through, request, anchor.clone())?;
                    if cancellation.is_cancelled() {
                        return Err("Context compaction cancelled".into());
                    }
                    self.run_events.context_batch(vec![("context.compacted", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"trigger":effective_trigger,"strategy":"tool-result-pruning","beforeHash":c::hash(&before),"afterHash":c::hash(request),"retainedExchanges":retained,"removedChars":removed_chars,"removedTokens":removed_tokens,"pruned":pruned,"document":ContextDocument::from_request(request),"body":"Large old tool results compacted; original results remain in Run details."})),("context.checkpoint",checkpoint)])?;
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
                let under_tokens = window.is_none_or(|window| {
                    c::pressure(request, anchor.as_ref()).unwrap_or(u64::MAX)
                        < metadata.policy.threshold(window)
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
                let Some(cut) = c::select_prefix(&surface, retain) else {
                    break;
                };
                let before_hash = c::hash(request);
                let source_generation = self.selection_generation(&owner)?;
                let id = format!(
                    "compact.{}.{}",
                    self.context.run_id,
                    self.run_events
                        .context_events_shared()?
                        .last()
                        .map_or(1, |e| e.sequence + 1)
                );
                // The summary budget is derived from the window's target, never
                // configured, and capped so a summary stays a summary. It is
                // also never more than half the span it replaces, so a committed
                // compaction always halves what it shadows. Without a declared
                // window the span is the only bound left.
                let shadowed_tokens: u64 = surface[..cut].iter().map(c::Unit::tokens).sum();
                let summary_output_cap = replacement
                    .map_or(shadowed_tokens / 2, |plan| plan.summary)
                    .min(shadowed_tokens / 2);
                let mut summary_surface = surface[..cut].to_vec();
                summary_surface.push(c::Unit::Message(ModelToolContextV1 {
                    content: c::INSTRUCTION.trim_end().into(),
                    ..Default::default()
                }));
                let stream = self.run_events.context_events_shared()?;
                let source_events: Vec<_> = stream
                    .iter()
                    .filter(|e| {
                        e.sequence == source_generation
                            || (matches!(e.kind.as_str(), "message.user" | "message.assistant")
                                && e.sequence > source_generation)
                    })
                    .map(|e| e.event_id.clone())
                    .collect();
                let start = self.run_events.context_event("context.compaction-started", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"trigger":effective_trigger,"beforeHash":before_hash,"sourceGeneration":source_generation,"sourceEventIds":source_events,"outer":outer,"through":through,"sourceUnits":surface[..cut].iter().map(c::hash).collect::<Vec<_>>(),"sourceInstructionIds":surface[..cut].iter().filter_map(|u|match u{c::Unit::Message(m)=>m.instruction_event_id.as_ref(),_=>None}).collect::<Vec<_>>(),"body":"Compacting context…"}))?;
                let mut summary_plan = plan.clone();
                // Auxiliary work consumes the selected source before reduction,
                // so the acting node's smaller admission limit cannot price it.
                summary_plan.maximum_input_bytes =
                    crate::runtime::context_inspection::MAX_CONTEXT_BYTES;
                summary_plan.maximum_output_bytes = usize::MAX;
                if let Some(target) = &metadata.summary_target {
                    summary_plan.candidates = vec![aworkit_capability_host::ModelCandidateV1 {
                        binding_id: c::SUMMARY_BINDING.into(),
                        version_hash: c::hash(target),
                    }];
                }
                // A summary the auxiliary call could not produce is retried
                // against a smaller selected input before the failure becomes
                // terminal: the large tool results inside the prompt are reduced
                // first, then the oldest unit the prompt can spare is dropped.
                // The shadowed span is unchanged, so the balanced boundaries, the
                // framed-summary shrink check and the one-terminal-event contract
                // still apply to whatever is committed, and every original stays
                // durably retrievable. The budget is per attempt, so progress
                // resets it.
                let mut shrunk = 0u32;
                let (result, auxiliary, provider_error) = loop {
                    let summary_attempt = shrunk + 1;
                    let mut summary_request = request.clone();
                    c::replace_units(&mut summary_request, &summary_surface)?;
                    summary_request.retry_notice = None;
                    if let Some(target) = &metadata.summary_target {
                        summary_request.parameters = target.model.parameters.clone();
                    }
                    summary_request
                        .parameters
                        .insert("maxOutputTokens".into(), json!(summary_output_cap));
                    let capture = SummaryCapture::default();
                    let provider = gateway.execute_compaction_cancellable(
                        &summary_plan,
                        &summary_request,
                        cancellation,
                        &capture,
                    );
                    let raw = capture.0.into_inner().unwrap_or_else(|p| p.into_inner());
                    let output = project_model_tool_events(&raw);
                    outcome.input_tokens = outcome.input_tokens.saturating_add(output.input_tokens);
                    outcome.output_tokens =
                        outcome.output_tokens.saturating_add(output.output_tokens);
                    let auxiliary = json!({"selectedBinding":provider.as_ref().ok().map(|e|&e.selected_binding),"maxTokens":summary_output_cap,"attempt":summary_attempt,"rawOutput":raw,"inputTokens":output.input_tokens,"outputTokens":output.output_tokens,"cache":output.cache});
                    // Keep the provider's own verdict typed: the Agent loop reports
                    // it to the model, while an internal condition stays a real
                    // authority failure.
                    let mut provider_error = None;
                    let result = provider.map_err(|error| {
                        let message = error.to_string();
                        provider_error = Some(error);
                        message
                    }).and_then(|_evidence| {
                        if cancellation.is_cancelled() { return Err("Context compaction cancelled".into()); }
                        // Match Harness text projection. Auxiliary tool requests
                        // remain raw evidence only; this path has no tool dispatcher.
                        if output.assistant_text.trim().is_empty() { return Err("Compaction produced no text summary".into()); }
                        let summary = c::Unit::Message(ModelToolContextV1 { content:c::frame_summary(output.assistant_text.trim()), ..Default::default() });
                        let tail = &surface[cut..];
                        // Prefer a replacement that carries the user's own turns
                        // across the boundary rather than only summarising them, so
                        // direction outranks tool spam of the same size. When that
                        // cannot shrink the span — one oversized direction with
                        // little other content — fall back to the plain summary so
                        // compaction still makes progress.
                        let user_budget = replacement
                            .map_or(shadowed_tokens / 2, |plan| plan.retain / 2);
                        let pinned = c::pinned_user_units(&surface, cut, user_budget);
                        let pinned_units = pinned.len();
                        let pinned_tokens: u64 = pinned.iter().map(c::Unit::tokens).sum();
                        let mut carried = pinned;
                        carried.push(summary.clone());
                        carried.extend_from_slice(tail);
                        let carried_tokens: u64 = carried.iter().map(c::Unit::tokens).sum();
                        let (next, pinned_units, pinned_tokens) = if carried_tokens < shadowed_tokens {
                            (carried, pinned_units, pinned_tokens)
                        } else {
                            let mut plain = vec![summary];
                            plain.extend_from_slice(tail);
                            (plain, 0, 0)
                        };
                        let kept_tokens: u64 = next.iter().map(c::Unit::tokens).sum();
                        if kept_tokens >= shadowed_tokens { return Err("Compaction replacement is not smaller than the selected history including checkpoint framing".into()); }
                        if c::hash(request) != before_hash || self.selection_generation(&owner)? != source_generation { return Err("Context changed during compaction".into()); }
                        let mut replacement = request.clone();
                        c::replace_units(&mut replacement,&next)?;
                        // The summary is a projection; the Run's goal, task list and
                        // touched files are re-derived from their records and appended
                        // to the replacement, so they are part of the checkpoint too.
                        self.state_context(&mut replacement).map_err(|error| error.to_string())?;
                        let checkpoint=self.snapshot_payload(&owner,outer,through,&replacement,anchor.clone())?;
                        if cancellation.is_cancelled() { return Err("Context compaction cancelled".into()); }
                        self.run_events.context_batch(vec![("context.compacted", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"startSequence":start.sequence,"trigger":effective_trigger,"strategy":"summary","beforeHash":before_hash,"afterHash":c::hash(&replacement),"shadowedUnits":cut,"shadowedTokenCount":shadowed_tokens,"pinnedUnits":pinned_units,"pinnedTokenCount":pinned_tokens,"summaryAttempts":summary_attempt,"document":ContextDocument::from_request(&replacement),"auxiliary":auxiliary,"body":"Context compacted. Earlier history remains available in this Chat."})),("context.checkpoint",checkpoint)])?;
                        *request = replacement;
                        Ok(())
                    });
                    if result.is_err()
                        && !cancellation.is_cancelled()
                        && shrunk < c::SUMMARY_SHRINK_RETRIES
                    {
                        // Bulk leaves the prompt before a whole unit does: a
                        // reduced tool result keeps the unit's position, so the
                        // prompt keeps its shape while the output the summariser
                        // must produce shrinks. A Chat that disabled result
                        // pruning keeps that choice here too.
                        let smaller = if shrunk == 0
                            && metadata.policy.prune_tool_results
                            && !c::prune(&mut summary_request, &metadata.policy, 0).is_empty()
                        {
                            summary_surface = c::units(&summary_request)?;
                            true
                        } else {
                            c::shrink_summary(&mut summary_surface)
                        };
                        if smaller {
                            shrunk += 1;
                            continue;
                        }
                    }
                    break (result, auxiliary, provider_error);
                };
                self.run_events.context_event("context.compaction-ended", json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"compactionId":id,"error":result.as_ref().err(),"auxiliary":auxiliary}))?;
                match result {
                    Ok(()) => outcome.changed = true,
                    Err(error) => {
                        if cancellation.is_cancelled() || trigger == c::Trigger::Manual {
                            match provider_error {
                                // An explicit "compact now" command is the user's
                                // own action with no model turn waiting, so the
                                // provider's diagnostic is reported as its failure.
                                Some(provider) if trigger == c::Trigger::Manual => {
                                    outcome.error = Some(provider.to_string());
                                }
                                // Automatic compaction that the provider refused
                                // keeps the selection unchanged and tells the model.
                                Some(provider) => outcome.provider_error = Some(provider),
                                None => outcome.error = Some(error),
                            }
                            return Ok(outcome);
                        }
                        if provider_error.is_some() {
                            outcome.provider_error = provider_error;
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
            && window.is_some_and(|window| {
                c::pressure(request, anchor.as_ref()).unwrap_or(u64::MAX)
                    >= metadata.policy.threshold(window)
            })
        {
            self.run_events.context_event("context.compaction-warning",json!({"ownerKey":self.context_key(),"nodeId":owner.node_id,"child":owner.child,"body":"Context is still above the configured pressure threshold after the permitted reductions. The latest context is preserved; provider overflow recovery remains bounded by the configured retry policy."}))?;
        }
        mark("manage", &mut since, &mut timings);
        if cancellation.is_cancelled() {
            return Err("Context preparation cancelled".into());
        }
        self.save_context(&owner, outer, through, request, anchor)?;
        mark("checkpoint", &mut since, &mut timings);
        self.run_events.context_event(
            "context.prepare-timing",
            json!({
                "ownerKey":self.context_key(),
                "nodeId":owner.node_id,
                "child":owner.child,
                "trigger":format!("{trigger:?}"),
                "changed":outcome.changed,
                "totalMs":prepare_started.elapsed().as_millis(),
                "phases":timings.iter().map(|(name,ms)|json!({"phase":name,"ms":ms})).collect::<Vec<_>>(),
            }),
        )?;
        Ok(outcome)
    }
}
