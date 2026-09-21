//! A delegated child is a background job: the parent starts it, observes live
//! state, steers or stops it through the shared job tools, and can finish its
//! own turn while the child keeps running.
use super::*;
use crate::runtime::approvals::ApprovalMode;
use crate::runtime::tool_loop::{SUBAGENT_CAPABILITY_ID, SUBAGENT_FORK_CAPABILITY_ID, is_subagent_tool};
use crate::runtime::tool_registry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    /// Spawn, wait for the job, then finish.
    Query,
    /// Spawn, steer through job_input, then collect the steered outcome.
    Steer,
    /// Spawn a child that keeps working, stop it through job_stop.
    Stop,
    /// Spawn, keep the running child, and finish the parent's turn.
    Keep,
}

#[derive(Clone)]
struct BackgroundScenario {
    plan: Plan,
    requests: Arc<Mutex<Vec<ModelToolRequestV1>>>,
    /// Test-controlled release for the kept child, so the parent's turn is
    /// never racing the child's completion.
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl ProviderFactoryV1 for BackgroundScenario {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(BackgroundProvider {
            scenario: self.clone(),
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
        }))
    }
}

struct BackgroundProvider {
    scenario: BackgroundScenario,
    binding: String,
    version: String,
}

impl ProviderEnginePortV1 for BackgroundProvider {
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
        match self.scenario.plan {
            Plan::Query => {
                if parent && exchanges == 0 {
                    emit(spawn("delegate", json!({"task":"Background research task"})))?;
                } else if parent && exchanges == 1 {
                    emit(job_call(
                        "collect",
                        "tool.job.output",
                        json!({"jobId":delegated_job(request),"waitMs":5000}),
                    ))?;
                } else if !parent && exchanges == 0 {
                    emit(read("child-read"))?;
                } else if !parent {
                    emit(text("child-done"))?;
                } else {
                    emit(text("parent-done"))?;
                }
            }
            Plan::Steer => {
                let steered = request
                    .context_messages
                    .iter()
                    .any(|message| message.content.contains("PIVOT"));
                if parent && exchanges == 0 {
                    emit(spawn("delegate", json!({"task":"Steerable background task"})))?;
                } else if parent && exchanges == 1 {
                    emit(job_call(
                        "steer",
                        "tool.job.input",
                        json!({"jobId":delegated_job(request),"text":"PIVOT now"}),
                    ))?;
                } else if parent && exchanges == 2 {
                    emit(job_call(
                        "collect",
                        "tool.job.output",
                        json!({"jobId":delegated_job(request),"waitMs":5000}),
                    ))?;
                } else if !parent && steered {
                    emit(text("pivoted"))?;
                } else if !parent {
                    emit(read(&format!("child-read-{exchanges}")))?;
                } else {
                    emit(text("parent-done"))?;
                }
            }
            Plan::Stop => {
                if parent && exchanges == 0 {
                    emit(spawn("delegate", json!({"task":"Long background task"})))?;
                } else if parent && exchanges == 1 {
                    emit(job_call(
                        "stop",
                        "tool.job.stop",
                        json!({"jobId":delegated_job(request)}),
                    ))?;
                } else if parent && exchanges == 2 {
                    emit(job_call("list", "tool.subagent_list", json!({})))?;
                } else if !parent {
                    emit(read(&format!("child-read-{exchanges}")))?;
                } else {
                    emit(text("parent-done"))?;
                }
            }
            Plan::Keep => {
                if parent && exchanges == 0 {
                    emit(spawn("delegate", json!({"task":"Kept background task"})))?;
                } else if parent && exchanges == 1 {
                    emit(job_call(
                        "keep",
                        "tool.job.keep",
                        json!({"jobId":delegated_job(request),"reason":"long research"}),
                    ))?;
                } else if !parent {
                    while !self
                        .scenario
                        .release
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    emit(text("kept-child-done"))?;
                } else {
                    emit(text("parent-done"))?;
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

/// A background delegation call, explicitly overriding the default.
fn spawn(id: &str, arguments: Value) -> ModelToolEventV1 {
    let mut arguments = arguments;
    arguments["runInBackground"] = json!(true);
    call(id, SUBAGENT_CAPABILITY_ID, arguments)
}

fn read(id: &str) -> ModelToolEventV1 {
    call(
        id,
        FILE_READ_CAPABILITY_ID,
        json!({"path":"notes.txt"}),
    )
}

fn job_call(id: &str, tool: &str, arguments: Value) -> ModelToolEventV1 {
    call(id, tool, arguments)
}

fn text(value: &str) -> ModelToolEventV1 {
    ModelToolEventV1::AssistantOutput {
        text: value.into(),
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

/// The background job ticket returned by the newest delegation.
fn delegated_job(request: &ModelToolRequestV1) -> String {
    request.exchanges[0].results[0].content["jobId"]
        .as_str()
        .expect("background delegation returns a job id")
        .to_owned()
}

fn native(id: &str) -> WorkflowToolBindingV1 {
    let setting = tool_registry::native_defaults()
        .into_iter()
        .find(|tool| tool.id == id)
        .unwrap();
    let frozen = tool_registry::freeze_settings(&setting).unwrap();
    let mut frozen = frozen;
    frozen.options.approval_mode = Some(ApprovalMode::FullAccess);
    WorkflowToolBindingV1 {
        capability_id: id.into(),
        configuration: serde_json::to_value(frozen.configuration).unwrap(),
        options: frozen.options,
        credential_bindings: Vec::new(),
        definition: None,
    }
}

/// Every tool a background delegation test needs at once.
const TOOLS: &[&str] = &[
    SUBAGENT_CAPABILITY_ID,
    SUBAGENT_FORK_CAPABILITY_ID,
    "tool.subagent_list",
    "tool.job.output",
    "tool.job.input",
    "tool.job.stop",
    "tool.job.keep",
    "tool.job.list",
    FILE_READ_CAPABILITY_ID,
];

fn prepare(
    root: &TempDir,
    plan: Plan,
) -> (
    WorkflowExecutionPipeline,
    WorkflowExecutionRequestV1,
    BackgroundScenario,
) {
    let project = subagent_project(root);
    let (mut pipeline, _, metadata, _, _) = setup(root, ScriptedBehavior::Succeed);
    let scenario = BackgroundScenario {
        plan,
        requests: Arc::default(),
        release: Arc::default(),
    };
    pipeline.provider_factory = Arc::new(scenario.clone());
    let mut request = subagent_request(&pipeline, metadata, &project, &[]);
    request.approvals.mode = ApprovalMode::FullAccess;
    request.tools = TOOLS.iter().map(|id| native(id)).collect();
    request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(TOOLS);
    (pipeline, request, scenario)
}

fn parent_requests(scenario: &BackgroundScenario) -> Vec<ModelToolRequestV1> {
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
fn background_delegation_returns_a_ticket_and_the_parent_reads_live_state() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(&root, Plan::Query);
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let spawn = &final_turn.exchanges[0].results[0].content;
    assert!(spawn["jobId"].as_str().unwrap().starts_with("job."), "{spawn}");
    assert_eq!(spawn["running"], true, "{spawn}");
    assert_eq!(spawn["status"], "running", "{spawn}");
    // The parent kept working while the child ran, then collected the outcome.
    let collected = &final_turn.exchanges[1].results[0].content;
    assert_eq!(collected["kind"], "subagent", "{collected}");
    assert_eq!(collected["running"], false, "{collected}");
    assert_eq!(collected["status"], "completed", "{collected}");
    assert_eq!(collected["child"]["status"], "completed", "{collected}");
    assert_eq!(collected["child"]["finalText"], "child-done", "{collected}");
    assert!(
        collected["stdout"]
            .as_str()
            .unwrap()
            .contains("read_file"),
        "live per-turn progress reaches the job log: {collected}"
    );
}

#[test]
fn background_child_is_steered_at_its_next_step_boundary() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(&root, Plan::Steer);
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let steered = &final_turn.exchanges[1].results[0].content;
    assert_eq!(steered["mode"], "steered", "{steered}");
    let collected = &final_turn.exchanges[2].results[0].content;
    assert_eq!(collected["running"], false, "{collected}");
    assert_eq!(
        collected["child"]["finalText"], "pivoted",
        "the queued steering is what made the child finish: {collected}"
    );
}

#[test]
fn background_child_is_stopped_through_job_stop_and_its_scope_closes() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(&root, Plan::Stop);
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let stopped = &final_turn.exchanges[1].results[0].content;
    assert_eq!(stopped["running"], false, "{stopped}");
    assert_eq!(stopped["status"], "stopped", "{stopped}");
    let listed = &final_turn.exchanges[2].results[0].content;
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(
        listed["children"][0]["status"], "cancelled",
        "a stopped child scope is closed, not resumable: {listed}"
    );
}

#[test]
fn a_kept_background_child_lets_the_parent_finish_its_turn() {
    let root = TempDir::new().unwrap();
    let (pipeline, request, scenario) = prepare(&root, Plan::Keep);
    let result = pipeline.execute(request).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    let parent = parent_requests(&scenario);
    let final_turn = parent.last().unwrap();
    let kept = &final_turn.exchanges[1].results[0].content;
    assert_eq!(kept["kept"], true, "{kept}");
    assert_eq!(final_turn.exchanges.len(), 2, "the parent finished its turn");
    // Release the kept child only after the parent's turn is fully committed.
    scenario
        .release
        .store(true, std::sync::atomic::Ordering::Release);
    std::thread::sleep(Duration::from_millis(300));
}
