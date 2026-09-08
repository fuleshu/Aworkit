use super::*;
use crate::runtime::compaction::{self as c, Trigger};
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
    f.authority.context.model_context["policy"]["auto"] = json!(true);
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = counter.clone();
    let (gateway, calls, plan) = gateway(move |_| {
        Ok(
            if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                "Interim ".repeat(8000)
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
    assert!(c::estimate(&request).unwrap() < 12800);
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
    assert!(c::estimate(&next).unwrap() < 12800);
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
    assert_eq!(summary_requests[0].parameters["maxOutputTokens"], 8192);
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
fn failed_cancelled_and_nonshrinking_summaries_leave_the_selection_unchanged() {
    for mode in ["failure", "cancel", "large", "empty"] {
        let mut f = Fixture::new();
        let (outer, mut request) = history(&mut f);
        let before = request.clone();
        let (gateway, _, plan) = gateway(move |token| match mode {
            "failure" => Err(ProviderError::RequestTimedOut),
            "cancel" => {
                token.cancel();
                Err(ProviderError::Cancelled)
            }
            "large" => Ok("larger than original ".repeat(6000)),
            _ => Ok(String::new()),
        });
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
                .error
                .is_some()
        );
        assert_eq!(request, before, "{mode}");
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
