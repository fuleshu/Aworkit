use super::*;
use aworkit_capability_host::{
    ModelEventV1, ModelRequestV1, ModelToolEventV1, ProviderAcceptanceV1, ProviderEnginePortV1,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

const TEXT: &str = "I can see Adashi in my tool list. Let me check its project context.";

fn call(id: &str) -> ModelToolCallV1 {
    ModelToolCallV1 {
        call_id: id.into(),
        provider_call_id: Some(id.into()),
        capability_id: format!("tool.{id}"),
        name: id.into(),
        arguments: json!({}),
        provider_context: None,
    }
}

fn request(id: &StableId) -> ModelToolLoopRequestV1<'_> {
    ModelToolLoopRequestV1 {
        agent_context: None,
        outer_invocation_id: id,
        input: json!("Can you see Adashi?"),
        parameters: BTreeMap::new(),
        definitions: ["read", "shell", "adashi"]
            .into_iter()
            .map(|id| ModelToolDefinitionV1 {
                capability_id: format!("tool.{id}"),
                name: id.into(),
                description: id.into(),
                input_schema: json!({"type":"object"}),
            })
            .collect(),
        binding_id: "model".into(),
        binding_version_hash: "v1".into(),
        maximum_input_bytes: 1_000_000,
        maximum_output_bytes: 1_000_000,
        maximum_tool_output_bytes: 4096,
        maximum_timeout_recoveries: 0,
        maximum_tokens: 1000,
    }
}

struct Provider(Arc<Mutex<Vec<ModelToolRequestV1>>>);
impl ProviderEnginePortV1 for Provider {
    fn binding_id(&self) -> &str {
        "model"
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
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.0.lock().unwrap().push(request.clone());
        if request.exchanges.is_empty() {
            emit(ModelToolEventV1::AssistantOutput { text: TEXT.into() })?;
            for id in ["read", "shell", "adashi"] {
                emit(ModelToolEventV1::ToolCall { call: call(id) })?;
            }
        } else {
            emit(ModelToolEventV1::AssistantOutput {
                text: "Adashi remains available.".into(),
            })?;
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

#[derive(Default)]
struct Authority {
    invoked: Mutex<Vec<String>>,
    resolved: Mutex<Vec<String>>,
    committed: Mutex<Vec<ModelToolExchangeV1>>,
    second_approval: bool,
}
fn settled(call: &ModelToolCallV1, approved: bool) -> SettledModelToolCallV1 {
    SettledModelToolCallV1 {
        result: ModelToolResultV1 {
            call_id: call.call_id.clone(),
            content: if approved {
                json!({"project":"Aworkit"})
            } else {
                json!({"error":"user_rejected","detail":"Do not run shell commands."})
            },
            is_error: !approved,
        },
        activity: WorkflowToolActivityV1 {
            call_id: call.call_id.clone(),
            invocation_id: StableId::parse(format!("invoke.{}", call.call_id)).unwrap(),
            capability_id: call.capability_id.clone(),
            path: String::new(),
            status: "completed".into(),
            summary: String::new(),
            outcome_hash: String::new(),
            replayed: false,
        },
    }
}
impl ModelToolInvocationPortV1 for Authority {
    fn invoke(
        &self,
        _: &StableId,
        _: u32,
        call: &ModelToolCallV1,
        _: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        Ok(settled(call, true))
    }
    fn invoke_extended(
        &self,
        _: &StableId,
        _: u32,
        call: &ModelToolCallV1,
        _: &CancellationToken,
    ) -> Result<ToolInvokeV1, String> {
        self.invoked.lock().unwrap().push(call.call_id.clone());
        if call.call_id == "shell" || self.second_approval && call.call_id == "adashi" {
            Ok(ToolInvokeV1::Approval(ToolApprovalChallengeV1 {
                project_scope: None,
                decision_id: format!("decision.{}", call.call_id),
                invocation_id: format!("invoke.{}", call.call_id),
                nonce: "nonce.test".into(),
                expires_epoch_millis: 10000,
                capability_id: call.capability_id.clone(),
                call_id: call.call_id.clone(),
                title: "Review".into(),
                summary: "Review".into(),
            }))
        } else {
            Ok(ToolInvokeV1::Settled(settled(call, true)))
        }
    }
    fn resolve(
        &self,
        _: &StableId,
        _: u32,
        call: &ModelToolCallV1,
        response: &ApprovalResponseV1,
        _: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        self.resolved.lock().unwrap().push(call.call_id.clone());
        Ok(settled(call, response.approved))
    }
    fn commit_exchange(
        &self,
        _: &StableId,
        _: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), String> {
        self.committed.lock().unwrap().push(exchange.clone());
        Ok(())
    }
}

fn pending(run: ModelToolLoopRunV1) -> ModelToolLoopPendingV1 {
    let ModelToolLoopRunV1::Suspended { pending, .. } = run else {
        panic!("expected approval")
    };
    // Exercise the exact checkpoint serialization used across process restart.
    serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap()
}

#[test]
fn approval_and_denial_keep_text_completed_results_and_every_requested_call() {
    for approved in [true, false] {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let gateway = FrozenModelGateway::new(vec![Box::new(Provider(observed.clone()))]);
        let authority = Authority::default();
        let id = StableId::parse("outer.test").unwrap();
        let cancellation = CancellationToken::default();
        let checkpoint = pending(
            execute_model_tool_loop_approval_v1(&gateway, request(&id), &authority, &cancellation)
                .unwrap(),
        );
        let ModelToolLoopRunV1::Completed(outcome) = resume_model_tool_loop_v1(
            &gateway,
            request(&id),
            &authority,
            &checkpoint,
            approved,
            1,
            &cancellation,
        )
        .unwrap() else {
            panic!("expected completion")
        };
        let turns = observed.lock().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns[0].tools, turns[1].tools,
            "A denial must not remove MCP definitions"
        );
        let exchange = &turns[1].exchanges[0];
        assert_eq!(
            exchange.assistant_content[0],
            ModelAssistantContentV1::Text { text: TEXT.into() }
        );
        assert_eq!(
            exchange
                .results
                .iter()
                .map(|r| r.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "shell", "adashi"]
        );
        assert_eq!(exchange.results[0].content, json!({"project":"Aworkit"}));
        assert_eq!(exchange.results[1].is_error, !approved);
        if !approved {
            assert_eq!(
                exchange.results[2].content["error"],
                "not_executed_after_denial"
            );
        }
        assert_eq!(
            *authority.invoked.lock().unwrap(),
            if approved {
                vec!["read", "shell", "adashi"]
            } else {
                vec!["read", "shell"]
            }
        );
        assert_eq!(*authority.resolved.lock().unwrap(), vec!["shell"]);
        assert_eq!(outcome.settled_tool_calls, if approved { 3 } else { 2 });
        assert_eq!(authority.committed.lock().unwrap().len(), 1);
    }
}

#[test]
fn another_approval_in_the_same_response_does_not_replay_model_or_completed_calls() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let gateway = FrozenModelGateway::new(vec![Box::new(Provider(observed.clone()))]);
    let authority = Authority {
        second_approval: true,
        ..Default::default()
    };
    let id = StableId::parse("outer.test").unwrap();
    let cancellation = CancellationToken::default();
    let first = pending(
        execute_model_tool_loop_approval_v1(&gateway, request(&id), &authority, &cancellation)
            .unwrap(),
    );
    let second = pending(
        resume_model_tool_loop_v1(
            &gateway,
            request(&id),
            &authority,
            &first,
            true,
            1,
            &cancellation,
        )
        .unwrap(),
    );
    assert_eq!(observed.lock().unwrap().len(), 1);
    assert_eq!(second.call.call_id, "adashi");
    assert_eq!(second.pending_exchange.as_ref().unwrap().results.len(), 2);
    let ModelToolLoopRunV1::Completed(outcome) = resume_model_tool_loop_v1(
        &gateway,
        request(&id),
        &authority,
        &second,
        true,
        2,
        &cancellation,
    )
    .unwrap() else {
        panic!("expected completion")
    };
    assert_eq!(outcome.exchanges[0].assistant_content.len(), 4);
    assert_eq!(outcome.exchanges[0].results.len(), 3);
    assert_eq!(outcome.settled_tool_calls, 3);
    assert_eq!(observed.lock().unwrap().len(), 2);
    assert_eq!(
        *authority.invoked.lock().unwrap(),
        vec!["read", "shell", "adashi"]
    );
    assert_eq!(*authority.resolved.lock().unwrap(), vec!["shell", "adashi"]);
}

#[test]
fn older_single_call_checkpoints_still_resume() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let gateway = FrozenModelGateway::new(vec![Box::new(Provider(observed))]);
    let authority = Authority::default();
    let id = StableId::parse("outer.test").unwrap();
    let cancellation = CancellationToken::default();
    let mut checkpoint = pending(
        execute_model_tool_loop_approval_v1(&gateway, request(&id), &authority, &cancellation)
            .unwrap(),
    );
    checkpoint.pending_exchange = None;
    assert!(
        resume_pending_turn(
            &request(&id),
            &authority,
            &mut checkpoint,
            false,
            1,
            &cancellation
        )
        .unwrap()
    );
    assert_eq!(checkpoint.exchanges[0].results.len(), 1);
    assert_eq!(checkpoint.exchanges[0].results[0].call_id, "shell");
}
