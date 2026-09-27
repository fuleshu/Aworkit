use super::*;
use crate::runtime::compaction::{self as c, GOAL_STATE_LABEL, TASK_STATE_LABEL, Trigger};
use aworkit_capability_host::{
    FrozenModelGateway, ModelCandidateV1, ModelEventV1, ModelRequestV1, ModelResolutionPlanV1,
    ModelToolContextV1, ModelToolEventV1, ProviderAcceptanceV1, ProviderEnginePortV1,
    ProviderError,
};

struct Summarizer {
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    emit_tool_call: bool,
    respond: Box<dyn Fn(&CancellationToken) -> Result<String, ProviderError> + Send + Sync>,
}
impl ProviderEnginePortV1 for Summarizer {
    fn binding_id(&self) -> &str {
        "model.compaction-test"
    }
    fn version_hash(&self) -> &str {
        "v1"
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        _: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        unreachable!()
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let text = (self.respond)(cancellation)?;
        if self.emit_tool_call {
            let tool = &request.tools[0];
            emit(ModelToolEventV1::ToolCall {
                call: ModelToolCallV1 {
                    call_id: "auxiliary.only".into(),
                    provider_call_id: Some("auxiliary.only".into()),
                    capability_id: tool.capability_id.clone(),
                    name: tool.name.clone(),
                    arguments: json!({"path":"src/file.txt"}),
                    provider_context: None,
                },
            })?;
        }
        emit(ModelToolEventV1::AssistantOutput { text })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 7000,
            output_tokens: 50,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}
fn gateway(
    respond: impl Fn(&CancellationToken) -> Result<String, ProviderError> + Send + Sync + 'static,
) -> (
    FrozenModelGateway,
    Arc<Mutex<Vec<ModelToolRequestV1>>>,
    ModelResolutionPlanV1,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let gateway = FrozenModelGateway::new(vec![Box::new(Summarizer {
        requests: requests.clone(),
        emit_tool_call: false,
        respond: Box::new(respond),
    })]);
    (
        gateway,
        requests,
        ModelResolutionPlanV1 {
            candidates: vec![ModelCandidateV1 {
                binding_id: "model.compaction-test".into(),
                version_hash: "v1".into(),
            }],
            maximum_input_bytes: 768 * 1024,
            maximum_output_bytes: 16 * 1024,
        },
    )
}

#[test]
fn new_turn_context_preserves_prefix_and_is_admitted_once_after_reopen() {
    for callable in [true, false] {
        let mut f = Fixture::with_tools(if callable { &[FILE_READ_CAPABILITY_ID] } else { &[] });
        f.authority.context.model_context = json!({"policy":{"auto":false}});
        f.committer.commit(vec![SemanticEventDraft::new("message.user", json!({"body":"first"}))]).unwrap();
        let (gateway, calls, plan) = gateway(|_| panic!("No auxiliary call needed"));
        let prepare = |f: &Fixture, outer: &str, label: &str| {
            let mut request = f.request();
            request.input["messages"].as_array_mut().unwrap().insert(0,
                json!({"role":"system","content":"Frozen agent instructions"}));
            request.context_messages.push(ModelToolContextV1 {
                content: label.into(), role: Some("user".into()), ..Default::default()
            });
            f.authority.manage_model_context(&gateway, &plan, &stable(outer).unwrap(), 0,
                Some(&f.agent), &mut request, &CancellationToken::default(), Trigger::Pressure).unwrap();
            request
        };
        let first = prepare(&f, "outer.first", "PLAN ONE");
        assert_eq!(c::units(&first).unwrap(), c::units(&prepare(&f, "outer.first", "PLAN ONE")).unwrap());
        f.committer.commit(vec![
            SemanticEventDraft::new("message.assistant", json!({"body":"First answer"})),
            SemanticEventDraft::new("message.user", json!({"body":"New user request"})),
        ]).unwrap();
        f.authority.runtime.records = Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
        let second = prepare(&f, "outer.second", "PLAN TWO");
        let prefix = c::units(&first).unwrap();
        let units = c::units(&second).unwrap();
        assert_eq!(second.input, first.input, "Frozen header and base history stay byte-stable");
        assert_eq!(&units[..prefix.len()], prefix.as_slice());
        let tail = serde_json::to_string(&units[prefix.len()..]).unwrap();
        assert!(tail.find("New user request").unwrap() < tail.find("PLAN TWO").unwrap());
        assert_eq!(serde_json::to_string(&units).unwrap().matches("PLAN ONE").count(), 1);
        assert_eq!(tail.matches("PLAN TWO").count(), 1);
        let replay = prepare(&f, "outer.second", "PLAN TWO");
        assert_eq!(units, c::units(&replay).unwrap());
        for keep_plan in [true, false] {
            let mut edited = second.clone();
            if !keep_plan {
                edited.context_messages.retain(|m| m.content != "PLAN TWO");
            }
            f.committer.commit(vec![SemanticEventDraft::new("context.edited", json!({
                "nodeId": f.agent.node_id,
                "document": crate::runtime::context_inspection::ContextDocument::from_request(&edited)
            }))]).unwrap();
            let resumed = prepare(&f, "outer.second", "PLAN TWO");
            assert_eq!(c::units(&edited).unwrap(), c::units(&resumed).unwrap(),
                "Edits neither duplicate nor resurrect admitted graph context");
        }
        assert!(calls.lock().unwrap().is_empty());
    }
}

/// A compaction that produced no reduction commits no reduction.
///
/// The request itself is not a valid witness: preparation injects the trusted
/// host clock and any newly reconciled workspace instruction for the current
/// turn, so a hash taken before `manage_model_context` legitimately changes even
/// when compaction does nothing. The contract is therefore asserted through the
/// committed evidence.
fn assert_no_reduction(f: &Fixture, changed: bool, context: &str) {
    assert!(!changed, "{context}: a failed compaction must not reduce");
    assert!(
        !f.committer
            .committed_events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "context.compacted"),
        "{context}: a failed compaction must commit no reduction"
    );
}

fn history(f: &mut Fixture) -> (StableId, ModelToolRequestV1) {
    f.authority.context.review_messages[0].content =
        "Established requirements and implementation history. ".repeat(1800);
    f.authority.context.model_context = json!({"contextWindow":16000,"policy":{"auto":false,"retainTokens":0,"pruneToolResults":false}});
    let outer = stable("outer.compaction-integration").unwrap();
    let mut request = f.request();
    f.prepare(outer.as_str(), 0, &mut request);
    let call = ModelToolCallV1 {
        call_id: "read.compaction".into(),
        provider_call_id: Some("read.compaction".into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":"src/file.txt"}),
        provider_context: None,
    };
    let settled = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    let exchange = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall {
            call,
        }],
        results: vec![settled.result],
    };
    f.authority.commit_exchange(&outer, 1, &exchange).unwrap();
    request.exchanges.push(exchange);
    f.prepare(outer.as_str(), 1, &mut request);
    assert!(
        request
            .context_messages
            .iter()
            .any(|m| m.content.contains("NESTED V1"))
    );
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: 1,
        content: "Continue with the next task.".into(),
        ..Default::default()
    });
    (outer, request)
}

#[test]
fn automatic_pressure_repeats_a_valid_reduction_and_stops_once_under_threshold() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let window = 65_536_u64;
    f.authority.context.model_context["contextWindow"] = json!(window);
    f.authority.context.model_context["policy"]["auto"] = json!(true);
    // Enough content to cross the trigger, and a first summary that is smaller
    // than what it replaces but still leaves the context above the trigger, so
    // the loop must reduce a second time.
    request.input["messages"][0]["content"] =
        json!("Established requirements and implementation history. ".repeat(6000));
    let threshold = c::Policy::default().threshold(window);
    let long_summary = "Interim ".repeat(((threshold + 4000) / 2) as usize);
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = counter.clone();
    let (gateway, calls, plan) = gateway(move |_| {
        Ok(
            if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                long_summary.clone()
            } else {
                "Established requirements and decisions.".into()
            },
        )
    });
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.changed && prepared.error.is_none());
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(prepared.input_tokens, 14000);
    assert!(c::estimate(&request).unwrap() < threshold);
    let mut next = f.request();
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(c::estimate(&next).unwrap() < threshold);
}

#[test]
fn a_window_whose_target_is_out_of_reach_still_compacts_and_reports_once() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let window = 32_768_u64;
    f.authority.context.model_context["contextWindow"] = json!(window);
    f.authority.context.model_context["policy"]["auto"] = json!(true);
    let policy = c::Policy::default();
    // The recorded 32k configuration: a fixed context (instructions and tool
    // schemas) between the 8,192-token target and the 26,214-token trigger, so
    // one compaction still buys headroom even though the 25% target is out of
    // reach. This used to refuse and warn on every turn instead of compacting.
    request.input["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "system", "content": "s".repeat(49_616)}));
    let fixed = c::fixed_tokens(&request).unwrap();
    assert!(
        fixed > policy.target(window) && fixed < policy.threshold(window),
        "the fixture must sit between the target and the trigger: {fixed}"
    );
    assert!(
        policy.compaction_advisory(window, fixed).is_some(),
        "the target is out of reach at {fixed} fixed tokens"
    );
    assert!(
        policy.can_reduce(window, fixed),
        "but the window can still be reduced at {fixed} fixed tokens"
    );
    // Cross the 80% trigger with shadowed history, so the token threshold is
    // what fires rather than the legacy byte guard.
    let threshold = policy.threshold(window);
    let shortfall = threshold.saturating_sub(c::estimate(&request).unwrap());
    request.input["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "role": "user",
            "content": "x".repeat(((shortfall + 2_000) * 4) as usize),
        }));
    assert!(c::estimate(&request).unwrap() >= threshold);
    let (gateway, calls, plan) = gateway(|_| Ok("Prior requirements and decisions.".into()));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(
        prepared.changed && prepared.error.is_none(),
        "a declared window is honoured even when the target is out of reach"
    );
    assert_eq!(calls.lock().unwrap().len(), 1, "the summary is paid for");
    assert!(
        c::estimate(&request).unwrap() < threshold,
        "one clamped compaction still leaves the context under the trigger"
    );
    let advisories = |f: &Fixture| {
        f.committer
            .committed_events()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "context.compaction-warning")
            .filter_map(|e| e.payload["body"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let bodies = advisories(&f);
    assert_eq!(bodies.len(), 1, "one advisory, not 47: {bodies:?}");
    let minimum = policy.minimum_window(fixed).to_string();
    assert!(
        bodies[0].contains(&minimum) && bodies[0].contains("still runs"),
        "the advisory derives the {minimum}-token window and says compaction runs anyway: {bodies:?}"
    );
    // The same standing condition is not repeated on later turns, and a turn
    // that no longer crosses the trigger does not clear it either.
    let mut next = f.request();
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert_eq!(advisories(&f).len(), 1, "the condition is reported once");
}

#[test]
fn auxiliary_projects_text_and_preserves_tool_requests_as_evidence_without_dispatch() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let (_, calls, plan) = gateway(|_| Ok(String::new()));
    let gateway = FrozenModelGateway::new(vec![Box::new(Summarizer {
        requests: calls,
        emit_tool_call: true,
        respond: Box::new(|_| Ok("Prior requirements and decisions.".into())),
    })]);
    let before = f
        .authority
        .runtime
        .records
        .events("pipeline.tool-invocation-prepared")
        .unwrap()
        .len();
    let result = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(result.changed && result.error.is_none());
    assert_eq!(
        f.authority
            .runtime
            .records
            .events("pipeline.tool-invocation-prepared")
            .unwrap()
            .len(),
        before
    );
    assert!(f.committer.committed_events().unwrap().iter().any(|e| {
        e.kind == "context.compaction-ended"
            && e.payload["auxiliary"]["rawOutput"]
                .to_string()
                .contains("auxiliary.only")
    }));
}

#[test]
fn actual_compaction_restores_current_root_and_nested_rules_and_survives_reopen() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let initial_refs: Vec<_> = request
        .context_messages
        .iter()
        .filter_map(|m| m.instruction_event_id.clone())
        .collect();
    let workspace = f.authority.context.workspace.root.clone();
    let (gateway, requests, plan) = gateway(move |_| {
        std::fs::write(workspace.join("AGENTS.md"), "ROOT V2").unwrap();
        std::fs::write(workspace.join("src/AGENTS.md"), "NESTED V2").unwrap();
        Ok("The requirements were established; continue the next task.".into())
    });
    let result = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(result.changed);
    assert_eq!((result.input_tokens, result.output_tokens), (7000, 50));
    let summary_requests = requests.lock().unwrap();
    assert_eq!(summary_requests.len(), 1);
    assert_eq!(
        summary_requests[0].context_messages.last().unwrap().content,
        c::INSTRUCTION.trim_end()
    );
    // The summary budget is derived from what the window targets, never more
    // than half the span it replaces: a 16k window targets 4,000 tokens, and
    // the summary takes 38.2% of what the tail minimum leaves of that.
    let fixed = c::fixed_tokens(&summary_requests[0]).unwrap();
    assert_eq!(
        summary_requests[0].parameters["maxOutputTokens"],
        json!(
            c::Policy::default()
                .replacement_plan(16_000, fixed, None)
                .summary
        )
    );
    drop(summary_requests);
    let visible = c::units(&request).unwrap();
    let text = serde_json::to_string(&visible).unwrap();
    assert!(text.contains("<compacted-summary>"));
    assert!(
        text.contains("ROOT V2") && text.contains("NESTED V2"),
        "{text}"
    );
    assert!(!text.contains("ROOT V1") && !text.contains("NESTED V1"));
    assert!(
        request
            .context_messages
            .iter()
            .filter_map(|m| m.instruction_event_id.as_ref())
            .all(|id| !initial_refs.contains(id))
    );
    assert!(
        request.exchanges.is_empty(),
        "closed tools were replaced, never replayed"
    );
    let events = f.committer.committed_events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-started")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-ended")
            .count(),
        1
    );
    assert_eq!(
        f.authority
            .runtime
            .records
            .events("pipeline.model-tool-exchange")
            .unwrap()
            .len(),
        1
    );
    f.authority.runtime.records =
        Arc::new(ToolRecordStore::open(&f.root.path().join("events.sqlite3")).unwrap());
    let mut replay = f.request();
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut replay,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert_eq!(c::units(&request).unwrap(), c::units(&replay).unwrap());
    assert_eq!(requests.lock().unwrap().len(), 1);
}

/// Prompt content of one surface unit, independent of position bookkeeping.
/// The provider sees exactly this sequence, so comparing it proves the summary
/// prompt is the live prompt's prefix rather than a reshaped copy.
fn prompt_shape(unit: &c::Unit) -> Value {
    match unit {
        c::Unit::Message(message) => json!({
            "kind": "message",
            "role": message.role,
            "content": message.content,
            "images": message.images,
            "instructionEventId": message.instruction_event_id,
        }),
        c::Unit::Exchange(exchange) => json!({"kind": "exchange", "exchange": exchange}),
    }
}

/// Runs one manual compaction through the real authority.
fn compact_manual(
    f: &Fixture,
    gateway: &FrozenModelGateway,
    plan: &ModelResolutionPlanV1,
    outer: &StableId,
    request: &mut ModelToolRequestV1,
) -> c::Preparation {
    f.authority
        .manage_model_context(
            gateway,
            plan,
            outer,
            1,
            Some(&f.agent),
            request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap()
}

/// One closed tool exchange with a named call and an arbitrary result payload.
fn tool_exchange(call_id: &str, content: Value) -> aworkit_capability_host::ModelToolExchangeV1 {
    aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall {
            call: ModelToolCallV1 {
                call_id: call_id.into(),
                provider_call_id: Some(call_id.into()),
                capability_id: FILE_READ_CAPABILITY_ID.into(),
                name: FILE_READ_PROVIDER_NAME.into(),
                arguments: json!({}),
                provider_context: None,
            },
        }],
        results: vec![aworkit_capability_host::ModelToolResultV1 {
            images: Vec::new(),
            call_id: call_id.into(),
            content,
            is_error: false,
        }],
    }
}

/// Generated state messages that mention `text`, in request order.
fn generated_state(request: &ModelToolRequestV1, text: &str) -> Vec<ModelToolContextV1> {
    request
        .context_messages
        .iter()
        .filter(|message| c::is_generated_state(&message.content) && message.content.contains(text))
        .cloned()
        .collect()
}

#[test]
fn pruning_old_tool_results_clears_the_trigger_without_a_summary() {
    // The gate is retuned so old, large results are reduced before an auxiliary
    // summary is paid for: here one huge old result is the whole reason the
    // context is over the trigger, and pruning alone brings it back under.
    // No instruction binding, so nothing but the prune changes the request and
    // the pressure drop is exactly the recorded reduction.
    let mut f = Fixture::with_tools(&[FILE_READ_CAPABILITY_ID]);
    let window = 65_536_u64;
    f.authority.context.model_context = json!({
        "contextWindow": window,
        "policy": {"auto": true, "retainTokens": 0, "pruneToolResults": true}
    });
    let outer = stable("outer.prune-only").unwrap();
    let mut request = f.request();
    f.prepare(outer.as_str(), 0, &mut request);
    request
        .exchanges
        .push(tool_exchange("read.big", json!("z".repeat(300_000))));
    request
        .exchanges
        .push(tool_exchange("read.recent", json!("recent result")));
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: 2,
        content: "Continue with the next task.".into(),
        ..Default::default()
    });
    let threshold = c::Policy::default().threshold(window);
    let before = c::pressure(&request, None).unwrap();
    assert!(before >= threshold, "{before} >= {threshold}");
    let (gateway, calls, plan) = gateway(|_| panic!("pruning alone must not need a summary"));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            2,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(
        prepared.changed && prepared.error.is_none(),
        "{:?}",
        prepared.error
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "no auxiliary summary call"
    );
    let after = c::pressure(&request, None).unwrap();
    assert!(after < threshold, "{after} < {threshold}");
    assert!(
        request.exchanges[0].results[0]
            .content
            .as_str()
            .unwrap()
            .len()
            < 300_000
    );
    assert_eq!(
        request.exchanges[1].results[0].content,
        json!("recent result"),
        "the newest result is not a candidate"
    );
    let events = f.committer.committed_events().unwrap();
    let compacted: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "context.compacted")
        .collect();
    assert_eq!(compacted.len(), 1, "exactly one reduction was committed");
    assert_eq!(compacted[0].payload["strategy"], "tool-result-pruning");
    assert_eq!(compacted[0].payload["retainedExchanges"], 1);
    let pruned = compacted[0].payload["pruned"].as_array().unwrap();
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["callId"], "read.big");
    let removed: u64 = pruned
        .iter()
        .map(|entry| {
            entry["tokensBefore"].as_u64().unwrap() - entry["tokensAfter"].as_u64().unwrap()
        })
        .sum();
    assert_eq!(
        compacted[0].payload["removedTokens"].as_u64(),
        Some(removed)
    );
    assert!(removed > 0);
    assert_eq!(
        before - after,
        removed,
        "the pressure drop is exactly the recorded reduction"
    );
}

#[test]
fn a_pruned_original_stays_retrievable_through_the_context_tool() {
    // Pruning rewrites the request projection only: the durable archive the
    // context tool reads keeps the whole original, so omitted text is still
    // recoverable after a compaction.
    let mut f = Fixture::new();
    let native = crate::runtime::tool_registry::native_tool("tool.context").unwrap();
    let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
        capability_id: "tool.context".into(),
        configuration: json!(native.configuration),
        options: Default::default(),
        credential_bindings: vec![],
        definition: None,
    }])
    .unwrap()
    .remove(0);
    let descriptor = file_tool_descriptors()
        .unwrap()
        .remove("tool.context")
        .unwrap();
    f.authority
        .context
        .manifest
        .capability_bindings
        .push(file_tool_capability_binding(&binding, &descriptor).unwrap());
    f.authority.context.bindings.push(binding);
    f.agent.tool_ids.push("tool.context".into());
    // A host tool-output bound below the read page forces the archive path: the
    // bounded preview still exceeds the prune gate, and the archive keeps the
    // whole original for retrieval.
    f.authority.context.maximum_tool_output_bytes = 40_000;
    f.authority.context.model_context = json!({
        "contextWindow": 65_536,
        "policy": {
            "auto": true,
            "retainTokens": 0,
            "pruneToolResults": true,
            "compression": {"mode": "lossless", "targetRatio": 0.9, "minimumSavings": 0.05}
        }
    });
    let marker = "UNIQUE MIDDLE RECEIPT 7391";
    let source = |index: usize| {
        (0..600)
            .map(|step| {
                format!(
                    "{{\"step\":{step},\"worker\":{},\"detail\":\"observation {step} retained {}\"}}\n",
                    step * 7 + index,
                    step * 13
                )
            })
            .collect::<String>()
    };
    let mut files = Vec::new();
    let mut written = Vec::new();
    for index in 0..6 {
        let mut text = source(index);
        if index == 0 {
            text = text.replacen(
                "observation 300 retained",
                &format!("{marker} observation 300 retained"),
                1,
            );
        }
        let path = format!("src/big_{index}.txt");
        f.write(&path, &text);
        files.push(path);
        written.push(text);
    }
    let outer = stable("outer.retrievable").unwrap();
    f.authority
        .register_compression_scope(&f.agent, &outer, &f.request())
        .unwrap();
    let mut request = f.request();
    let mut reference = Value::Null;
    let mut offset = 0;
    for (turn, path) in files.iter().enumerate() {
        let turn = turn as u32 + 1;
        let call = ModelToolCallV1 {
            call_id: format!("read.{turn}"),
            provider_call_id: Some(format!("read.{turn}")),
            capability_id: FILE_READ_CAPABILITY_ID.into(),
            name: FILE_READ_PROVIDER_NAME.into(),
            arguments: json!({"path": path}),
            provider_context: None,
        };
        let settled = f
            .authority
            .invoke(&outer, turn, &call, &CancellationToken::default())
            .unwrap();
        if turn == 1 {
            reference = settled.result.content["aworkitContext"]["reference"].clone();
            offset = written[0].find(marker).unwrap();
        }
        let exchange = aworkit_capability_host::ModelToolExchangeV1 {
            assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall {
                call,
            }],
            results: vec![settled.result],
        };
        f.authority
            .commit_exchange(&outer, turn, &exchange)
            .unwrap();
        request.exchanges.push(exchange);
    }
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: request.exchanges.len(),
        content: "Continue with the next task.".into(),
        ..Default::default()
    });
    assert!(reference.is_string());
    let newest = request.exchanges[5].results[0].content.clone();
    let threshold = c::Policy::default().threshold(65_536);
    let before = c::pressure(&request, None).unwrap();
    assert!(before >= threshold, "{before} >= {threshold}");
    assert!(
        serde_json::to_string(&request).unwrap().contains(marker),
        "the original text is part of the request projection before pruning"
    );
    let (gateway, calls, plan) = gateway(|_| panic!("pruning alone must not need a summary"));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(
        prepared.changed && prepared.error.is_none(),
        "{:?}",
        prepared.error
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(c::pressure(&request, None).unwrap() < threshold);
    assert!(
        !serde_json::to_string(&request).unwrap().contains(marker),
        "the reduced projection omits the middle of the old result"
    );
    assert_eq!(
        request.exchanges[5].results[0].content, newest,
        "the newest result is not a candidate"
    );
    let events = f.committer.committed_events().unwrap();
    let compacted: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "context.compacted")
        .collect();
    assert_eq!(compacted.len(), 1);
    assert_eq!(compacted[0].payload["strategy"], "tool-result-pruning");
    assert_eq!(compacted[0].payload["pruned"][0]["callId"], "read.1");
    // The archive keeps the whole original, so the omitted text is still
    // retrievable through the context tool.
    let recovered = f
        .authority
        .invoke(
            &outer,
            1,
            &ModelToolCallV1 {
                call_id: "recover.1".into(),
                provider_call_id: Some("recover.1".into()),
                capability_id: "tool.context".into(),
                name: "context".into(),
                arguments: json!({"operation":"read","reference":reference,"pointer":"/content","offset":offset,"limit":256}),
                provider_context: None,
            },
            &CancellationToken::default(),
        )
        .unwrap();
    assert!(!recovered.result.is_error, "{:?}", recovered.result);
    assert!(
        recovered.result.content.to_string().contains(marker),
        "{:?}",
        recovered.result.content
    );
}

#[test]
fn a_compaction_re_emits_the_live_goal_the_task_list_and_touched_files() {
    // A summary is lossy; the state the tools wrote is re-derived and appended
    // to the replacement, replacing any older copy left in the retained tail.
    let mut f = Fixture::with_tools(&[
        ID,
        FILE_READ_CAPABILITY_ID,
        GOAL_CAPABILITY_ID,
        TODO_CAPABILITY_ID,
    ]);
    let (outer, mut request) = history(&mut f);
    let run = f.authority.context.run_id.clone();
    f.authority
        .runtime
        .record_goal_state_for(
            &run,
            &json!({"status":"active","goal":"Ship the canonical state block"}),
        )
        .unwrap();
    f.authority
        .runtime
        .records
        .record_todo_state(
            &run,
            &json!([{"content":"compact safely","status":"in_progress"}]),
        )
        .unwrap();
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: 1,
        content: format!("{GOAL_STATE_LABEL}active; stale copy):\nOld goal"),
        ..Default::default()
    });
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let result = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(result.changed, "{:?}", result.error);
    let state = |request: &ModelToolRequestV1, text: &str| generated_state(request, text);
    let goal = state(&request, "Ship the canonical state block");
    assert_eq!(goal.len(), 1, "exactly the live goal, not a stale copy");
    assert_eq!(
        state(&request, "compact safely").len(),
        1,
        "the task list is restored"
    );
    assert_eq!(
        state(&request, "file.txt").len(),
        1,
        "a file this Run read is named"
    );
    assert!(
        request
            .context_messages
            .iter()
            .all(|message| !message.content.contains("Old goal")),
        "a superseded state copy is replaced rather than stacked"
    );
    assert!(goal[0].content.starts_with(GOAL_STATE_LABEL));
    for message in request
        .context_messages
        .iter()
        .filter(|message| c::is_generated_state(&message.content))
    {
        assert_eq!(
            message.after_exchanges,
            request.exchanges.len(),
            "restored state follows every retained exchange"
        );
        assert_eq!(message.role.as_deref(), Some("user"));
        assert!(message.instruction_event_id.is_none());
    }
    // The checkpoint document carries the state too, so a restore re-reads it.
    let events = f.committer.committed_events().unwrap();
    let compacted = events
        .iter()
        .find(|e| e.kind == "context.compacted")
        .unwrap();
    let document = serde_json::to_string(&compacted.payload["document"]).unwrap();
    assert!(
        document.contains("Ship the canonical state block"),
        "{document}"
    );
    assert!(document.contains("compact safely"), "{document}");
    // A repeated compaction re-derives the same state instead of stacking a
    // second copy, whether or not the second attempt can still shrink.
    let additional = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::Text {
            text: "Working.".into(),
        }],
        results: Vec::new(),
    };
    f.authority.commit_exchange(&outer, 2, &additional).unwrap();
    request.exchanges.push(additional);
    f.write("src/big.txt", &"z".repeat(30_000));
    let call = ModelToolCallV1 {
        call_id: "read.big".into(),
        provider_call_id: Some("read.big".into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":"src/big.txt"}),
        provider_context: None,
    };
    let settled = f
        .authority
        .invoke(&outer, 3, &call, &CancellationToken::default())
        .unwrap();
    let read = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall { call }],
        results: vec![settled.result],
    };
    f.authority.commit_exchange(&outer, 3, &read).unwrap();
    request.exchanges.push(read);
    let second = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(second.changed, "{:?}", second.error);
    assert_eq!(state(&request, "Ship the canonical state block").len(), 1);
    assert_eq!(state(&request, "compact safely").len(), 1);
    assert_eq!(state(&request, "file.txt").len(), 1);
}

#[test]
fn a_cleared_goal_and_an_empty_task_list_are_not_re_emitted() {
    // Generated state is derived, never assumed: nothing recorded means no
    // block, and a cleared goal must not be resurrected from an older copy.
    let mut f = Fixture::with_tools(&[ID, GOAL_CAPABILITY_ID, TODO_CAPABILITY_ID]);
    f.authority.context.review_messages[0].content =
        "Established requirements and implementation history. ".repeat(1800);
    f.authority.context.model_context =
        json!({"contextWindow":16000,"policy":{"auto":false,"retainTokens":0,"pruneToolResults":false}});
    let run = f.authority.context.run_id.clone();
    f.authority
        .runtime
        .record_goal_state_for(&run, &json!({"status":"cleared"}))
        .unwrap();
    f.authority
        .runtime
        .records
        .record_todo_state(&run, &json!([]))
        .unwrap();
    let outer = stable("outer.state-empty").unwrap();
    let mut request = f.request();
    f.prepare(outer.as_str(), 0, &mut request);
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: 0,
        content: "Continue with the next task.".into(),
        ..Default::default()
    });
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let result = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(result.changed, "{:?}", result.error);
    assert!(
        request
            .context_messages
            .iter()
            .all(|message| !c::is_generated_state(&message.content)),
        "no state block when the goal is cleared, the list is empty and no file was touched"
    );
}

#[test]
fn a_restore_refreshes_the_state_block_without_rewriting_the_selection() {
    // The pass after a compaction injects the live goal and restores the
    // checkpoint that also carries the goal. The refresh must deliver the
    // current objective without rewriting the copy already in the prompt: a
    // changed byte mid-selection re-bills every token after it. The restored
    // copy stays where it stood and this pass's own live copy is pushed after
    // it, so the model still reads the current objective last.
    let mut f = Fixture::with_tools(&[
        ID,
        FILE_READ_CAPABILITY_ID,
        GOAL_CAPABILITY_ID,
        TODO_CAPABILITY_ID,
    ]);
    let (outer, mut request) = history(&mut f);
    let run = f.authority.context.run_id.clone();
    f.authority
        .runtime
        .record_goal_state_for(&run, &json!({"status":"active","goal":"First objective"}))
        .unwrap();
    f.authority
        .runtime
        .records
        .record_todo_state(
            &run,
            &json!([{"content":"finish the block","status":"pending"}]),
        )
        .unwrap();
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let first = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(first.changed, "{:?}", first.error);
    assert_eq!(generated_state(&request, "First objective").len(), 1);

    // The user moves the goal on before the next pass begins.
    f.authority
        .runtime
        .record_goal_state_for(&run, &json!({"status":"active","goal":"Second objective"}))
        .unwrap();
    let mut next = f.request();
    next.context_messages.push(ModelToolContextV1 {
        after_exchanges: 0,
        role: Some("user".into()),
        content: format!(
            "{GOAL_STATE_LABEL}active; durable state for this Chat, not a new instruction):\nSecond objective"
        ),
        ..Default::default()
    });
    let second = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &stable("outer.restore-state").unwrap(),
            1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(second.error.is_none(), "{:?}", second.error);
    let goals = generated_state(&next, "objective");
    assert_eq!(goals.len(), 2, "{:?}", next.context_messages);
    assert!(
        goals[0].content.contains("First objective"),
        "the restored copy is left where it stood instead of being rewritten"
    );
    assert!(
        goals.last().unwrap().content.contains("Second objective"),
        "this pass's live goal is the last copy the model reads"
    );
    assert_eq!(
        goals
            .iter()
            .filter(|message| message.content.contains("Second objective"))
            .count(),
        1,
        "a current copy already in the selection is never appended twice"
    );
    assert_eq!(generated_state(&next, "finish the block").len(), 1);
}

#[test]
fn a_state_change_is_appended_at_the_tail_instead_of_rewriting_the_context() {
    // The measured defect: a 60-character state edit rewrote a message seven
    // exchanges deep and re-billed about 50k tokens. A change between
    // compactions must only append, so every unit the previous prompt carried
    // stays byte-identical and the provider's cached prefix survives.
    let mut f = Fixture::with_tools(&[ID, FILE_READ_CAPABILITY_ID, TODO_CAPABILITY_ID]);
    let (outer, mut request) = history(&mut f);
    let run = f.authority.context.run_id.clone();
    f.authority
        .runtime
        .records
        .record_todo_state(
            &run,
            &json!([{"content":"first task","status":"pending"}]),
        )
        .unwrap();
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let compacted = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(compacted.changed, "{:?}", compacted.error);
    let before = c::units(&request).unwrap();
    assert_eq!(
        before
            .iter()
            .filter(|unit| matches!(unit, c::Unit::Message(m) if m.content.starts_with(TASK_STATE_LABEL)))
            .count(),
        1,
        "a compaction consolidates the block into one copy"
    );

    // The Run finishes its first task and starts a second.
    f.authority
        .runtime
        .records
        .record_todo_state(
            &run,
            &json!([
                {"content":"first task","status":"completed"},
                {"content":"second task","status":"pending"}
            ]),
        )
        .unwrap();
    // A window this turn stays well inside, so nothing compacts and only the
    // refresh runs.
    f.authority.context.model_context = json!({"contextWindow":1_000_000,"policy":{"auto":true}});
    let mut next = f.request();
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &stable("outer.state-append").unwrap(),
            1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.error.is_none(), "{:?}", prepared.error);
    assert!(!prepared.changed, "an appended refresh is not a reduction");

    let after = c::units(&next).unwrap();
    assert!(
        after.len() > before.len(),
        "the state change is appended: {} -> {}",
        before.len(),
        after.len()
    );
    assert_eq!(
        &after[..before.len()],
        before.as_slice(),
        "every unit the previous prompt carried stays byte-identical"
    );
    let copies = generated_state(&next, "task");
    assert_eq!(copies.len(), 2, "{:?}", next.context_messages);
    assert!(
        copies[0].content.contains("first task") && copies[0].content.contains("pending"),
        "the copy the previous prompt carried is left where it stood"
    );
    let live = copies.last().unwrap();
    assert!(live.content.contains("second task"));
    assert!(
        live.content.ends_with("(Supersedes the earlier copy of this state in this context.)"),
        "the appended copy says which one wins: {}",
        live.content
    );
    assert!(
        live.after_exchanges == next.exchanges.len(),
        "the appended copy sits after every exchange"
    );

    // A refresh with nothing to say appends nothing, so a quiet turn costs the
    // provider's cache nothing at all.
    let unchanged = c::units(&next).unwrap();
    f.authority
        .state_context(
            &mut next,
            crate::runtime::tool_loop::state_context::StateEmission::Append,
        )
        .unwrap();
    assert_eq!(
        c::units(&next).unwrap(),
        unchanged,
        "an unchanged refresh appends nothing"
    );
}

#[test]
fn a_block_of_superseded_copies_is_consolidated_instead_of_appended_to_again() {
    // Appending keeps the cached prefix, but a superseded copy is never free, so
    // the block is collapsed once those copies pass their budget: one rewrite per
    // budget instead of one per change, and no unbounded growth.
    let mut f = Fixture::with_tools(&[ID, FILE_READ_CAPABILITY_ID, GOAL_CAPABILITY_ID]);
    let (outer, mut request) = history(&mut f);
    let run = f.authority.context.run_id.clone();
    f.authority.context.model_context = json!({"contextWindow":1_000_000,"policy":{"auto":true}});
    f.authority
        .runtime
        .record_goal_state_for(&run, &json!({"status":"active","goal":"live objective"}))
        .unwrap();
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    // A compaction writes the checkpoint whose restored block makes the next
    // refresh run, and it consolidates the block itself.
    let compacted = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(compacted.changed, "{:?}", compacted.error);
    let mut next = f.request();
    for index in 0..60 {
        next.context_messages.push(ModelToolContextV1 {
            content: format!(
                "{GOAL_STATE_LABEL}active; durable state for this Chat, not a new instruction):\nstale objective {index}"
            ),
            role: Some("user".into()),
            ..Default::default()
        });
    }
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &stable("outer.state-budget").unwrap(),
            1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.error.is_none(), "{:?}", prepared.error);
    let copies = generated_state(&next, "objective");
    assert_eq!(
        copies.len(),
        1,
        "the superseded block is collapsed instead of appended to: {:?}",
        next.context_messages
    );
    assert!(copies[0].content.contains("live objective"));
    assert!(
        !serde_json::to_string(&next).unwrap().contains("stale objective"),
        "the superseded copies are gone"
    );
}

#[test]
fn a_compaction_keeps_the_user_direction_and_consolidates_prior_checkpoints() {
    // The first user turn survives a real compaction verbatim instead of
    // existing only as summary prose.
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    // The fixture's first user turn is deliberately huge; make it small and
    // identifiable so pinning is observable in the replacement.
    let pin = "PINNED USER DIRECTION 7391";
    request.input["messages"][0]["content"] = json!(pin);
    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let first = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(first.changed, "{:?}", first.error);
    assert!(
        serde_json::to_string(&request.input).unwrap().contains(pin),
        "the first user direction must survive verbatim: {}",
        serde_json::to_string(&request).unwrap()
    );
    let events = f.committer.committed_events().unwrap();
    let compacted = events
        .iter()
        .find(|e| e.kind == "context.compacted")
        .unwrap();
    assert_eq!(compacted.payload["pinnedUnits"].as_u64(), Some(1));
    assert!(compacted.payload["pinnedTokenCount"].as_u64().unwrap() > 0);
}

#[test]
fn a_compaction_replaces_a_prior_checkpoint_instead_of_pinning_it() {
    let mut f = Fixture::new();
    f.authority.context.model_context =
        json!({"contextWindow":16000,"policy":{"auto":false,"retainTokens":0,"pruneToolResults":false}});
    let outer = stable("outer.consolidate").unwrap();
    let pin = "PINNED USER DIRECTION 7391";
    let mut request = f.request();
    request.input["messages"][0]["content"] = json!(pin);
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: 0,
        content: c::frame_summary("an older checkpoint"),
        ..Default::default()
    });
    f.prepare(outer.as_str(), 0, &mut request);
    // A large tool read inside the compactable span, closed by a small turn so
    // the selection can retain its mandatory last unit.
    f.write("src/big.txt", &"z".repeat(30_000));
    let call = ModelToolCallV1 {
        call_id: "read.big".into(),
        provider_call_id: Some("read.big".into()),
        capability_id: FILE_READ_CAPABILITY_ID.into(),
        name: FILE_READ_PROVIDER_NAME.into(),
        arguments: json!({"path":"src/big.txt"}),
        provider_context: None,
    };
    let settled = f
        .authority
        .invoke(&outer, 1, &call, &CancellationToken::default())
        .unwrap();
    let exchange = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::ToolCall { call }],
        results: vec![settled.result],
    };
    f.authority.commit_exchange(&outer, 1, &exchange).unwrap();
    request.exchanges.push(exchange);
    let closing = aworkit_capability_host::ModelToolExchangeV1 {
        assistant_content: vec![aworkit_capability_host::ModelAssistantContentV1::Text {
            text: "Read it.".into(),
        }],
        results: Vec::new(),
    };
    f.authority.commit_exchange(&outer, 2, &closing).unwrap();
    request.exchanges.push(closing);

    let (gateway, _requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let result = compact_manual(&f, &gateway, &plan, &outer, &mut request);
    assert!(result.changed, "{:?}", result.error);
    let serialized = serde_json::to_string(&request).unwrap();
    assert!(serialized.contains(pin), "the user direction survives");
    assert_eq!(
        serialized.matches(c::CHECKPOINT_PREAMBLE).count(),
        1,
        "the prior checkpoint is consolidated, not stacked or pinned as a user turn"
    );
    let events = f.committer.committed_events().unwrap();
    let compacted = events
        .iter()
        .find(|e| e.kind == "context.compacted")
        .unwrap();
    assert_eq!(compacted.payload["pinnedUnits"].as_u64(), Some(1));
}

#[test]
fn the_summary_prompt_is_the_live_prompt_prefix_plus_the_directive() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let live: Vec<Value> = c::units(&request).unwrap().iter().map(prompt_shape).collect();
    let (gateway, requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let result = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(result.changed, "{:?}", result.error);

    let summary = requests.lock().unwrap()[0].clone();
    let summary_units = c::units(&summary).unwrap();
    let (directive, prefix) = summary_units.split_last().expect("summary units");
    let c::Unit::Message(directive) = directive else {
        panic!("the summary prompt must end with the directive message");
    };
    assert_eq!(directive.content, c::INSTRUCTION.trim_end());
    // The auxiliary call reuses the exact live prompt prefix and adds only the
    // directive, so a provider prefix cache can match almost all of it.
    let prefix: Vec<Value> = prefix.iter().map(prompt_shape).collect();
    assert_eq!(prefix, live[..prefix.len()], "summary prefix diverged from the live prompt");
    assert!(!prefix.is_empty());
    assert!(
        prefix.len() < live.len(),
        "the retained tail is kept live, not resent for summarization"
    );
    assert_eq!(
        summary.parameters["maxOutputTokens"],
        json!(
            c::Policy::default()
                .replacement_plan(16_000, c::fixed_tokens(&summary).unwrap(), None)
                .summary
        )
    );
}

#[test]
fn actual_compaction_does_not_restore_deleted_nested_rules_from_warm_cache() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let workspace = f.authority.context.workspace.root.clone();
    let (gateway, _, plan) = gateway(move |_| {
        std::fs::remove_file(workspace.join("src/AGENTS.md")).unwrap();
        Ok("Requirements established.".into())
    });
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    let text = serde_json::to_string(&c::units(&request).unwrap()).unwrap();
    assert!(text.contains("ROOT V1"));
    assert!(!text.contains("NESTED V1"));
}

#[test]
fn an_oversized_summary_prompt_is_fitted_to_the_window_before_it_is_sent() {
    // Task 144's case: the context is already over the declared window when
    // compaction first fires, so the prompt the summary would need does not fit
    // the window it must be sent in. The prompt is sized before the call, so an
    // oversized prefix costs one fitting call instead of one failed round trip
    // per dropped unit.
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let window = 40_000_u64;
    f.authority.context.model_context =
        json!({"contextWindow":window,"policy":{"auto":true,"pruneToolResults":false}});
    // A small user direction and a lot of accumulated history: the recorded
    // shape of the failure, not an oversized instruction.
    request.input["messages"][0]["content"] = json!("Build the Tetris game.");
    for index in 0..4 {
        request.exchanges.push(tool_exchange(
            &format!("read.{index}"),
            json!(format!("result-{index}-").repeat(12_000)),
        ));
    }
    let fixed = c::fixed_tokens(&request).unwrap();
    let full: u64 = c::units(&request)
        .unwrap()
        .iter()
        .map(c::Unit::tokens)
        .sum();
    assert!(
        full + fixed > window,
        "the fixture must not fit its window: {full} + {fixed} > {window}"
    );
    let (gateway, requests, plan) = gateway(|_| Ok("Condensed checkpoint.".into()));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(
        prepared.changed && prepared.error.is_none(),
        "{:?}",
        prepared.error
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "one fitting call, not one failed round trip per dropped unit"
    );
    let sent = c::estimate(&requests.lock().unwrap()[0]).unwrap();
    assert!(
        sent <= window,
        "the summary prompt fits the window it is sent in: {sent} > {window}"
    );
    let events = f.committer.committed_events().unwrap();
    let compacted: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "context.compacted")
        .collect();
    assert_eq!(compacted.len(), 1);
    assert!(
        compacted[0].payload["trimmedUnits"].as_u64().unwrap() > 0,
        "the evidence records that history left without being summarised: {}",
        compacted[0].payload
    );
    assert_eq!(compacted[0].payload["promptFitsWindow"], json!(true));
    assert!(
        c::estimate(&request).unwrap() <= window,
        "the committed context fits its window"
    );
}

#[test]
fn a_context_over_the_window_is_reduced_even_when_no_summary_arrives() {
    // The provider never delivers the summary and the context cannot be sent as
    // it stands, so the oldest history leaves the selection instead of the run
    // being left over its own declared window. The user's own direction and a
    // marker naming the loss survive.
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let window = 40_000_u64;
    f.authority.context.model_context =
        json!({"contextWindow":window,"policy":{"auto":true,"pruneToolResults":false}});
    request.input["messages"][0]["content"] = json!("Build the Tetris game.");
    for index in 0..4 {
        request.exchanges.push(tool_exchange(
            &format!("read.{index}"),
            json!(format!("result-{index}-").repeat(12_000)),
        ));
    }
    let (gateway, requests, plan) = gateway(|_| Err(ProviderError::RequestTimedOut));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::ContextOverflow,
        )
        .unwrap();
    assert!(
        prepared.changed,
        "an unsendable context is reduced, not left as it is"
    );
    assert!(
        prepared.provider_error.is_some(),
        "the failed summary is still reported to the model"
    );
    // One bounded recovery ladder, and then the reduction commits anyway.
    assert!(
        requests.lock().unwrap().len() <= 1 + c::SUMMARY_SHRINK_RETRIES as usize,
        "the recovery budget is bounded"
    );
    let events = f.committer.committed_events().unwrap();
    let compacted: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "context.compacted")
        .collect();
    assert_eq!(compacted.len(), 1);
    assert_eq!(compacted[0].payload["strategy"], json!("drop"));
    assert!(
        compacted[0].payload["removedTokenCount"].as_u64().unwrap() > 0,
        "the evidence names what left: {}",
        compacted[0].payload
    );
    assert!(
        c::estimate(&request).unwrap() <= window,
        "the committed context fits its window"
    );
    let text = serde_json::to_string(&c::units(&request).unwrap()).unwrap();
    assert!(
        text.contains(c::DROPPED_SPAN_MARKER.trim()),
        "the model is told that earlier history was dropped"
    );
    assert!(
        !text.contains("result-0-"),
        "the oldest history left the selection"
    );
    assert!(
        text.contains("Build the Tetris game."),
        "the user's own direction survives"
    );
}

#[test]
fn failed_cancelled_and_nonshrinking_summaries_leave_the_selection_unchanged() {
    for mode in ["failure", "cancel", "large", "empty"] {
        let mut f = Fixture::new();
        let (outer, mut request) = history(&mut f);
        let (gateway, _, plan) = gateway(move |token| match mode {
            "failure" => Err(ProviderError::RequestTimedOut),
            "cancel" => {
                token.cancel();
                Err(ProviderError::Cancelled)
            }
            "large" => Ok("larger than original ".repeat(6000)),
            _ => Ok(String::new()),
        });
        let prepared = f
            .authority
            .manage_model_context(
                &gateway,
                &plan,
                &outer,
                1,
                Some(&f.agent),
                &mut request,
                &CancellationToken::default(),
                Trigger::Manual,
            )
            .unwrap();
        assert!(prepared.error.is_some(), "{mode}");
        assert_no_reduction(&f, prepared.changed, mode);
        let events = f.committer.committed_events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "context.compaction-started")
                .count(),
            1,
            "{mode}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "context.compaction-ended")
                .count(),
            1,
            "{mode}"
        );
        assert!(
            !events.iter().any(|e| e.kind == "context.compacted"),
            "{mode}"
        );
    }
}

#[test]
fn a_failed_summary_recovers_on_a_smaller_prompt_before_it_becomes_terminal() {
    // The auxiliary call overran its output cap (the measured 32k-run failure:
    // the stream ended without a supported stop reason). The same span is
    // retried against a smaller prompt: the large result inside it is reduced
    // first, then the oldest whole unit is dropped.
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    f.authority.context.model_context = json!({"policy":{"auto":false}});
    request
        .exchanges
        .push(tool_exchange("read.big", json!("z".repeat(20_000))));
    request.context_messages.push(ModelToolContextV1 {
        after_exchanges: request.exchanges.len(),
        content: "Continue with the next task.".into(),
        ..Default::default()
    });
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = attempts.clone();
    let (gateway, requests, plan) =
        gateway(
            move |_| match counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 | 1 => Err(ProviderError::Failed(
                    "provider failed: stream finished without a supported stop reason".into(),
                )),
                _ => Ok("Condensed checkpoint.".into()),
            },
        );
    let before = c::hash(&request);
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(
        prepared.changed && prepared.error.is_none(),
        "{:?}",
        prepared.error
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let first = c::units(&requests[0]).unwrap();
    let second = c::units(&requests[1]).unwrap();
    let third = c::units(&requests[2]).unwrap();
    assert_eq!(
        first.len(),
        second.len(),
        "the first recovery reduces the result without dropping a unit"
    );
    assert!(
        c::estimate(&requests[1]).unwrap() < c::estimate(&requests[0]).unwrap(),
        "the pruned prompt is smaller"
    );
    assert_eq!(
        second.len(),
        third.len() + 1,
        "the second recovery drops the oldest whole unit"
    );
    drop(requests);
    assert_ne!(c::hash(&request), before, "one reduction was committed");
    let events = f.committer.committed_events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-started")
            .count(),
        1,
        "recovery stays inside one attempt"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-ended")
            .count(),
        1
    );
    let compacted: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "context.compacted")
        .collect();
    assert_eq!(compacted.len(), 1);
    assert_eq!(compacted[0].payload["summaryAttempts"], 3);
}

#[test]
fn an_exhausted_summary_recovery_warns_once_and_keeps_the_selection() {
    // An automatic reduction the provider never delivers: the recovery budget is
    // spent, the selection is untouched, and the failure is reported once.
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    f.authority.context.model_context = json!({"contextWindow":65_536,"policy":{"auto":true,"retainTokens":0,"pruneToolResults":false}});
    request.input["messages"][0]["content"] =
        json!("Established requirements and implementation history. ".repeat(5_000));
    request
        .exchanges
        .push(tool_exchange("read.big", json!("z".repeat(20_000))));
    let (gateway, requests, plan) = gateway(|_| Err(ProviderError::RequestTimedOut));
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.provider_error.is_some());
    assert_no_reduction(&f, prepared.changed, "exhausted recovery");
    // This fixture's span cannot be fitted to its window - one user turn alone
    // is nearly the whole window - so the prompt is sent as it is, every
    // recovery is spent on a smaller one, and the selection is still unchanged
    // because no reduction here could fit. The fitting prompt and the fitting
    // reduction have their own tests.
    assert_eq!(
        requests.lock().unwrap().len(),
        1 + c::SUMMARY_SHRINK_RETRIES as usize,
        "the recovery budget is bounded"
    );
    let events = f.committer.committed_events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-started")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.compaction-ended")
            .count(),
        1
    );
    assert!(!events.iter().any(|e| e.kind == "context.compacted"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "context.checkpoint")
            .count(),
        1,
        "only the ordinary end-of-preparation snapshot is written"
    );
    let warnings: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "context.compaction-warning")
        .collect();
    assert_eq!(
        warnings
            .iter()
            .filter(|e| e.payload["body"]
                .as_str()
                .is_some_and(|body| body.contains("provider request timed out")))
            .count(),
        1,
        "one warning names the failure, not one per recovery"
    );
    assert_eq!(
        warnings.len(),
        2,
        "plus the existing still-above-threshold notice: {warnings:?}"
    );
}

#[test]
fn cancellation_wins_over_summary_recovery() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    f.authority.context.model_context = json!({"policy":{"auto":false}});
    request
        .exchanges
        .push(tool_exchange("read.big", json!("z".repeat(20_000))));
    let token = CancellationToken::default();
    let cancelling = token.clone();
    let (gateway, requests, plan) = gateway(move |_| {
        cancelling.cancel();
        Err(ProviderError::RequestTimedOut)
    });
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut request,
            &token,
            Trigger::Manual,
        )
        .unwrap();
    assert!(prepared.error.is_some());
    assert_no_reduction(&f, prepared.changed, "cancelled attempt");
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "a cancelled attempt is never retried"
    );
}

#[test]
fn concurrent_edit_rejects_stale_summary_and_closes_the_transaction() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let events = f.committer.clone();
    let (gateway, _, plan) = gateway(move |_| {
        events
            .commit(vec![SemanticEventDraft::new(
                "context.edited",
                json!({"nodeId":"agent.1","document":{}}),
            )])
            .unwrap();
        Ok("Old requirements.".into())
    });
    let error = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap()
        .error
        .unwrap();
    assert!(error.contains("changed during compaction"), "{error}");
    let events = f.committer.committed_events().unwrap();
    assert!(!events.iter().any(|e| e.kind == "context.compacted"));
    assert_eq!(events.last().unwrap().kind, "context.compaction-ended");
}

#[test]
fn actual_compaction_restores_unavailable_rules_as_stale_and_keeps_removal_tombstones() {
    for unavailable in [true, false] {
        let mut f = Fixture::new();
        let (outer, mut request) = history(&mut f);
        let path = f.authority.context.workspace.root.join("src/AGENTS.md");
        std::fs::remove_file(&path).unwrap();
        if unavailable {
            std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        } else {
            // Commit a genuine removal before compaction; its tombstone must
            // survive even though its model-visible instruction is shadowed.
            f.prepare(outer.as_str(), 2, &mut request);
        }
        let (gateway, _, plan) = gateway(|_| Ok("Preserve all established requirements.".into()));
        let prepared = f
            .authority
            .manage_model_context(
                &gateway,
                &plan,
                &outer,
                if unavailable { 1 } else { 2 },
                Some(&f.agent),
                &mut request,
                &CancellationToken::default(),
                Trigger::Manual,
            )
            .unwrap();
        assert!(prepared.changed && prepared.error.is_none());
        let text = serde_json::to_string(&c::units(&request).unwrap()).unwrap();
        assert_eq!(text.contains("NESTED V1"), unavailable, "{text}");
        if unavailable {
            assert!(f.committer.committed_events().unwrap().iter().any(|e| {
                e.kind == "context.instructions"
                    && e.payload["diagnostics"]
                        .to_string()
                        .to_lowercase()
                        .contains("stale")
            }));
        }
    }
}

#[test]
fn successive_actual_compactions_rearm_missing_scopes_and_preserve_partial_survival() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let root = request
        .context_messages
        .iter()
        .position(|m| m.content.contains("ROOT V1"))
        .unwrap();
    let mut root = request.context_messages.remove(root);
    let surviving = root.instruction_event_id.clone();
    root.after_exchanges = 1;
    request.context_messages.push(root);
    // A saved selection changes positions while retaining an authentic ref.
    f.committer.commit(vec![SemanticEventDraft::new("context.edited",json!({"nodeId":f.agent.node_id,"document":crate::runtime::context_inspection::ContextDocument::from_request(&request)}))]).unwrap();
    request.exchanges.clear();
    request.context_messages.clear();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = count.clone();
    let (gateway, _, plan) = gateway(move |_| {
        Ok(
            if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                "Prior requirements and decisions. ".repeat(120)
            } else {
                "Prior requirements remain applicable.".into()
            },
        )
    });
    let first = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(first.changed && first.error.is_none());
    assert!(
        request
            .context_messages
            .iter()
            .any(|m| m.instruction_event_id == surviving),
        "authentic surviving baseline remains selected"
    );
    let second = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(
        second.changed && second.error.is_none(),
        "{:?}",
        second.error
    );
    let text = serde_json::to_string(&c::units(&request).unwrap()).unwrap();
    assert!(text.contains("ROOT V1") && text.contains("NESTED V1"));
    assert_eq!(
        text.matches("<compacted-summary>").count(),
        1,
        "checkpoint consolidates older summaries"
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Commits a checkpoint that records the tool interface of an earlier build: the
/// descriptions of that pass plus a recorded call under the provider alias or the
/// capability of that pass.
fn commit_stale_checkpoint(
    f: &Fixture,
    outer: &str,
    request: &ModelToolRequestV1,
    capability_id: &str,
    name: &str,
) {
    let mut document = crate::runtime::context_inspection::ContextDocument::from_request(request);
    for tool in &mut document.tools {
        tool.description = "Superseded description from an earlier build.".into();
    }
    document.exchanges = request.exchanges.clone();
    for exchange in &mut document.exchanges {
        for content in exchange.assistant_content.iter_mut() {
            if let aworkit_capability_host::ModelAssistantContentV1::ToolCall { call } = content {
                call.capability_id = capability_id.into();
                call.name = name.into();
            }
        }
    }
    let snapshot = c::Snapshot {
        node_id: f.agent.node_id.clone(),
        child: None,
        outer: outer.into(),
        through: document.exchanges.len(),
        conversation_cursor: 0,
        conversation_sequence: None,
        document,
        anchor: None,
    };
    let owner_key = c::hash(&json!({
        "chat": f.authority.context.chat_id,
        "branch": f.authority.context.project_branch
    }));
    f.committer
        .commit(vec![SemanticEventDraft::new(
            "context.checkpoint",
            json!({
                "ownerKey": owner_key,
                "nodeId": f.agent.node_id,
                "child": null,
                "pressureTokens": 1,
                "pressureReported": false,
                "snapshot": snapshot,
            }),
        )])
        .unwrap();
}

/// A checkpoint records the interface of the pass that wrote it, while every pass
/// resolves its own interface from this build and a Chat owns only the authority
/// that selects capabilities. Continuing a Chat whose checkpoint predates a
/// refreshed description, schema or provider alias must restore its committed
/// history under the current selection: comparing the two interfaces instead
/// ended Agent nodes with "tool authority rejected the provider request:
/// Context checkpoint tools differ from the frozen Agent selection".
#[test]
fn checkpoint_with_an_older_tool_interface_restores_under_the_current_selection() {
    let mut f = Fixture::new();
    let (outer, request) = history(&mut f);
    commit_stale_checkpoint(&f, outer.as_str(), &request, FILE_READ_CAPABILITY_ID, "read_legacy");
    let (gateway, calls, plan) = gateway(|_| panic!("No auxiliary call needed"));
    let mut next = f.request();
    f.prepare(outer.as_str(), 0, &mut next);
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.error.is_none(), "{:?}", prepared.error);
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(next.tools, request.tools, "the pass keeps its own selection");
    assert_eq!(
        next.exchanges, request.exchanges,
        "the recorded call is re-pointed at the provider name this pass offers"
    );
    assert!(
        !f.committer
            .committed_events()
            .unwrap()
            .iter()
            .any(|e| e.kind == "context.selection-declined")
    );
}

/// A recorded call whose capability this pass no longer selects cannot be
/// represented in a provider request. The projection is declined, the condition
/// is committed, and the pass continues on its own context: committed evidence
/// stays untouched and no tool interface change can end an Agent node.
#[test]
fn checkpoint_calling_an_unselected_capability_is_declined_without_ending_the_node() {
    let mut f = Fixture::new();
    let (outer, request) = history(&mut f);
    commit_stale_checkpoint(&f, outer.as_str(), &request, "tool.shell.host", "shell");
    let (gateway, calls, plan) = gateway(|_| panic!("No auxiliary call needed"));
    let mut next = f.request();
    f.prepare(outer.as_str(), 0, &mut next);
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len(),
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.error.is_none(), "{:?}", prepared.error);
    assert!(calls.lock().unwrap().is_empty());
    let events = f.committer.committed_events().unwrap();
    let declined = events
        .iter()
        .find(|e| e.kind == "context.selection-declined")
        .expect("the declined projection is recorded");
    assert_eq!(
        declined.payload["capabilities"][0],
        json!("tool.shell.host"),
        "the condition names the capability this pass cannot offer"
    );
    assert!(next.exchanges.is_empty());
    assert!(
        events
            .iter()
            .any(|e| e.kind == "context.checkpoint" && e.sequence > declined.sequence),
        "the pass saves its own checkpoint, so the next turn is consistent again"
    );
}

/// Exchanges are also appended from committed records, not only from the
/// checkpoint. Those records were written by the pass that settled them, so they
/// name their calls under the provider alias of that build too. Every call the
/// restored selection carries is admitted before it can reach a provider.
#[test]
fn appended_committed_exchanges_are_admitted_under_the_current_selection() {
    let mut f = Fixture::new();
    let (outer, request) = history(&mut f);
    let mut legacy = request.exchanges[0].clone();
    for content in legacy.assistant_content.iter_mut() {
        if let aworkit_capability_host::ModelAssistantContentV1::ToolCall { call } = content {
            call.name = "read_legacy".into();
        }
    }
    f.authority.commit_exchange(&outer, 2, &legacy).unwrap();
    commit_stale_checkpoint(
        &f,
        outer.as_str(),
        &request,
        FILE_READ_CAPABILITY_ID,
        FILE_READ_PROVIDER_NAME,
    );
    let (gateway, calls, plan) = gateway(|_| panic!("No auxiliary call needed"));
    let mut next = f.request();
    f.prepare(outer.as_str(), 0, &mut next);
    let prepared = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            request.exchanges.len() + 1,
            Some(&f.agent),
            &mut next,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    assert!(prepared.error.is_none(), "{:?}", prepared.error);
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(next.exchanges.len(), 2, "{:?}", next.exchanges);
    for exchange in &next.exchanges {
        for content in &exchange.assistant_content {
            if let aworkit_capability_host::ModelAssistantContentV1::ToolCall { call } = content {
                assert!(
                    next.tools.iter().any(|tool| {
                        tool.capability_id == call.capability_id && tool.name == call.name
                    }),
                    "recorded call {} is not offered by this pass",
                    call.name
                );
            }
        }
    }
    assert!(!f.committer
        .committed_events()
        .unwrap()
        .iter()
        .any(|e| e.kind == "context.selection-declined"));
}

#[test]
fn checkpoint_isolation_covers_chat_node_branch_and_child() {
    for dimension in ["chat", "node", "branch", "child"] {
        let mut f = Fixture::new();
        let (outer, mut request) = history(&mut f);
        let (gateway, calls, plan) = gateway(|_| Ok("Prior requirements.".into()));
        assert!(
            f.authority
                .manage_model_context(
                    &gateway,
                    &plan,
                    &outer,
                    1,
                    Some(&f.agent),
                    &mut request,
                    &CancellationToken::default(),
                    Trigger::Manual
                )
                .unwrap()
                .changed
        );
        match dimension {
            "chat" => f.authority.context.chat_id = "chat.other".into(),
            "node" => f.agent.node_id = "agent.other".into(),
            "branch" => f.authority.context.project_branch = Some("other".into()),
            _ => f.agent.child = Some("child.other".into()),
        }
        let mut other = f.request();
        f.authority
            .manage_model_context(
                &gateway,
                &plan,
                &stable("outer.other").unwrap(),
                0,
                Some(&f.agent),
                &mut other,
                &CancellationToken::default(),
                Trigger::Pressure,
            )
            .unwrap();
        assert!(
            !serde_json::to_string(&other)
                .unwrap()
                .contains("<compacted-summary>"),
            "{dimension}"
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn real_compaction_respects_instruction_restoration_budget_and_retries_omitted_scopes() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    if let StoredFileToolLimitV1::WorkspaceInstructions { configuration } = &mut f
        .authority
        .context
        .bindings
        .iter_mut()
        .find(|b| b.capability_id == ID)
        .unwrap()
        .limit
    {
        configuration.max_bytes = 700;
    }
    f.write("AGENTS.md", &"Broad root instructions ".repeat(70));
    f.write("src/AGENTS.md", &"Specific nested instructions ".repeat(20));
    let (gateway, _, plan) = gateway(|_| Ok("Prior requirements.".into()));
    let result = f
        .authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    assert!(result.changed && result.error.is_none());
    assert!(
        request
            .context_messages
            .iter()
            .filter(|m| m.instruction_event_id.is_some())
            .all(|m| m.content.len() <= 700)
    );
    assert!(f.committer.committed_events().unwrap().iter().any(|e| {
        e.kind == "context.instructions"
            && e.payload["diagnostics"]
                .to_string()
                .to_lowercase()
                .contains("budget")
    }));
}

#[test]
fn fork_restores_only_explicitly_inherited_instruction_records_after_compaction() {
    let mut f = Fixture::new();
    let (outer, mut request) = history(&mut f);
    let (gateway, _, plan) = gateway(|_| Ok("Prior requirements.".into()));
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &outer,
            1,
            Some(&f.agent),
            &mut request,
            &CancellationToken::default(),
            Trigger::Manual,
        )
        .unwrap();
    let sources: Vec<_> = f
        .authority
        .runtime
        .records
        .events(KIND)
        .unwrap()
        .iter()
        .filter_map(|step| {
            step.get("event")
                .filter(|e| e["id"].is_string())
                .map(|e| json!({"id":e["id"],"owner":e["owner"]}))
        })
        .collect();
    f.authority.context.chat_id = "chat.fork".into();
    request
        .context_messages
        .retain(|message| message.instruction_event_id.is_none());
    let owner_key =
        c::hash(&json!({"chat":"chat.fork","branch":f.authority.context.project_branch}));
    // These are core-owned fork facts; arbitrary summary prose supplies no authority.
    f.committer.commit(vec![SemanticEventDraft::new("context.fork-source",json!({"nodeId":f.agent.node_id,"ownerKey":owner_key,"instructionSources":sources})),SemanticEventDraft::new("context.edited",json!({"nodeId":f.agent.node_id,"ownerKey":owner_key,"document":crate::runtime::context_inspection::ContextDocument::from_request(&request)}))]).unwrap();
    f.write("src/AGENTS.md", &"x".repeat(2 * 1024 * 1024));
    let mut child = f.request();
    f.authority
        .manage_model_context(
            &gateway,
            &plan,
            &stable("outer.fork").unwrap(),
            0,
            Some(&f.agent),
            &mut child,
            &CancellationToken::default(),
            Trigger::Pressure,
        )
        .unwrap();
    let text = serde_json::to_string(&child).unwrap();
    assert!(
        text.contains("<compacted-summary>") && text.contains("NESTED V1"),
        "{text}"
    );
    assert!(f.committer.committed_events().unwrap().iter().any(|e| {
        e.kind == "context.instructions"
            && e.payload["diagnostics"]
                .to_string()
                .to_lowercase()
                .contains("stale")
    }));
}
