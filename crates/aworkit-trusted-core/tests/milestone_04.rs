use std::{fs, time::{SystemTime, UNIX_EPOCH}};

use aworkit_local_store::{CommitBatch, Event, LocalHistoryStore, OutboxEntry};
use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{CanonicalCommitter, ChatAggregate, ChatCommand, ChatState, CommitRequest, HistoryBinding, WorkerControl, WorkerSupervisor};
use serde_json::json;

fn id(value: &str) -> StableId { StableId::parse(value).expect("stable id") }
fn path(name: &str) -> std::path::PathBuf { std::env::temp_dir().join(format!("aworkit-m04-{name}-{}", SystemTime::now().duration_since(UNIX_EPOCH).expect("time").as_nanos())) }

#[test]
fn local_commit_is_ordered_deduplicated_and_delivered_after_commit() {
    let root = path("history");
    fs::create_dir_all(&root).expect("root");
    let store = LocalHistoryStore::open(root.join("history.sqlite")).expect("store");
    let committer = CanonicalCommitter::new(store.clone());
    let batch = CommitBatch { chat_id: "chat.1".into(), branch_id: "main".into(), expected_head: 0, events: vec![Event { event_id: "event.1".into(), kind: "started".into(), payload: json!({"snapshotHash":"frozen"}) }], attempt: None, checkpoint: None, deduplication: None, outbox: vec![OutboxEntry { outbox_id: "outbox.1".into(), destination: "worker".into(), payload: json!({"control":"resume"}) }] };
    committer.commit(&CommitRequest { binding: HistoryBinding::LocalSqlite, batch }).expect("commit");
    assert_eq!(committer.pending_delivery(10).expect("outbox").len(), 1);
    assert!(matches!(committer.bind_chat("chat.1", HistoryBinding::PortableProject { repository_id: "other".into() }), Err(_)));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn lifecycle_and_supervisor_reject_stale_paths() {
    let mut chat = ChatAggregate::new(id("chat.2"));
    chat.apply(ChatCommand::Start { snapshot_hash: "frozen".into() }).expect("start");
    chat.apply(ChatCommand::Pause).expect("pause");
    assert_eq!(chat.state, ChatState::Paused);
    assert!(chat.apply(ChatCommand::Complete).is_err());
    let mut supervisor = WorkerSupervisor::with_restart_budget(1);
    let snapshot = aworkit_trusted_core::FrozenRunSnapshot { chat_id: id("chat.2"), workflow_id: id("workflow.1"), workflow_version: 1, workflow_hash: "wf".into(), workspace: aworkit_trusted_core::WorkspaceBinding { root: std::env::temp_dir(), identity: aworkit_trusted_core::WorkspaceIdentity { canonical_path: "tmp".into(), created_at_nanos: None } }, authority: aworkit_trusted_core::AuthorityManifest { manifest_id: id("manifest.1"), capability_bindings: vec![], summary: "none".into() }, snapshot_hash: "frozen".into() };
    let handshake = supervisor.start(&snapshot).expect("start worker");
    supervisor.acknowledge_handshake(&handshake).expect("handshake");
    assert!(supervisor.deliver(&id("chat.2"), ProcessGeneration(0), &WorkerControl::Cancel).is_err());
}
