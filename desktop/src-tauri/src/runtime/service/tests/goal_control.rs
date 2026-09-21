use super::*;

/// The desktop user owns the Chat goal: setting it records the durable snapshot
/// the next Agent pass injects and commits the canonical fact the timeline
/// folds, and clearing it abandons the goal without completing it.
#[test]
fn user_sets_and_clears_the_chat_goal() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider);
    configure(&mut desktop);
    let before = desktop.snapshot(0).unwrap();
    let target = before.chat.chat_id.clone();
    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "goal.set".into(),
            expected_version: before.version,
            action: "set_goal".into(),
            target_id: Some(target.clone()),
            payload: json!({"goal": "Ship the goal UI", "clear": false}),
        })
        .unwrap();

    let after = desktop.snapshot(0).unwrap();
    let fact = after
        .events
        .iter()
        .rev()
        .find(|event| event.kind == "tool.goal")
        .expect("the user goal commits a canonical fact");
    assert_eq!(fact.payload["goal"]["status"], "active");
    assert_eq!(fact.payload["goal"]["goal"], "Ship the goal UI");

    // The durable snapshot is exactly what a later Agent pass reads.
    let run_id = StableId::parse(after.chat.run_id.clone()).expect("run identity");
    assert_eq!(
        desktop
            .pipeline
            .run_goal_state(&run_id)
            .expect("goal state")
            .expect("stored goal")["goal"],
        "Ship the goal UI"
    );

    // Clearing records a cleared snapshot rather than a completed goal.
    desktop
        .command(UiCommandInput {
            schema_version: 1,
            command_id: "goal.clear".into(),
            expected_version: after.version,
            action: "set_goal".into(),
            target_id: Some(target),
            payload: json!({"goal": "", "clear": true}),
        })
        .unwrap();
    let cleared = desktop.snapshot(0).unwrap();
    let fact = cleared
        .events
        .iter()
        .rev()
        .find(|event| event.kind == "tool.goal")
        .expect("the clear commits a canonical fact");
    assert_eq!(fact.payload["goal"]["status"], "cleared");
}

/// An empty set and an oversized objective are rejected before anything is
/// recorded, so a malformed user command cannot corrupt the durable goal.
#[test]
fn user_goal_input_is_bounded() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut desktop = runtime(&root, provider);
    configure(&mut desktop);
    let before = desktop.snapshot(0).unwrap();
    let rejected = desktop.command(UiCommandInput {
        schema_version: 1,
        command_id: "goal.empty".into(),
        expected_version: before.version,
        action: "set_goal".into(),
        target_id: Some(before.chat.chat_id.clone()),
        payload: json!({"goal": "   ", "clear": false}),
    });
    assert!(rejected.is_err());
    assert!(
        desktop
            .snapshot(0)
            .unwrap()
            .events
            .iter()
            .all(|event| event.kind != "tool.goal")
    );
}
