//! One command may commit twice: it suspends at a gate and it later settles.
//!
//! Those two commits are different facts with different request hashes, so they
//! must not share one deduplication key. When they did, the store refused the
//! settle with `DeduplicationKeyReused` and an approval-gated run could commit
//! its whole graph and still never deliver its answer.
use super::*;

#[test]
fn a_suspension_and_its_settlement_commit_under_different_keys() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider);
    configure(&mut core);

    let head = core.history.head().unwrap();
    core.history
        .append_suspension(
            "command.fixture",
            "hash.suspend",
            head,
            vec![("approval.requested", json!({"createdAt":"1"}))],
        )
        .expect("the suspension commits");
    let head = core.history.head().unwrap();
    let receipt = core
        .history
        .append(
            "command.fixture",
            "hash.settle",
            head,
            vec![(
                "message.assistant",
                json!({"createdAt":"2","body":"the review answer"}),
            )],
        )
        .expect("the settle of a suspended command must commit");
    assert!(receipt.accepted);
    let snapshot = core.snapshot(0).unwrap();
    assert!(
        snapshot
            .events
            .iter()
            .any(|event| event.kind == "message.assistant"),
        "the answer reaches the Chat"
    );
    assert!(
        snapshot
            .events
            .iter()
            .any(|event| event.kind == "approval.requested"),
        "the suspension is still recorded"
    );
}

#[test]
fn only_direct_children_of_a_span_are_reported_as_its_orphans() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider);
    configure(&mut core);

    let run_span = "span.run.run.fixture.request.fixture";
    let orphan = "span.agent-loop.request.fixture.span.run.run.fixture.request.fixture";
    let nested = "span.node.run.fixture.request.fixture.agent.1";
    let head = core.history.head().unwrap();
    core.history
        .append(
            "run.begin",
            "hash.begin",
            head,
            vec![
                (
                    "span.started",
                    json!({
                        "schemaVersion": 1,
                        "requestId": "request.fixture",
                        "runId": "run.fixture",
                        "spanId": run_span,
                        "parentSpanId": null,
                        "spanKind": "run",
                        "semanticRole": "run",
                        "title": "Run",
                        "status": "running",
                        "createdAt": "1",
                        "hasInput": false,
                        "input": null,
                    }),
                ),
                (
                    "span.started",
                    json!({
                        "schemaVersion": 1,
                        "requestId": "request.fixture",
                        "runId": "run.fixture",
                        "spanId": orphan,
                        "parentSpanId": run_span,
                        "spanKind": "agent_loop",
                        "semanticRole": "agent_loop",
                        "title": "Agent",
                        "status": "running",
                        "createdAt": "2",
                    }),
                ),
                (
                    "span.started",
                    json!({
                        "schemaVersion": 1,
                        "requestId": "request.fixture",
                        "runId": "run.fixture",
                        "spanId": nested,
                        "parentSpanId": run_span,
                        "spanKind": "graph_node",
                        "semanticRole": "agent",
                        "title": "Agent",
                        "status": "running",
                        "createdAt": "3",
                    }),
                ),
            ],
        )
        .unwrap();

    // A terminating Run closes what is open directly under it - including an Agent
    // loop that no Agent node owns, which is exactly what blocked the Run span
    // from terminating with "cannot terminate while child ... is open".
    let orphans = core
        .history
        .open_child_terminal_facts(run_span, "failed", "interrupted", "9")
        .unwrap();
    assert_eq!(orphans.len(), 2, "{orphans:?}");
    assert!(orphans.iter().any(|fact| fact["spanId"] == orphan));
    assert!(orphans.iter().any(|fact| fact["spanId"] == nested));
    assert!(
        !orphans.iter().any(|fact| fact["spanId"] == run_span),
        "a span is never its own child"
    );
    assert!(
        core.history
            .open_child_terminal_facts("span.absent", "failed", "interrupted", "9")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn one_deduplication_key_cannot_carry_two_different_commits() {
    let root = TempDir::new().unwrap();
    let provider = Arc::new(FixtureProvider::new());
    let mut core = runtime(&root, provider);
    configure(&mut core);

    let head = core.history.head().unwrap();
    core.history
        .append(
            "command.fixture",
            "hash.one",
            head,
            vec![("approval.requested", json!({"createdAt":"1"}))],
        )
        .unwrap();
    let head = core.history.head().unwrap();
    let error = core
        .history
        .append(
            "command.fixture",
            "hash.two",
            head,
            vec![("message.assistant", json!({"createdAt":"2","body":"answer"}))],
        )
        .unwrap_err();
    // The failure mode the ledger recorded: the second commit is refused, so the
    // run never settles. Suspensions therefore use their own key type.
    assert!(
        error.contains("cannot commit desktop Chat history"),
        "reusing one key for a different commit is refused: {error}"
    );
}
