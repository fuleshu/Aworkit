//! Question decisions remain readable through the durable command journal.
use super::*;

fn record(payload: Value) -> PendingChatCommandV1 {
    let command = UiCommandInput {
        schema_version: 1,
        command_id: "question.answer".into(),
        expected_version: 0,
        action: "question".into(),
        target_id: Some("chat.fixture".into()),
        payload,
    };
    PendingChatCommandV1 {
        schema_version: 1,
        frozen_context_hash: format!("sha256:{}", "a".repeat(64)),
        command_hash: canonical_hash(&command).unwrap(),
        command,
    }
}

#[test]
fn question_decisions_survive_journal_reopen_without_ordinary_input() {
    for payload in [
        json!({"questionId": "question.fixture", "optionId": "beta"}),
        json!({"questionId": "question.fixture", "freeText": "Use preview"}),
        json!({"questionId": "question.fixture", "path": "C:/project"}),
        json!({"questionId": "question.fixture", "cancelled": true}),
    ] {
        let root = TempDir::new().unwrap();
        let pending = record(payload);
        let port = Arc::new(SwitchableEventPort {
            fail: AtomicBool::new(false),
            delivered: Mutex::new(Vec::new()),
        });
        let history = ChatHistory::open_with_committed_events(root.path(), port.clone()).unwrap();
        history.stage_effect_command(pending.clone()).unwrap();
        drop(history);
        let reopened = ChatHistory::open_with_committed_events(root.path(), port).unwrap();
        assert_eq!(
            reopened.stage_effect_command(pending.clone()).unwrap(),
            pending
        );
    }
}

#[test]
fn question_journal_rejects_missing_identity_or_answer() {
    for payload in [
        json!({"optionId": "beta"}),
        json!({"questionId": "invalid question id", "cancelled": true}),
        json!({"questionId": "question.fixture"}),
        json!({"questionId": "question.fixture", "freeText": "  "}),
    ] {
        assert!(validate_pending_command_record(&record(payload)).is_err());
    }
}
