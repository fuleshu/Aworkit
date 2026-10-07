//! One Agent turn whose provider fails, and one whose exact request outgrew the
//! frozen plan's input bound. Every provider, context and tool condition is
//! reported to the model on the same frozen route and none of them ends the node:
//! only a final answer, cancellation, or an unrecoverable authority denial ends
//! an Agent node.

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

/// Fails the same way `failures` times, then answers. The condition cannot be
/// changed by any model turn, so the loop must keep reporting it and let the
/// model's eventual answer stand instead of ending the node.
struct StuckUntilAnswering {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    failures: usize,
    dispatches: AtomicUsize,
}

impl StuckUntilAnswering {
    fn new(observed: Arc<Mutex<Vec<ModelToolRequestV1>>>, failures: usize) -> Self {
        Self {
            observed,
            failures,
            dispatches: AtomicUsize::new(0),
        }
    }
}

impl ProviderEnginePortV1 for StuckUntilAnswering {
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
        if self.dispatches.fetch_add(1, Ordering::SeqCst) < self.failures {
            return Err(ProviderError::Failed("quota exceeded".into()));
        }
        emit(ModelToolEventV1::AssistantOutput {
            text: "Stopping with what I have.".into(),
        })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cache: Default::default(),
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
fn a_short_run_of_unchanged_failures_is_reported_and_the_model_still_answers() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    // Three identical rejections is below the identical-failure threshold, so
    // every dispatch is reported and the model's eventual answer stands. A longer
    // identical streak ends the pass instead: see
    // an_identically_repeating_provider_failure_ends_the_pass_instead_of_spinning.
    let result = execute_model_tool_loop_approval_v1(
        &gateway(StuckUntilAnswering::new(observed.clone(), 3)),
        tool_request(&id, user_input()),
        &Stuck,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| {
        panic!("a repeated provider failure must not end the node: {failure}")
    });
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };
    assert_eq!(outcome.assistant_text, "Stopping with what I have.");

    let turns = observed.lock().unwrap();
    assert_eq!(
        turns.len(),
        4,
        "three rejected attempts and the turn the model answered on"
    );
    let notices: Vec<&str> = turns
        .iter()
        .filter_map(|turn| turn.retry_notice.as_deref())
        .collect();
    assert_eq!(notices.len(), 3, "every rejected attempt carries a notice");
    assert!(notices[0].contains("quota exceeded"));
    assert!(
        notices[1].contains("same provider failure repeated"),
        "a repeat is reported as a repeat instead of being withheld: {}",
        notices[1]
    );
    assert!(notices[2].contains("same provider failure repeated"));
}

#[test]
fn an_identically_repeating_provider_failure_ends_the_pass_instead_of_spinning() {
    // The recorded run: the provider answered HTTP 400 to every dispatch for
    // three minutes because the request carried an assistant message it refuses,
    // so no turn ever reached the model. There was no Agent decision pending and
    // nothing the loop could newly report, yet it kept dispatching the same
    // rejected bytes. Identical failures end the pass instead.
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    let failure = match execute_model_tool_loop_approval_v1(
        &gateway(StuckUntilAnswering::new(observed.clone(), 500)),
        tool_request(&id, user_input()),
        &Stuck,
        &CancellationToken::default(),
    ) {
        Ok(_) => panic!("a provider that keeps refusing the same request ends the pass"),
        Err(failure) => failure,
    };
    assert!(
        failure.error.to_string().contains("quota exceeded"),
        "the provider's own failure is what gets reported: {failure}"
    );

    let turns = observed.lock().unwrap();
    assert_eq!(
        turns.len(),
        crate::runtime::model_tool_loop::MAXIMUM_IDENTICAL_PROVIDER_FAILURES as usize,
        "the loop stops at the threshold instead of dispatching the same bytes again"
    );
    assert!(
        failure
            .error
            .to_string()
            .contains("refused this exact request"),
        "the reported failure explains why the pass stopped: {failure}"
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
fn every_failure_is_reported_and_no_report_budget_can_end_the_node() {
    let mut request = ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: user_input(),
        parameters: BTreeMap::new(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        retry_notice: None,
    };
    let mut ledger = ProviderRecoveryBudget::default();
    let bound = ProviderError::InputBoundExceeded {
        input_bytes: 8,
        maximum_input_bytes: 4,
    };
    let first = ledger.note(&bound, &mut request);
    assert!(
        first.contains("exceeds what this model accepts"),
        "the first report carries the provider's own diagnostic: {first}"
    );
    let repeat = ledger.note(&bound, &mut request);
    assert!(
        repeat.contains("same provider failure repeated"),
        "an identical failure is reported as a repeat, not withheld: {repeat}"
    );
    // No number of reports exhausts the ledger or removes the notice.
    for index in 0..MAXIMUM_ERROR_RECOVERIES.saturating_add(8) {
        let notice = ledger.note(&ProviderError::Failed(format!("failure {index}")), &mut request);
        assert!(!notice.is_empty(), "a report is always available");
    }
    assert!(
        request.retry_notice.is_some(),
        "the frozen request always carries a notice"
    );
    assert!(
        request.retry_notice.as_deref().unwrap_or_default().len()
            <= aworkit_capability_host::MAX_RETRY_NOTICE_BYTES,
        "a notice is bounded, so it can never make the request invalid"
    );
}

/// A context authority that cannot prepare a turn at all.
struct BrokenContext;

impl ModelToolInvocationPortV1 for BrokenContext {
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
        Err("the context store is unavailable".into())
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

#[test]
fn a_context_preparation_failure_is_reported_instead_of_ending_the_node() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    let result = execute_model_tool_loop_approval_v1(
        &gateway(Answering::new(observed.clone())),
        tool_request(&id, user_input()),
        &BrokenContext,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| panic!("a context condition must not end the node: {failure}"));
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };
    assert_eq!(outcome.assistant_text, "Done with what I have.");

    let turns = observed.lock().unwrap();
    let notice = turns[0]
        .retry_notice
        .as_deref()
        .expect("the model is told about the context condition");
    assert!(notice.contains("Aworkit context notice"), "{notice}");
    assert!(notice.contains("context store is unavailable"), "{notice}");
}

/// A tool authority whose every invocation fails to settle.
struct FailingTool;

impl ModelToolInvocationPortV1 for FailingTool {
    fn invoke(
        &self,
        _: &StableId,
        _: u32,
        _: &ModelToolCallV1,
        _: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        Err("the shell host is unavailable".into())
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

/// Asks for one tool, then answers once it has seen the result.
struct ToolCallingThenAnswering {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    dispatches: AtomicUsize,
}

impl ToolCallingThenAnswering {
    fn new(observed: Arc<Mutex<Vec<ModelToolRequestV1>>>) -> Self {
        Self {
            observed,
            dispatches: AtomicUsize::new(0),
        }
    }
}

impl ProviderEnginePortV1 for ToolCallingThenAnswering {
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
            0 => emit(ModelToolEventV1::ToolCall { call: call("read0") })?,
            _ => emit(ModelToolEventV1::AssistantOutput {
                text: "Recovered from the failed tool.".into(),
            })?,
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

#[test]
fn a_tool_that_cannot_settle_is_reported_as_an_error_result_not_a_node_failure() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let id = StableId::parse("outer.test").unwrap();

    let result = execute_model_tool_loop_approval_v1(
        &gateway(ToolCallingThenAnswering::new(observed.clone())),
        tool_request(&id, user_input()),
        &FailingTool,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|failure| panic!("a tool error must not end the node: {failure}"));
    let ModelToolLoopRunV1::Completed(outcome) = result else {
        panic!("expected completion")
    };
    assert_eq!(outcome.assistant_text, "Recovered from the failed tool.");

    let turns = observed.lock().unwrap();
    let last = turns.last().unwrap();
    let exchange = last
        .exchanges
        .last()
        .expect("the failed exchange is carried to the next turn");
    assert_eq!(exchange.results.len(), 1);
    assert!(exchange.results[0].is_error, "the error is a result");
    assert!(
        exchange.results[0]
            .content
            .to_string()
            .contains("the shell host is unavailable"),
        "the failure reaches the model as data: {}",
        exchange.results[0].content
    );
    let notice = last
        .retry_notice
        .as_deref()
        .expect("the model is told about the failed tool");
    assert!(notice.contains("Aworkit tool notice"), "{notice}");
}

#[test]
fn an_oversized_notice_is_clamped_to_the_dispatch_bound() {
    let mut request = ModelToolRequestV1 {
        context_messages: Vec::new(),
        input: user_input(),
        parameters: BTreeMap::new(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        retry_notice: Some("x".repeat(64 * 1024)),
    };
    bound_request_notices(&mut request);
    let notice = request.retry_notice.expect("a notice survives the clamp");
    assert!(
        notice.len() <= aworkit_capability_host::MAX_RETRY_NOTICE_BYTES,
        "a notice is clamped to the dispatch bound: {} bytes",
        notice.len()
    );
    assert!(
        notice.contains("truncated"),
        "the clamp says the detail was dropped"
    );
}
