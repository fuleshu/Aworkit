use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_local_store::PortableRuntimeJournal;
use aworkit_portable_store::{PortableCommit, PortableEvent, PortablePaths, PortableRepository};
use aworkit_trusted_core::PortableCommitGate;
use serde_json::json;

fn root() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aworkit-m06-gate-{nonce}"));
    fs::create_dir_all(&path).expect("root");
    path
}

#[test]
fn acknowledgement_requires_a_matching_linked_runtime_journal() {
    let root = root();
    let repository = PortableRepository::new(PortablePaths::open(&root).expect("paths"));
    let journal = PortableRuntimeJournal::open(root.join("runtime.sqlite")).expect("journal");
    let gate = PortableCommitGate::new(repository, journal, "machine-a", 7);
    let commit = PortableCommit {
        branch_id: "branch-1".into(),
        expected_generation: 0,
        commit_id: "commit-1".into(),
        events: vec![PortableEvent {
            event_id: "event-1".into(),
            chat_id: "chat-1".into(),
            branch_id: "branch-1".into(),
            ordinal: 0,
            kind: "message".into(),
            payload: json!({"text":"safe"}),
        }],
        checkpoint: None,
    };
    let receipt = gate.commit(&commit).expect("gated commit");
    assert!(
        gate.recovery_facts("commit-1", &receipt.head_segment_hash)
            .expect("facts")
            .resumable
    );
    assert!(
        gate.recovery_facts("commit-1", "sha256:not-the-head")
            .expect("facts")
            .quarantined
    );
    fs::remove_dir_all(root).expect("cleanup");
}
