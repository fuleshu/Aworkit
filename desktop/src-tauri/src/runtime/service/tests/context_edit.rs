use super::*;
use crate::runtime::context_inspection::{ContextDocument, apply_edit, select_context};

#[test]
fn manual_compaction_settles_without_conversation_messages_and_fork_keeps_selection() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider.clone());
    configure(&mut desktop);
    desktop
        .command(send("compact.first", 0, "Original question"))
        .unwrap();
    let before = desktop.snapshot(0).unwrap();
    let document=ContextDocument::from_input(&json!({"messages":[{"role":"user","content":"<compacted-summary>Established context</compacted-summary>"}]})).unwrap();
    desktop
        .history
        .append(
            "compact.fixture",
            "compact.fixture.hash",
            before.version,
            vec![(
                "context.checkpoint",
                json!({"nodeId":"agent.1","child":null,"snapshot":{"document":document}}),
            )],
        )
        .unwrap();
    let before = desktop.snapshot(0).unwrap();
    let selection = select_context(&before.events, "agent.1").unwrap();
    let command = UiCommandInput {
        schema_version: 1,
        command_id: "compact.manual".into(),
        expected_version: before.version,
        action: "compact_context".into(),
        target_id: Some(before.chat.chat_id.clone()),
        payload: json!({"nodeId":"agent.1","baseSequence":selection.sequence}),
    };
    desktop.command(command.clone()).unwrap();
    desktop.command(command).unwrap();
    let after = desktop.snapshot(0).unwrap();
    assert!(!after.chat.recovery_pending);
    assert_eq!(after.chat.phase, "waiting_input");
    assert_eq!(
        after
            .events
            .iter()
            .filter(|e| e.kind == "message.user" || e.kind == "message.assistant")
            .count(),
        2
    );
    assert_eq!(
        after
            .events
            .iter()
            .filter(|e| e.kind == "context.manual-completed")
            .count(),
        1
    );
    let request = provider
        .execution_requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(request.compact_node.as_deref(), Some("agent.1"));
    assert_eq!(request.messages.last().unwrap().role, "user");
    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "compact.fork".into(),
            expected_version: after.version,
            action: "fork".into(),
            target_id: Some(after.chat.chat_id.clone()),
            payload: json!({}),
        })
        .unwrap();
    let child = desktop.snapshot(0).unwrap();
    assert_ne!(child.chat.chat_id, after.chat.chat_id);
    assert_eq!(
        select_context(&child.events, "agent.1").unwrap().document,
        document
    );
    drop(desktop);
    let reopened = runtime(&root, provider).snapshot(0).unwrap();
    assert_eq!(
        select_context(&reopened.events, "agent.1")
            .unwrap()
            .document,
        document
    );
    assert!(!reopened.chat.recovery_pending);
}

#[test]
fn context_edit_commits_once_reopens_and_preserves_conversation() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider.clone());
    configure(&mut desktop);
    desktop
        .command(send("context.first", 0, "Question"))
        .unwrap();
    let head = desktop.snapshot(0).unwrap().version;
    let input = json!({"messages":[{"role":"system","content":"Original prompt"},{"role":"user","content":"Question"}]});
    desktop.history.append("context.fixture", "context.fixture.hash", head, vec![
        ("span.started", json!({"spanId":"context.node","spanKind":"graph_node","nodeId":"agent.1"})),
        ("span.started", json!({"spanId":"context.model","spanKind":"model_call","parentSpanId":"context.node","input":input})),
        ("span.completed", json!({"spanId":"context.model","output":[{"kind":"assistant_output","text":"Original answer"}]})),
        ("span.completed", json!({"spanId":"context.node"})),
    ]).unwrap();
    let snapshot = desktop.snapshot(0).unwrap();
    let original_conversation = desktop.history.conversation().unwrap();
    let mut selection = select_context(&snapshot.events, "agent.1").unwrap();
    selection.document.input["messages"][0]["content"] = json!("Revised prompt");
    selection.document.context_messages[0].content = "Revised answer".into();
    let edit = UiCommandInput {
        schema_version: 1,
        command_id: "context.edit".into(),
        expected_version: snapshot.version,
        action: "edit_context".into(),
        target_id: Some(snapshot.chat.chat_id.clone()),
        payload: json!({"nodeId":"agent.1","baseSequence":selection.sequence,"document":selection.document}),
    };
    let receipt = desktop.command(edit.clone()).unwrap();
    assert!(receipt.accepted);
    assert_eq!(
        desktop.command(edit).unwrap().current_version,
        receipt.current_version
    );
    let contents = |messages: Vec<ConversationMessage>| {
        messages
            .into_iter()
            .map(|m| (m.role, m.content, m.images))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        contents(desktop.history.conversation().unwrap()),
        contents(original_conversation)
    );
    drop(desktop);
    let mut desktop = runtime(&root, provider);
    let snapshot = desktop.snapshot(0).unwrap();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|e| e.kind == "context.edited")
            .count(),
        1
    );
    let selection = select_context(&snapshot.events, "agent.1").unwrap();
    assert_eq!(
        selection.document.input["messages"][0]["content"],
        "Revised prompt"
    );
    let mut next = ContextDocument::from_input(&input).unwrap().request();
    apply_edit(&snapshot.events, "agent.1", &mut next).unwrap();
    assert_eq!(next.input["messages"][0]["content"], "Revised prompt");
    assert_eq!(next.context_messages[0].content, "Revised answer");
    let unchanged = UiCommandInput {
        schema_version: 1,
        command_id: "context.unchanged".into(),
        expected_version: snapshot.version,
        action: "edit_context".into(),
        target_id: Some(snapshot.chat.chat_id.clone()),
        payload: json!({"nodeId":"agent.1","baseSequence":selection.sequence,"document":selection.document}),
    };
    assert_eq!(
        desktop.command(unchanged.clone()).unwrap().current_version,
        snapshot.version
    );
    assert_eq!(
        desktop.command(unchanged.clone()).unwrap().current_version,
        snapshot.version
    );
    let mut reused = unchanged;
    reused.payload["document"]["input"]["messages"][0]["content"] =
        json!("Different content under same ID");
    assert!(desktop.command(reused).unwrap_err().contains("reused"));
    assert_eq!(desktop.snapshot(0).unwrap().version, snapshot.version);
    let stale = UiCommandInput {
        schema_version: 1,
        command_id: "context.stale".into(),
        expected_version: snapshot.version,
        action: "edit_context".into(),
        target_id: Some(snapshot.chat.chat_id),
        payload: json!({"nodeId":"agent.1","baseSequence":1,"document":selection.document}),
    };
    assert!(
        desktop
            .command(stale)
            .unwrap_err()
            .contains("Context changed")
    );
}
