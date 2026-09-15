//! One Agent turn whose provider fails, and one whose exact request outgrew the
//! frozen plan's input bound. Neither ends the Run: the failure is reported to
//! the model on the same frozen route, which then decides how to finish.

use super::*;
use super::super::super::compaction::{Preparation, Trigger};
use aworkit_capability_host::{
    ModelEventV1, ModelRequestV1, ModelToolEventV1, ProviderAcceptanceV1, ProviderEnginePortV1,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// Base input bounded to 8 KiB, well inside the tool loop's 512 KiB exchange
/// headroom, so one settled result is what exceeds the bound.
const MAXIMUM_INPUT_BYTES: usize = 8 * 1024;
/// One settled result under the tool-output cap, as a real Files read is.
const RESULT_BYTES: usize = 256 * 1024;

fn call(id: &str) -> ModelToolCallV1 {
    ModelToolCallV1 {
        call_id: id.into(),
        provider_call_id: Some(id.into()),
        capability_id: "tool.files.read".into(),
        name: "read".into(),
        arguments: json!({}),
        provider_context: None,
    }
}

fn activity(call: &ModelToolCallV1) -> WorkflowToolActivityV1 {
    WorkflowToolActivityV1 {
        call_id: call.call_id.clone(),
        invocation_id: StableId::parse(format!("invoke.{}", call.call_id)).unwrap(),
        capability_id: call.capability_id.clone(),
        path: String::new(),
        status: "completed".into(),
        summary: String::new(),
        outcome_hash: String::new(),
        replayed: false,
    }
}

fn tool_request(id: &StableId, input: Value) -> ModelToolLoopRequestV1<'_> {
    ModelToolLoopRequestV1 {
        agent_context: None,
        outer_invocation_id: id,
        input,
        parameters: BTreeMap::new(),
        definitions: vec![ModelToolDefinitionV1 {
            capability_id: "tool.files.read".into(),
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
        }],
        binding_id: "model".into(),
        binding_version_hash: "v1".into(),
        maximum_input_bytes: MAXIMUM_INPUT_BYTES,
        maximum_output_bytes: 1_000_000,
        maximum_tool_output_bytes: RESULT_BYTES,
        maximum_timeout_recoveries: 0,
    }
}

fn user_input() -> Value {
    json!({"messages":[{"role":"user","content":"Inspect the workspace."}]})
}

/// Settles one oversized result on the first turn, then fails every remaining
/// turn until the model has been told about the failure, and finally answers.
struct Provider {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    dispatches: AtomicUsize,
}

impl Provider {
    fn new(observed: Arc<Mutex<Vec<ModelToolRequestV1>>>) -> Self {
        Self {
            observed,
            dispatches: AtomicUsize::new(0),
        }
    }
}

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
        self.observed.lock().unwrap().push(request.clone());
        let dispatch = self.dispatches.fetch_add(1, Ordering::SeqCst);
        if request.retry_notice.is_some() || request.exchanges.is_empty() {
            if request.exchanges.is_empty() && request.retry_notice.is_none() {
                emit(ModelToolEventV1::ToolCall { call: call("read0") })?;
            } else {
                emit(ModelToolEventV1::AssistantOutput {
                    text: "The workspace is understood.".into(),
                })?;
            }
        } else if dispatch == 1 {
            // The exact refusal the model must be told about. This is the turn
            // whose request also outgrew the frozen bound after the settled
            // exchange, so the two conditions are exercised together.
            return Err(ProviderError::Failed("quota exceeded".into()));
        } else {
            emit(ModelToolEventV1::AssistantOutput {
                text: "The workspace is understood.".into(),
            })?;
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

/// Settles one large exchange and never reduces anything, so the authority
/// cannot resolve an over-bound request on its own.
struct Stuck;

impl ModelToolInvocationPortV1 for Stuck {
    fn manage_model_context(
        &self,
        _: &FrozenModelGateway,
        _: &ModelResolutionPlanV1,
        _: &StableId,
        _: usize,
        _: Option<&AgentContextV1>,
        _: &mut ModelToolRequestV1,
        _: &CancellationToken,
        _: Trigger,
    ) -> Result<Preparation, String> {
        Ok(Preparation {
            max_overflow_retries: 1,
            ..Default::default()
        })
    }
    fn invoke(
        &self,
        _: &StableId,
        _: u32,
        call: &ModelToolCallV1,
        _: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        Ok(SettledModelToolCallV1 {
            result: ModelToolResultV1 {
                images: Vec::new(),
                call_id: call.call_id.clone(),
                content: json!("x".repeat(RESULT_BYTES)),
                is_error: false,
            },
            activity: activity(call),
        })
    }
    fn commit_exchange(
        &self,
        _: &StableId,
        _: u32,
        _: &ModelToolExchangeV1,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn gateway(provider: Provider) -> FrozenModelGateway {
    FrozenModelGateway::new(vec![Box::new(provider)])
}

#[test]
fn a_provider_failure_is_reported_to_the_model_and_the_agent_finishes() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    let result = execute_model_tool_loop_approval_v1(
        &gateway(Provider::new(observed.clone())),
        tool_request(&id, user_input()),
        &Stuck,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| panic!("a reported provider failure must not end the node: {failure}"));
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };

    assert_eq!(outcome.assistant_text, "The workspace is understood.");
    let turns = observed.lock().unwrap();
    let reported = turns
        .iter()
        .find(|turn| turn.retry_notice.is_some())
        .expect("the provider failure reached the model");
    assert!(
        reported
            .retry_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("quota exceeded")),
        "the model sees the provider's own diagnostic"
    );
    assert_eq!(
        turns.last().unwrap().exchanges.len(),
        1,
        "the settled exchange is neither replayed nor discarded while recovering"
    );
}

#[test]
fn an_over_bound_request_that_cannot_be_reduced_is_still_reported_to_the_model() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    // The settled exchange pushes the exact request past the bound and the
    // authority cannot reduce anything. The condition is reported to the model
    // like any other provider failure, and the node still completes.
    let result = execute_model_tool_loop_approval_v1(
        &gateway(Provider::new(observed.clone())),
        tool_request(&id, user_input()),
        &Stuck,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| panic!("the turn must keep running: {failure}"));
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };
    assert_eq!(outcome.assistant_text, "The workspace is understood.");
    let turns = observed.lock().unwrap();
    assert!(
        turns.iter().any(|turn| turn.retry_notice.is_some()),
        "the failure is model-visible"
    );
}
