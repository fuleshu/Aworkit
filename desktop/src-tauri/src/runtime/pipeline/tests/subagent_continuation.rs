//! Fork isolation, child continuation, control and admission limits.
//!
//! Every case crosses the real gateway, broker, record store and filesystem
//! hosts: the assertions inspect the exact provider requests and committed
//! child frames rather than an internal shortcut.
use super::*;
use crate::runtime::approvals::ApprovalMode;
use crate::runtime::tool_loop::{
    SUBAGENT_CANCEL_CAPABILITY_ID, SUBAGENT_FORK_CAPABILITY_ID, SUBAGENT_LIST_CAPABILITY_ID,
    SUBAGENT_MESSAGE_CAPABILITY_ID, is_subagent_tool,
};
use crate::runtime::tool_registry;

/// One scripted parent/child dialogue shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    /// Delegate, then message the returned child with a follow-up.
    Continue,
    /// Read once, then fork the resulting conversation.
    Fork,
    /// Fork the same parent revision twice with the same task.
    ForkTwice,
    /// Delegate, then list, cancel, cancel again and message the closed child.
    Control,
    /// Two spawns in one turn with fan-out frozen to a single child.
    Fanout,
    /// One spawn with delegation depth frozen to zero.
    Depth,
    /// Message a child id this Agent never created.
    UnknownChild,
}

#[derive(Clone)]
struct ContinuationScenario {
    scenario: Scenario,
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
}

impl ProviderFactoryV1 for ContinuationScenario {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(ContinuationProvider {
            scenario: self.clone(),
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
        }))
    }
}

struct ContinuationProvider {
    scenario: ContinuationScenario,
    binding: String,
    version: String,
}

impl ProviderEnginePortV1 for ContinuationProvider {
    fn binding_id(&self) -> &str {
        &self.binding
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        _: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        panic!("Delegation must never invoke an approval reviewer")
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        _: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.scenario.requests.lock().unwrap().push(request.clone());
        let parent = request
            .tools
            .iter()
            .any(|tool| is_subagent_tool(&tool.capability_id));
        let exchanges = request.exchanges.len();
        match self.scenario.scenario {
            Scenario::Continue => {
                let continued = request
                    .context_messages
                    .iter()
                    .any(|message| message.content.contains("Continue the task."));
                if parent && exchanges == 0 {
                    emit(call(
                        "spawn",
                        SUBAGENT_CAPABILITY_ID,
                        json!({"task":"First task"}),
                    ))?;
                } else if parent && exchanges == 1 {
                    let child = delegated_child(request);
                    emit(call(
                        "message",
                        SUBAGENT_MESSAGE_CAPABILITY_ID,
                        json!({"childId":child,"message":"Continue the task."}),
                    ))?;
                } else if !parent && continued {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "child-second".into(),
                    })?;
                } else if !parent && exchanges == 0 {
                    emit(call(
                        "child-read",
                        FILE_READ_CAPABILITY_ID,
                        json!({"path":"notes.txt"}),
                    ))?;
                } else if !parent {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "child-first".into(),
                    })?;
                } else {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "parent-done".into(),
                    })?;
                }
            }
            Scenario::Fork | Scenario::ForkTwice => {
                if parent && exchanges == 0 {
                    emit(call(
                        "read",
                        FILE_READ_CAPABILITY_ID,
                        json!({"path":"notes.txt"}),
                    ))?;
                } else if parent && exchanges == 1 {
                    emit(call(
                        "fork-a",
                        SUBAGENT_FORK_CAPABILITY_ID,
                        json!({"task":"Same fork task"}),
                    ))?;
                    if self.scenario.scenario == Scenario::ForkTwice {
                        emit(call(
                            "fork-b",
                            SUBAGENT_FORK_CAPABILITY_ID,
                            json!({"task":"Same fork task"}),
                        ))?;
                    }
                } else if !parent {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "fork-child".into(),
                    })?;
                } else {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "parent-done".into(),
                    })?;
                }
            }
            Scenario::Control => {
                if parent && exchanges == 0 {
                    emit(call(
                        "spawn",
                        SUBAGENT_CAPABILITY_ID,
                        json!({"task":"Controlled task"}),
                    ))?;
                } else if parent && exchanges == 1 {
                    emit(call("list", SUBAGENT_LIST_CAPABILITY_ID, json!({})))?;
                } else if parent && (exchanges == 2 || exchanges == 3) {
                    let child = delegated_child_at(request, 0);
                    emit(call(
                        if exchanges == 2 { "cancel" } else { "cancel-again" },
                        SUBAGENT_CANCEL_CAPABILITY_ID,
                        json!({"childId":child}),
                    ))?;
                } else if parent && exchanges == 4 {
                    let child = delegated_child_at(request, 0);
                    emit(call(
                        "message",
                        SUBAGENT_MESSAGE_CAPABILITY_ID,
                        json!({"childId":child,"message":"Are you still there?"}),
                    ))?;
                } else if !parent {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "child-first".into(),
                    })?;
                } else {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "parent-done".into(),
                    })?;
                }
            }
            Scenario::Fanout | Scenario::Depth => {
                if parent && exchanges == 0 {
                    emit(call(
                        "spawn-one",
                        SUBAGENT_CAPABILITY_ID,
                        json!({"task":"First sibling"}),
                    ))?;
                    if self.scenario.scenario == Scenario::Fanout {
                        emit(call(
                            "spawn-two",
                            SUBAGENT_CAPABILITY_ID,
                            json!({"task":"Second sibling"}),
                        ))?;
                    }
                } else if !parent {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "child-first".into(),
                    })?;
                } else {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "parent-done".into(),
                    })?;
                }
            }
            Scenario::UnknownChild => {
                if parent && exchanges == 0 {
                    emit(call(
                        "message",
                        SUBAGENT_MESSAGE_CAPABILITY_ID,
                        json!({"childId":"child.subagent.missing","message":"Hello?"}),
                    ))?;
                } else {
                    emit(ModelToolEventV1::AssistantOutput {
                        text: "parent-done".into(),
                    })?;
                }
            }
        }
        emit(ModelToolEventV1::Usage {
            input_tokens: 5,
            output_tokens: 2,
            cache: Default::default(),
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

/// Builds a tool call using the installed provider name of the capability.
fn call(id: &str, tool: &str, arguments: Value) -> ModelToolEventV1 {
    let name = tool_registry::native_tool(tool)
        .unwrap()
        .provider_name
        .as_str();
    match tool_call(id, tool, name, arguments) {
        event @ ModelToolEventV1::ToolCall { .. } => event,
        _ => unreachable!(),
    }
}

/// Durable child id from the newest delegation result in the request.
fn delegated_child(request: &ModelToolRequestV1) -> String {
    delegated_child_at(request, request.exchanges.len() - 1)
}

fn delegated_child_at(request: &ModelToolRequestV1, exchange: usize) -> String {
    request.exchanges[exchange].results[0].content["childId"]
        .as_str()
        .expect("delegation returns a child id")
        .to_owned()
}

/// Builds one frozen built-in binding with optional configuration overrides.
fn native(id: &str, overrides: &[(&str, Value)]) -> WorkflowToolBindingV1 {
    let setting = tool_registry::native_defaults()
        .into_iter()
        .find(|tool| tool.id == id)
        .unwrap();
    let mut frozen = tool_registry::freeze_settings(&setting).unwrap();
    for (key, value) in overrides {
        frozen.configuration.insert((*key).into(), value.clone());
    }
    if is_subagent_tool(id) {
        frozen.options.approval_mode = Some(ApprovalMode::FullAccess);
        // This suite exercises the inline continuation path; background
        // children are covered by the dedicated background suite.
        frozen
            .configuration
            .insert("runInBackground".into(), json!(false));
    }
    WorkflowToolBindingV1 {
        capability_id: id.into(),
        configuration: serde_json::to_value(frozen.configuration).unwrap(),
        options: frozen.options,
        credential_bindings: Vec::new(),
        definition: None,
    }
}

type Tools<'a> = &'a [(&'a str, &'a [(&'a str, Value)])];

/// Prepares a Run whose Agent node binds exactly the supplied tools.
fn prepare(
    root: &TempDir,
    tools: Tools<'_>,
    scenario: Scenario,
) -> (
    WorkflowExecutionPipeline,
    WorkflowExecutionRequestV1,
    ContinuationScenario,
) {
    let project = subagent_project(root);
    let (mut pipeline, _, metadata, _, _) = setup(root, ScriptedBehavior::Succeed);
    let scenario = ContinuationScenario {
        scenario,
        requests: Arc::default(),
    };
    pipeline.provider_factory = Arc::new(scenario.clone());
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = tools
        .iter()
        .map(|(id, overrides)| native(id, overrides))
        .collect();
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] =
        json!(tools.iter().map(|(id, _)| *id).collect::<Vec<_>>());
    (pipeline, request, scenario)
}

/// Requests belonging to a delegated child conversation.
fn child_requests(scenario: &ContinuationScenario) -> Vec<ModelToolRequestV1> {
    scenario
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            !request
                .tools
                .iter()
                .any(|tool| is_subagent_tool(&tool.capability_id))
        })
        .cloned()
        .collect()
}

/// Requests belonging to the delegating Agent.
fn parent_requests(scenario: &ContinuationScenario) -> Vec<ModelToolRequestV1> {
    scenario
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            request
                .tools
                .iter()
                .any(|tool| is_subagent_tool(&tool.capability_id))
        })
        .cloned()
        .collect()
}

#[test]
fn continuation_resumes_the_same_child_prefix_across_turns() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(
        &root,
        &[
            (SUBAGENT_CAPABILITY_ID, &[]),
            (SUBAGENT_MESSAGE_CAPABILITY_ID, &[]),
            (FILE_READ_CAPABILITY_ID, &[]),
        ],
        Scenario::Continue,
    );
    let result = pipeline.execute(request.clone()).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let children = child_requests(&scenario);
    let continued = children
        .iter()
        .filter(|request| {
            request
                .context_messages
                .iter()
                .any(|message| message.content.contains("Continue the task."))
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(continued.len(), 1, "one continuation turn-set");
    // The immutable prompt prefix is byte-identical across every child turn,
    // and the child never inherits a delegation or control tool.
    assert!(
        children.iter().all(|request| request.input == children[0].input),
        "the child prompt prefix never changes between turns"
    );
    assert!(
        !continued[0].exchanges.is_empty(),
        "continuation seeds the child's committed head"
    );
    assert!(
        continued[0]
            .tools
            .iter()
            .all(|tool| !is_subagent_tool(&tool.capability_id)),
        "a child can never delegate or control a sibling"
    );
    assert!(
        continued[0]
            .tools
            .iter()
            .any(|tool| tool.capability_id == FILE_READ_CAPABILITY_ID)
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let handoff = &final_turn.exchanges[1].results[0].content;
    assert_eq!(handoff["status"], "completed", "{handoff}");
    assert_eq!(handoff["finalText"], "child-second");
    assert_eq!(handoff["headRevision"], 2);
    // Replaying the identical command reuses committed child work.
    let turns = scenario.requests.lock().unwrap().len();
    assert!(pipeline.execute(request).unwrap().replayed);
    assert_eq!(scenario.requests.lock().unwrap().len(), turns);
}

#[test]
fn fork_inherits_a_declared_projection_and_stays_reproducible() {
    for scenario_kind in [Scenario::Fork, Scenario::ForkTwice] {
        let root = TempDir::new().unwrap();
        let (pipeline, request, scenario) = prepare(
            &root,
            &[
                (SUBAGENT_FORK_CAPABILITY_ID, &[]),
                (FILE_READ_CAPABILITY_ID, &[]),
            ],
            scenario_kind,
        );
        let result = pipeline.execute(request).unwrap();
        assert_eq!(
            result.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        let children = child_requests(&scenario);
        assert_eq!(
            children.len(),
            if scenario_kind == Scenario::ForkTwice {
                2
            } else {
                1
            }
        );
        let inherited = children[0].input.to_string();
        assert!(
            inherited.contains("Inherited parent conversation"),
            "{inherited}"
        );
        assert!(
            inherited.contains("alpha beta alpha"),
            "the parent's committed read result is inherited: {inherited}"
        );
        let parent = parent_requests(&scenario);
        let handoff = &parent.last().unwrap().exchanges[1].results[0].content;
        assert_eq!(handoff["kind"], "fork", "{handoff}");
        assert_eq!(handoff["status"], "completed");
        if scenario_kind == Scenario::ForkTwice {
            // The same parent revision and the same declared bounds always
            // produce the same child prefix.
            assert_eq!(children[0].input, children[1].input);
            assert_eq!(children[0].tools, children[1].tools);
        }
    }
}

#[test]
fn fork_projection_is_bounded_by_frozen_message_limit() {
    let root = TempDir::new().unwrap();
    let maximum = json!(1);
    let (pipeline, request, scenario) = prepare(
        &root,
        &[
            (SUBAGENT_FORK_CAPABILITY_ID, &[("forkMaximumItems", maximum)]),
            (FILE_READ_CAPABILITY_ID, &[]),
        ],
        Scenario::Fork,
    );
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let children = child_requests(&scenario);
    let inherited = children[0].input.to_string();
    assert!(
        inherited.contains("1 item(s) kept") && inherited.contains("omitted"),
        "the declared bound must drop older items: {inherited}"
    );
}

#[test]
fn child_controls_list_cancel_idempotently_and_report_a_closed_scope() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(
        &root,
        &[
            (SUBAGENT_CAPABILITY_ID, &[]),
            (SUBAGENT_LIST_CAPABILITY_ID, &[]),
            (SUBAGENT_MESSAGE_CAPABILITY_ID, &[]),
            (SUBAGENT_CANCEL_CAPABILITY_ID, &[]),
            (FILE_READ_CAPABILITY_ID, &[]),
        ],
        Scenario::Control,
    );
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let listed = &final_turn.exchanges[1].results[0].content;
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["children"][0]["status"], "completed");
    assert_eq!(
        final_turn.exchanges[2].results[0].content["status"], "cancelled",
        "{:?}",
        final_turn.exchanges[2].results[0]
    );
    // Repeating the cancel is a no-op success, not an error.
    assert!(!final_turn.exchanges[3].results[0].is_error);
    assert_eq!(
        final_turn.exchanges[3].results[0].content["status"],
        "cancelled"
    );
    // A cancelled child cannot be continued, and messaging it is not an error.
    assert!(!final_turn.exchanges[4].results[0].is_error);
    assert_eq!(
        final_turn.exchanges[4].results[0].content["status"],
        "cancelled"
    );
    assert_eq!(
        scenario.requests.lock().unwrap().len(),
        parent.len() + 1,
        "a cancelled child runs no further turn"
    );
}

#[test]
fn unknown_child_ids_are_refused_before_any_child_work() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(
        &root,
        &[(SUBAGENT_MESSAGE_CAPABILITY_ID, &[])],
        Scenario::UnknownChild,
    );
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let refusal = &parent.last().unwrap().exchanges[0].results[0];
    assert!(refusal.is_error, "{refusal:?}");
    assert!(
        refusal.content["error"]
            .as_str()
            .is_some_and(|error| error.contains("not owned")),
        "{refusal:?}"
    );
    assert!(child_requests(&scenario).is_empty());
}

#[test]
fn fanout_exhaustion_is_refused_before_the_second_child_exists() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(
        &root,
        &[
            (
                SUBAGENT_CAPABILITY_ID,
                &[("maximumChildren", json!(1))],
            ),
            (FILE_READ_CAPABILITY_ID, &[]),
        ],
        Scenario::Fanout,
    );
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let results = &parent.last().unwrap().exchanges[0].results;
    assert_eq!(results.len(), 2, "{results:?}");
    let refused = results.iter().filter(|result| result.is_error).count();
    assert_eq!(refused, 1, "{results:?}");
    assert!(
        results
            .iter()
            .any(|result| result.content["error"]
                .as_str()
                .is_some_and(|error| error.contains("fan-out"))),
        "{results:?}"
    );
    assert_eq!(child_requests(&scenario).len(), 1);
}

#[test]
fn depth_exhaustion_is_refused_before_any_child_exists() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(
        &root,
        &[(
            SUBAGENT_CAPABILITY_ID,
            &[("maximumDepth", json!(0))],
        )],
        Scenario::Depth,
    );
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let result = &parent.last().unwrap().exchanges[0].results[0];
    assert!(result.is_error, "{result:?}");
    assert!(
        result.content["error"]
            .as_str()
            .is_some_and(|error| error.contains("delegation depth")),
        "{result:?}"
    );
    assert!(child_requests(&scenario).is_empty());
}
