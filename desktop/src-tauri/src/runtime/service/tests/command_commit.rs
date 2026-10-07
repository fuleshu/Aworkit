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
