//! Stop resumes the interrupted graph node; completion starts a fresh pass.
use super::*;
use crate::runtime::semantic_events::{SemanticEventCommitter, SemanticEventDraft};

#[derive(Default)]
struct SteeringControl {
    stop_at: AtomicUsize,
    inputs: Mutex<Vec<Value>>,
}
struct SteeringFactory(Arc<SteeringControl>);
struct SteeringProvider {
    binding: String,
    version: String,
    control: Arc<SteeringControl>,
}
impl ProviderFactoryV1 for SteeringFactory {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        _: &StoredProviderBindingV1,
        _: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        Ok(Box::new(SteeringProvider {
            binding: descriptor.capability_id.clone(),
            version: descriptor.version_hash.clone(),
            control: self.0.clone(),
        }))
    }
}
impl ProviderEnginePortV1 for SteeringProvider {
    fn binding_id(&self) -> &str {
        &self.binding
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.execute_cancellable(request, &CancellationToken::default(), emit)
    }
    fn execute_cancellable(
        &self,
        request: &ModelRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        let index = {
            let mut inputs = self.control.inputs.lock().unwrap();
            inputs.push(request.input.clone());
            inputs.len()
        };
        if index == self.control.stop_at.load(Ordering::SeqCst) {
            cancellation.cancel();
            return Err(ProviderError::Cancelled);
        }
        emit(ModelEventV1::AssistantOutput(
            "Preserved plan and answer".into(),
        ))?;
        emit(ModelEventV1::Usage {
            input_tokens: 12,
            output_tokens: 6,
        })?;
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

fn record_message(events: &Arc<dyn SemanticEventCommitter>, text: &str) {
    events
        .commit(vec![SemanticEventDraft::new(
            "message.user",
            json!({"body":text}),
        )])
        .unwrap();
}

fn verify_steering(stop_at: usize, node_id: &str) {
    let root = TempDir::new().unwrap();
    let (initial, credentials, metadata, _, _) = setup(&root, ScriptedBehavior::Succeed);
    let events = initial.event_committer.clone();
    drop(initial);
    let control = Arc::new(SteeringControl::default());
    control.stop_at.store(stop_at, Ordering::SeqCst);
    let factory = Arc::new(SteeringFactory(control.clone()));
    let pipeline =
        WorkflowExecutionPipeline::compose(root.path(), credentials.clone(), factory.clone())
            .unwrap()
            .with_chat_events(events.clone());
    let first = graph_request(metadata);
    record_message(&events, &first.messages[0].content);
    let interrupted = pipeline.execute(first.clone()).unwrap();
    assert_ne!(interrupted.status, WorkflowExecutionStatusV1::Succeeded);
    assert_eq!(
        pipeline.stopped_node(&first.request_id).unwrap().as_deref(),
        Some(node_id)
    );
    let saved = pipeline
        .records
        .stopped_pass(&first.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.completed.contains(&"plan.1".into()), stop_at == 2);
    drop(pipeline);

    // Both the stopped pass and its frozen authority survive a new pipeline.
    let pipeline = WorkflowExecutionPipeline::compose(root.path(), credentials, factory)
        .unwrap()
        .with_chat_events(events.clone());
    let mut steer = first.clone();
    steer.request_id = stable("command.steer-one").unwrap();
    steer.steer_from_request_id = Some(first.request_id.clone());
    steer.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Steer once".into(),
        images: vec![],
    });
    let mut foreign = steer.clone();
    foreign.chat_id = stable("chat.foreign").unwrap();
    assert!(pipeline.preflight(&foreign).is_err());
    let mut changed = steer.clone();
    changed.frozen_context_hash = format!("sha256:{}", "c".repeat(64));
    assert!(pipeline.preflight(&changed).is_err());
    record_message(&events, "Steer once");
    control.stop_at.store(stop_at + 1, Ordering::SeqCst);
    let interrupted_again = pipeline.execute(steer.clone()).unwrap();
    assert_eq!(
        pipeline.stopped_node(&steer.request_id).unwrap().as_deref(),
        Some(node_id)
    );
    assert_eq!(
        interrupted_again
            .node_activity
            .iter()
            .find(|a| a.status == "started")
            .unwrap()
            .node_id,
        node_id
    );

    let mut twice = steer.clone();
    twice.request_id = stable("command.steer-two").unwrap();
    twice.steer_from_request_id = Some(steer.request_id.clone());
    twice.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Steer twice".into(),
        images: vec![],
    });
    record_message(&events, "Steer twice");
    let completed = pipeline.execute(twice.clone()).unwrap();
    assert_eq!(
        completed.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(
        completed
            .node_activity
            .iter()
            .find(|a| a.status == "started")
            .unwrap()
            .node_id,
        node_id
    );
    assert_eq!(completed.snapshot_hash, interrupted.snapshot_hash);
    assert_eq!(
        completed.authority_manifest_id,
        interrupted.authority_manifest_id
    );
    assert_eq!(pipeline.stopped_node(&twice.request_id).unwrap(), None);
    assert!(pipeline.execute(twice.clone()).unwrap().replayed);
    for (offset, steering_text) in [(stop_at, "Steer once"), (stop_at + 1, "Steer twice")] {
        let inputs = control.inputs.lock().unwrap();
        let messages = inputs[offset]["messages"].as_array().unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["content"] == steering_text)
                .count(),
            1
        );
    }
    let mut follow_up = twice.clone();
    follow_up.request_id = stable("command.normal-followup").unwrap();
    follow_up.steer_from_request_id = None;
    follow_up.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "A new task".into(),
        images: vec![],
    });
    record_message(&events, "A new task");
    let fresh = pipeline.execute(follow_up).unwrap();
    assert_eq!(
        fresh.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        fresh.error
    );
    assert_eq!(
        fresh
            .node_activity
            .iter()
            .filter(|a| a.status == "started")
            .map(|a| a.node_id.as_str())
            .collect::<Vec<_>>(),
        ["input.1", "plan.1", "agent.1", "output.1", "wait.1"]
    );
}

#[test]
fn stopped_agent_resumes_without_replanning_after_restart_and_repeated_stop() {
    verify_steering(2, "agent.1");
}

#[test]
fn stopped_planner_receives_steering_before_continuing_the_workflow() {
    verify_steering(1, "plan.1");
}
