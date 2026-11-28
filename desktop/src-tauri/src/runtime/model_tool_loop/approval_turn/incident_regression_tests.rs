//! Regression for the recorded incident `chat.c4dbf38e958dce7a2926a61926b9b86628776d72`:
//! six delegated children dispatched against a provider that answered 429 to
//! every request, and one child ran to 107 refusals while its parent stopped at
//! ten because the two loops counted different things.
//!
//! The tests pin the two halves the failure-policy amendment requires: a provider
//! answer is classified before anything is re-dispatched, a spend limit is
//! account-scoped, and the repeated-refusal guard counts a class rather than a
//! byte-identical payload. No test waits on the clock.

use super::*;
use crate::runtime::provider_limits::ProviderLimitRegistryV1;
use aworkit_capability_host::{
    ModelEventV1, ModelRequestV1, ModelToolEventV1, ProviderAcceptanceV1, ProviderEnginePortV1,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const CHAT: &str = "chat.c4dbf38e958dce7a2926a61926b9b86628776d72";
const SPEND_TEXT: &str = "You exceeded your spend-based rate limit";

fn child_request(id: &StableId) -> ModelToolLoopRequestV1<'_> {
    ModelToolLoopRequestV1 {
        agent_context: None,
        outer_invocation_id: id,
        input: json!({"messages":[{"role":"user","content":"Audit one batch of recipes."}]}),
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
        maximum_input_bytes: 64 * 1024,
        maximum_output_bytes: 1_000_000,
        maximum_tool_output_bytes: 64 * 1024,
        maximum_timeout_recoveries: 0,
    }
}

/// What the provider saw, readable after the gateway has taken ownership of the
/// provider itself.
#[derive(Clone, Default)]
struct Probe {
    observed: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    dispatches: Arc<AtomicUsize>,
}

impl Probe {
    fn new() -> Self {
        Self::default()
    }
    fn dispatches(&self) -> usize {
        self.dispatches.load(Ordering::SeqCst)
    }
    fn observed(&self) -> Vec<ModelToolRequestV1> {
        self.observed.lock().unwrap().clone()
    }
}

/// Answers each dispatch through a script, so a test can reproduce "429 to every
/// dispatch" or "throttle once, then work".
struct ScriptedProvider {
    probe: Probe,
    script: fn(usize) -> Result<(), ProviderError>,
}

impl ProviderEnginePortV1 for ScriptedProvider {
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
        self.probe.observed.lock().unwrap().push(request.clone());
        let dispatch = self.probe.dispatches.fetch_add(1, Ordering::SeqCst);
        (self.script)(dispatch)?;
        emit(ModelToolEventV1::AssistantOutput {
            text: "Batch audited.".into(),
        })?;
        emit(ModelToolEventV1::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

fn scripted(script: fn(usize) -> Result<(), ProviderError>) -> (Probe, FrozenModelGateway) {
    let probe = Probe::new();
    let gateway = FrozenModelGateway::new(vec![Box::new(ScriptedProvider {
        probe: probe.clone(),
        script,
    })]);
    (probe, gateway)
}

fn spend(_dispatch: usize) -> Result<(), ProviderError> {
    Err(ProviderError::HttpStatus {
        status: 429,
        detail: format!(": {SPEND_TEXT}"),
        retry_after_seconds: None,
    })
}

fn throttling_once_then_working(dispatch: usize) -> Result<(), ProviderError> {
    if dispatch == 0 {
        return Err(ProviderError::HttpStatus {
            status: 429,
            detail: ": Too many requests".into(),
            retry_after_seconds: Some(7),
        });
    }
    Ok(())
}

fn always_throttling(_dispatch: usize) -> Result<(), ProviderError> {
    Err(ProviderError::HttpStatus {
        status: 429,
        detail: ": Too many requests".into(),
        retry_after_seconds: None,
    })
}

/// The recorded child's payload changed with every retry, so a byte-identical
/// guard would never fire. The class is constant.
fn rotating_refusal(dispatch: usize) -> Result<(), ProviderError> {
    Err(ProviderError::Failed(format!(
        "upstream rejected attempt {dispatch}"
    )))
}

fn bad_credential(_dispatch: usize) -> Result<(), ProviderError> {
    Err(ProviderError::HttpStatus {
        status: 401,
        detail: ": invalid api key".into(),
        retry_after_seconds: None,
    })
}

fn answering(_dispatch: usize) -> Result<(), ProviderError> {
    Ok(())
}

/// The delegating Agent's view: a Chat-scoped provider-limit registry, the
/// classified answers recorded on the attempt, one-shot child reports, and the
/// pass-close hook.
struct Harness {
    limits: ProviderLimitRegistryV1,
    answers: Mutex<Vec<(String, String, String)>>,
    reports: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl Harness {
    fn new() -> Self {
        Self {
            limits: ProviderLimitRegistryV1::default(),
            answers: Mutex::new(Vec::new()),
            reports: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }
}

impl ModelToolInvocationPortV1 for Harness {
    fn provider_limit_notice(&self) -> Option<String> {
        self.limits.notice(CHAT)
    }
    fn record_provider_limit(&self, provider_text: &str) {
        self.limits.record(CHAT, provider_text);
    }
    fn note_provider_failure(&self, class: &str, provider_text: &str, reaction: &str) {
        self.answers.lock().unwrap().push((
            class.to_owned(),
            provider_text.to_owned(),
            reaction.to_owned(),
        ));
    }
    fn pending_delegation_reports(&self) -> Result<Option<String>, String> {
        Ok(self.reports.lock().unwrap().pop())
    }
    fn close_pass(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
    fn invoke(
        &self,
        _: &StableId,
        _: u32,
        _: &ModelToolCallV1,
        _: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        Err("this provider never requested a tool".into())
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
fn a_spend_limit_observed_by_one_child_stops_every_sibling() {
    // The incident topology: six delegated children, one provider that answers
    // the same account-scoped 429 to every dispatch.
    let (probe, gateway) = scripted(spend);
    let harness = Harness::new();
    let mut failures = Vec::new();
    for child in 0..6 {
        let id = StableId::parse(format!("invoke.child.{child}")).unwrap();
        let failure = match execute_model_tool_loop_v1(
            &gateway,
            child_request(&id),
            &harness,
            &CancellationToken::default(),
        ) {
            Ok(_) => panic!("a spend limit must end the pass"),
            Err(failure) => failure,
        };
        failures.push(failure.error.to_string());
    }
    // The first child's observation is the only one that reaches the provider:
    // the limit belongs to the account, so five siblings do not rediscover it.
    assert_eq!(
        probe.dispatches(),
        1,
        "the first spend-limit observation must stop its siblings"
    );
    for failure in &failures {
        assert!(
            failure.contains(SPEND_TEXT),
            "the provider's own text is surfaced: {failure}"
        );
        assert!(
            failure.contains("belongs to the provider account"),
            "the failure explains what a human must change: {failure}"
        );
    }
    // The class, the provider's text and the chosen reaction are on the attempt.
    let answers = harness.answers.lock().unwrap();
    assert_eq!(answers.len(), 1, "one attempt was classified, not six");
    assert_eq!(answers[0].0, "spend_or_quota_limit");
    assert!(answers[0].1.contains(SPEND_TEXT));
    assert_eq!(answers[0].2, "stopped: account spend or quota limit");
    // A parent that terminates leaves no live child behind.
    assert!(harness.closed.load(Ordering::SeqCst));
}

#[test]
fn a_rotating_payload_cannot_outrun_the_class_streak() {
    // The recorded child reached 107 refusals because its payload changed with
    // every retry. The guard counts the class, so a changing payload does not
    // reset it: the child stops at the same threshold as its parent.
    let (probe, gateway) = scripted(rotating_refusal);
    let harness = Harness::new();
    let id = StableId::parse("invoke.child.rotating").unwrap();
    let failure = match execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    ) {
        Ok(_) => panic!("a refusal class that never changes ends the pass"),
        Err(failure) => failure,
    };
    assert!(
        failure
            .error
            .to_string()
            .contains("repeated 10 times in a row"),
        "the class streak is what stops the loop: {failure}"
    );
    assert_eq!(
        probe.dispatches(),
        crate::runtime::model_tool_loop::MAXIMUM_IDENTICAL_PROVIDER_FAILURES as usize,
        "the pass ends at the class threshold instead of reaching 107"
    );
    assert_eq!(
        harness.answers.lock().unwrap().len(),
        crate::runtime::model_tool_loop::MAXIMUM_IDENTICAL_PROVIDER_FAILURES as usize,
        "every refusal was classified before the pass ended"
    );
}

#[test]
fn an_authentication_failure_is_not_retried_and_names_the_setting() {
    let (probe, gateway) = scripted(bad_credential);
    let harness = Harness::new();
    let id = StableId::parse("invoke.child.auth").unwrap();
    let failure = match execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    ) {
        Ok(_) => panic!("an unusable credential cannot be repaired by retrying"),
        Err(failure) => failure,
    };
    let text = failure.error.to_string();
    assert!(text.contains("invalid api key"), "{text}");
    assert!(text.contains("Check the provider binding"), "{text}");
    assert_eq!(probe.dispatches(), 1, "an authentication failure is not retried");
    let answers = harness.answers.lock().unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, "authentication_or_configuration");
    assert_eq!(answers[0].2, "stopped: configuration cannot be retried");
}

#[test]
fn a_rate_limit_honours_the_wait_it_names_then_dispatches_again() {
    let (probe, gateway) = scripted(throttling_once_then_working);
    let harness = Harness::new();
    let id = StableId::parse("invoke.child.throttled").unwrap();
    let outcome = execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    )
    .expect("a throttled provider that recovers lets the work continue");
    assert_eq!(outcome.assistant_text, "Batch audited.");
    assert_eq!(probe.dispatches(), 2, "one wait, then one re-dispatch");
    let answers = harness.answers.lock().unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, "rate_limit");
    assert_eq!(
        answers[0].2, "waited, then re-dispatched",
        "a rate limit waits instead of resending immediately"
    );
}

#[test]
fn a_rate_limit_that_never_clears_ends_with_the_provider_text() {
    let (probe, gateway) = scripted(always_throttling);
    let harness = Harness::new();
    let id = StableId::parse("invoke.child.wall").unwrap();
    let failure = match execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    ) {
        Ok(_) => panic!("a provider that only throttles ends the pass"),
        Err(failure) => failure,
    };
    let text = failure.error.to_string();
    assert!(text.contains("Too many requests"), "provider text: {text}");
    assert!(text.contains("kept throttling after"), "reaction: {text}");
    assert_eq!(
        probe.dispatches(),
        5,
        "one dispatch and four bounded waits, then the pass ends"
    );
}

#[test]
fn a_settled_child_reaches_the_delegating_model_as_one_bounded_report() {
    let (probe, gateway) = scripted(answering);
    let harness = Harness::new();
    harness.reports.lock().unwrap().push(
        "1 delegated child/children settled. child child.abc failed (7 model turn(s), 3 tool \
         call(s), 200 input / 40 output tokens). Outcome: the provider rejected the request"
            .to_owned(),
    );
    let id = StableId::parse("invoke.parent").unwrap();
    execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    )
    .expect("the delegating Agent still answers");

    // The report was delivered exactly once, on the next turn's frozen request.
    assert!(
        harness.reports.lock().unwrap().is_empty(),
        "a delivered report is not re-delivered"
    );
    let observed = probe.observed();
    let notice = observed
        .first()
        .and_then(|request| request.retry_notice.as_deref())
        .expect("the delegating model is told about the settled child");
    assert!(notice.contains("Aworkit delegation notice"), "{notice}");
    assert!(notice.contains("child.abc"), "{notice}");
    assert!(notice.contains("failed"), "{notice}");
    assert!(
        notice.len() <= aworkit_capability_host::MAX_RETRY_NOTICE_BYTES,
        "the report is bounded like every other notice"
    );
}

#[test]
fn a_terminal_answer_still_closes_the_pass() {
    let (_probe, gateway) = scripted(answering);
    let harness = Harness::new();
    let id = StableId::parse("invoke.parent.answer").unwrap();
    execute_model_tool_loop_v1(
        &gateway,
        child_request(&id),
        &harness,
        &CancellationToken::default(),
    )
    .expect("the model answers");
    assert!(
        harness.closed.load(Ordering::SeqCst),
        "a pass that ends for any reason closes its children"
    );
}
