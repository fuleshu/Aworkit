//! One Agent turn whose provider fails, and one whose exact request outgrew the
//! frozen plan's input bound. A provider failure is reported to the model on the
//! same frozen route; a failure that cannot change is surfaced instead of
//! looping, so the turn always ends.

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
        initial_context: Vec::new(),
        initial_exchanges: Vec::new(),
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
        match self.dispatches.fetch_add(1, Ordering::SeqCst) {
            // The model asks for the tool whose result pushes the request past
            // the frozen bound.
            0 => {
                emit(ModelToolEventV1::ToolCall { call: call("read0") })?;
            }
            // Distinct provider failures, each the model's to handle.
            1 => return Err(ProviderError::Failed("quota exceeded".into())),
            2 => return Err(ProviderError::Failed("upstream unavailable".into())),
            // The model answers after being told.
            _ => {
                emit(ModelToolEventV1::AssistantOutput {
                    text: "The workspace is understood.".into(),
                })?;
            }
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

/// Always fails the same way, so no model turn can change the condition.
struct StuckProvider {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
}

impl StuckProvider {
    fn new(observed: Arc<Mutex<Vec<ModelToolRequestV1>>>) -> Self {
        Self { observed }
    }
}

impl ProviderEnginePortV1 for StuckProvider {
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
        _: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.observed.lock().unwrap().push(request.clone());
        Err(ProviderError::Failed("quota exceeded".into()))
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

/// A context authority whose auxiliary compaction request the provider rejected.
/// The rejection reaches the loop through the authority channel, where a limit
/// must not be able to end the Agent node.
struct CompactionProviderFailure {
    rejected: AtomicUsize,
}

impl ModelToolInvocationPortV1 for CompactionProviderFailure {
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
        if self.rejected.fetch_add(1, Ordering::SeqCst) > 0 {
            return Ok(Preparation::default());
        }
        Ok(Preparation {
            provider_error: Some(ProviderError::Failed(
                "this model accepts at most 20 images per request".into(),
            )),
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
                content: json!("settled"),
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

fn gateway(provider: impl ProviderEnginePortV1 + 'static) -> FrozenModelGateway {
    FrozenModelGateway::new(vec![Box::new(provider)])
}

/// Records every dispatched request and answers immediately, so a test can read
/// exactly what the model was told.
struct Answering {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
}

impl Answering {
    fn new(observed: Arc<Mutex<Vec<ModelToolRequestV1>>>) -> Self {
        Self { observed }
    }
}

impl ProviderEnginePortV1 for Answering {
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
        emit(ModelToolEventV1::AssistantOutput {
            text: "Done with what I have.".into(),
        })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
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
    let notices: Vec<&str> = turns
        .iter()
        .filter_map(|turn| turn.retry_notice.as_deref())
        .collect();
    assert_eq!(
        notices.len(),
        2,
        "each distinct provider failure becomes one model-visible turn"
    );
    assert!(notices[0].contains("quota exceeded"));
    assert!(notices[1].contains("upstream unavailable"));
    assert_eq!(
        turns.last().unwrap().exchanges.len(),
        1,
        "the settled exchange is neither replayed nor discarded while recovering"
    );
}

#[test]
fn a_failure_that_cannot_change_is_surfaced_instead_of_looping() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    // Every dispatch fails with the same condition, which no model turn can
    // change. The first report is granted and the unchanged failure is then
    // surfaced, so the invocation always ends instead of spinning with no
    // durable activity while the runtime lock is held.
    let failure = match execute_model_tool_loop_approval_v1(
        &gateway(StuckProvider::new(observed.clone())),
        tool_request(&id, user_input()),
        &Stuck,
        &CancellationToken::default(),
    ) {
        Ok(ModelToolLoopRunV1::Suspended { .. }) => panic!("no approval was requested"),
        Ok(ModelToolLoopRunV1::Completed(outcome)) => {
            panic!(
                "an irreducible failure must be surfaced, not completed: {}",
                outcome.assistant_text
            )
        }
        Err(failure) => failure,
    };
    assert!(
        failure.error.to_string().contains("quota exceeded"),
        "the surfaced failure names the real condition: {}",
        failure.error
    );

    let turns = observed.lock().unwrap();
    let reports = turns
        .iter()
        .filter(|turn| turn.retry_notice.is_some())
        .count();
    assert_eq!(
        reports, 1,
        "an unchanged failure is reported once, never repeatedly"
    );
    assert_eq!(
        turns.len(),
        2,
        "one report and one surfaced failure, with no further turns"
    );
}

#[test]
fn a_rejected_compaction_request_is_reported_to_the_model_instead_of_failing_the_node() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    // The auxiliary compaction request is rejected by the provider while the
    // acting request's context carries an over-limit image selection. That
    // rejection arrives through the context-authority channel, which must report
    // it on the frozen route instead of ending the Agent node.
    let result = execute_model_tool_loop_approval_v1(
        &gateway(Answering::new(observed.clone())),
        tool_request(&id, user_input()),
        &CompactionProviderFailure {
            rejected: AtomicUsize::new(0),
        },
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| {
        panic!("a provider-rejected compaction request must not end the node: {failure}")
    });
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };
    assert_eq!(outcome.assistant_text, "Done with what I have.");

    let turns = observed.lock().unwrap();
    let notice = turns[0]
        .retry_notice
        .as_deref()
        .expect("the model is told about the rejected compaction request");
    assert!(notice.contains("Aworkit recovery notice"));
    assert!(
        notice.contains("this model accepts at most 20 images per request"),
        "the provider's own diagnostic reaches the model: {notice}"
    );
}

#[test]
fn the_report_budget_is_finite_and_owns_the_whole_invocation() {
    let mut request = ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: user_input(),
        parameters: BTreeMap::new(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        retry_notice: None,
    };
    let mut budget = ProviderRecoveryBudget::default();
    let bound = ProviderError::InputBoundExceeded {
        input_bytes: 8,
        maximum_input_bytes: 4,
    };
    assert!(
        budget.note(&bound, &mut request).is_some(),
        "the first report reaches the model"
    );
    assert!(
        budget.note(&bound, &mut request).is_none(),
        "an identical failure is surfaced instead of reported again"
    );
    for index in 0..MAXIMUM_ERROR_RECOVERIES {
        let _ = budget.note(&ProviderError::Failed(format!("failure {index}")), &mut request);
    }
    assert!(budget.exhausted(), "the report budget is finite");
    assert!(
        budget
            .note(&ProviderError::Failed("one more".into()), &mut request)
            .is_none(),
        "an exhausted budget surfaces the failure"
    );
}
