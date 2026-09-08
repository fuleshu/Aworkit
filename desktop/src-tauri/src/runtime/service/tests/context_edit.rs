use super::*;
use crate::runtime::context_inspection::{ContextDocument, apply_edit, select_context};

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
