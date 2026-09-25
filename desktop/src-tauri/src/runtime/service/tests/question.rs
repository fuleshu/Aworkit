//! The desktop command boundary must accept answers from a lagging projection.
use super::*;

fn setup() -> (TempDir, DesktopRuntime, Arc<QuestionPipeline>) {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline {
        provider,
        result: Mutex::new(None),
        answers: Mutex::new(Vec::new()),
        fail_resume: AtomicBool::new(false),
    });
    core.pipeline = pipeline.clone();
    core.command(send("question.start", 0, "Ask me")).unwrap();
    (root, core, pipeline)
}

fn action(core: &DesktopRuntime, id: &str, kind: &str, payload: Value) -> UiCommandInput {
    UiCommandInput {
        schema_version: 1,
        command_id: id.into(),
        expected_version: core.history.head().unwrap(),
        action: kind.into(),
        target_id: None,
        payload,
    }
}

#[test]
fn interrupted_question_recovery_retries_failed_resume_and_persists_its_receipt() {
    let (root, mut core, pipeline) = setup();
    let answer = action(
        &core,
        "answer",
        "question",
        json!({"questionId":"question.fixture","optionId":"beta"}),
    );
    pipeline.fail_resume.store(true, Ordering::SeqCst);
    assert!(core.command(answer).unwrap_err().contains("simulated"));
    assert!(core.snapshot(0).unwrap().chat.recovery_pending);
    let resume = action(&core, "resume", "resume", json!({}));
    pipeline.fail_resume.store(true, Ordering::SeqCst);
    assert!(
        core.command(resume.clone())
            .unwrap_err()
            .contains("simulated")
    );
    assert!(core.snapshot(0).unwrap().chat.recovery_pending);
    let receipt = core.command(resume.clone()).unwrap();
    assert_eq!(receipt.command_id, "resume");
    assert!(!core.snapshot(0).unwrap().chat.recovery_pending);
    drop(core);
    let mut reopened = runtime(&root, pipeline.provider.clone());
    reopened.pipeline = pipeline.clone();
    let replayed = reopened.command(resume).unwrap();
    assert_eq!(replayed.command_id, receipt.command_id);
    assert_eq!(replayed.current_version, receipt.current_version);
    assert!(replayed.accepted);
    assert_eq!(pipeline.answers.lock().unwrap().len(), 1);
    assert_eq!(
        reopened
            .snapshot(0)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.kind == "question.answered")
            .count(),
        1
    );
}

#[test]
fn stop_closes_question_and_legacy_stops_reject_late_answers_without_staging() {
    for legacy in [false, true] {
        let (_root, mut core, pipeline) = setup();
        if legacy {
            core.history
                .append(
                    "legacy.stop",
                    "hash",
                    core.history.head().unwrap(),
                    vec![("chat.turn_stopped", json!({"createdAt":"now"}))],
                )
                .unwrap();
        } else {
            core.command(action(&core, "stop", "cancel", json!({})))
                .unwrap();
            assert!(
                core.snapshot(0)
                    .unwrap()
                    .events
                    .iter()
                    .any(|event| event.kind == "question.cancelled")
            );
        }
        let head = core.history.head().unwrap();
        assert_eq!(core.snapshot(0).unwrap().chat.phase, "waiting_input");
        let answer = action(
            &core,
            "late.answer",
            "question",
            json!({"questionId":"question.fixture","optionId":"beta"}),
        );
        assert!(
            core.command(answer)
                .unwrap_err()
                .contains("no longer waiting")
        );
        assert_eq!(core.history.head().unwrap(), head);
        assert!(!core.snapshot(0).unwrap().chat.recovery_pending);
        assert!(pipeline.answers.lock().unwrap().is_empty());
    }
}

#[test]
fn recovery_abandons_question_approval_and_maintenance_without_text_or_effects() {
    for (kind, payload) in [
        (
            "question",
            json!({"questionId":"question.fixture","optionId":"beta"}),
        ),
        (
            "approval",
            json!({"decisionId":"approval.fixture","approved":true}),
        ),
        (
            "compact_context",
            json!({"nodeId":"agent.1","baseSequence":1}),
        ),
    ] {
        let (_root, mut core, pipeline) = setup();
        let command = action(&core, "interrupted", kind, payload);
        let frozen = core.history.current_frozen_context().unwrap().unwrap();
        core.history
            .stage_effect_command(PendingChatCommandV1 {
                schema_version: 1,
                frozen_context_hash: frozen.context_hash,
                command_hash: command_fingerprint(&command).unwrap(),
                command,
            })
            .unwrap();
        let before = core.snapshot(0).unwrap();
        core.command(action(
            &core,
            "stop.recovery",
            "abandon_recovery",
            json!({}),
        ))
        .unwrap();
        let after = core.snapshot(0).unwrap();
        assert!(!after.chat.recovery_pending);
        assert_eq!(after.chat.phase, "waiting_input");
        assert_eq!(pipeline.provider.calls.load(Ordering::SeqCst), 1);
        assert!(pipeline.answers.lock().unwrap().is_empty());
        assert_eq!(
            after
                .events
                .iter()
                .filter(|event| event.kind == "message.user")
                .count(),
            before
                .events
                .iter()
                .filter(|event| event.kind == "message.user")
                .count()
        );
    }
}

struct QuestionPipeline {
    provider: Arc<FixtureProvider>,
    result: Mutex<Option<WorkflowExecutionResultV1>>,
    answers: Mutex<Vec<Value>>,
    fail_resume: AtomicBool,
}

impl WorkflowPipelinePort for QuestionPipeline {
    fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, String> {
        let mut result = FixtureWorkflowPipeline {
            provider: self.provider.clone(),
            goal: Mutex::new(None),
        }
        .execute(request)?;
        *self.result.lock().unwrap() = Some(result.clone());
        result.status = WorkflowExecutionStatusV1::AwaitingAnswer;
        result.approval = Some(
            serde_json::from_value(json!({
                "decisionId": "question.fixture",
                "nodeId": "agent.1",
                "title": "Release channel",
                "message": "Choose a channel",
                "question": {
                    "kind": "choice",
                    "prompt": "Choose a channel",
                    "options": [{"id": "beta", "label": "Beta"}]
                }
            }))
            .unwrap(),
        );
        Ok(result)
    }

    fn validate_question_target(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &Value,
    ) -> Result<(), String> {
        let result = self.result.lock().unwrap();
        if question_id != "question.fixture"
            || result.as_ref().unwrap().chat_id.as_str() != chat_id
            || answer["optionId"] != "beta"
        {
            return Err("invalid question answer".into());
        }
        Ok(())
    }

    fn resume_question(
        &self,
        question_id: &str,
        chat_id: &str,
        answer: &Value,
    ) -> Result<WorkflowExecutionResultV1, String> {
        self.validate_question_target(question_id, chat_id, answer)?;
        if self.fail_resume.swap(false, Ordering::SeqCst) {
            return Err("simulated interrupted answer".into());
        }
        self.answers.lock().unwrap().push(answer.clone());
        Ok(self.result.lock().unwrap().clone().unwrap())
    }
}

#[test]
fn stale_question_answer_commits_once_and_resumes_its_chat() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider.clone());
    configure(&mut core);
    let pipeline = Arc::new(QuestionPipeline {
        provider,
        result: Mutex::new(None),
        answers: Mutex::new(Vec::new()),
        fail_resume: AtomicBool::new(false),
    });
    core.pipeline = pipeline.clone();
    core.command(send("question.start", 0, "Ask me")).unwrap();
    let waiting = core.snapshot(0).unwrap();
    assert_eq!(waiting.chat.phase, "awaiting_answer");
    let answer = UiCommandInput {
        schema_version: 1,
        command_id: "question.answer".into(),
        // Live question delivery can precede the snapshot which advances this.
        expected_version: 0,
        action: "question".into(),
        target_id: Some(waiting.chat.chat_id),
        payload: json!({"questionId": "question.fixture", "optionId": "beta"}),
    };
    let receipt = core.command(answer.clone()).expect("answer must commit");
    assert!(receipt.accepted);
    assert!(core.command(answer).unwrap().accepted);
    let settled = core.snapshot(0).unwrap();
    assert_eq!(settled.chat.phase, "waiting_input");
    assert!(!settled.chat.recovery_pending);
    assert_eq!(pipeline.answers.lock().unwrap().len(), 1);
    assert_eq!(
        settled
            .events
            .iter()
            .filter(|event| event.kind == "question.answered")
            .count(),
        1
    );
}
