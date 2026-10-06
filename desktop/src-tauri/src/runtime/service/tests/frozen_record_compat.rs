//! Profile-level compatibility QA for stored frozen records (task 184, P0).
//!
//! The acceptance case is a profile that outlives the build which wrote it: one
//! stored record this build cannot read must not stop the app opening, listing
//! its Chats, or starting a Chat whose turn completes.
use super::*;

#[test]
fn a_profile_with_one_unreadable_frozen_record_still_opens_lists_and_starts() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider.clone());
    configure(&mut desktop);
    // One stored Chat so the profile has recoverable history to list after the
    // unreadable record below is written.
    desktop.command(send("compat.seed", 0, "seed topic")).unwrap();
    // One record this build cannot read, written for an unrelated Chat.
    desktop
        .history
        .stage_stored_session_event_for_test(
            "chat.execution-context-frozen",
            json!({
                "schemaVersion": 1,
                "record": {
                    "context": {
                        "schemaVersion": 1,
                        "identity": {"chatId": "chat.unreadable", "runId": "run.unreadable"},
                    },
                    "contextHash": format!("sha256:{}", "c".repeat(64)),
                },
            }),
        )
        .unwrap();
    drop(desktop);

    // The app opens again over that profile and still lists its Chats.
    let mut desktop = runtime(&root, provider.clone());
    let listed = desktop.snapshot(0).unwrap();
    assert!(!listed.history.is_empty(), "Chats are still listed");

    // A new Chat starts and one stub-provider turn completes.
    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "compat.new-chat".into(),
            expected_version: desktop.history.head().unwrap(),
            action: "new_chat".into(),
            target_id: None,
            payload: json!({}),
        })
        .unwrap();
    desktop.command(send("compat.start", 0, "hello")).unwrap();
    let started = desktop.snapshot(0).unwrap();
    assert_eq!(
        started
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "message.assistant")
            .and_then(|event| event.payload.get("body"))
            .and_then(Value::as_str),
        Some("fixture: hello"),
        "one stub-provider turn completes despite the unreadable record"
    );
}
