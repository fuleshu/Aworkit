use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use aworkit_local_store::{LocalHistoryStore, PortableRuntimeJournal};
use aworkit_portable_store::{
    ExportPolicy, PortableCapabilityRequirementV1, PortableCommitContextV1,
    PortableFrozenSnapshotV1, PortableGitFactsV1, PortablePaths, PortableProvenanceV1,
    PortableRepository, portable_snapshot_hash,
};
use aworkit_protocol::{
    CommitBatchV1, EventV1, HistoryBackendV1, HistoryPortErrorV1, PortableCommitReceiptV1,
    PortableRuntimeBeginV1, PortableRuntimeFactsV1, PortableRuntimeFinalizeV1,
    PortableRuntimeJournalPort, StableId,
};
use aworkit_trusted_core::{
    CanonicalCommitOutcomeV1, CanonicalCommitRequestV1, CanonicalCommitter, CoreCommitError,
    HistoryBinding, PortableCommitGate, PortableGateError,
};
use serde_json::json;
use tempfile::TempDir;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn hash(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

fn context() -> PortableCommitContextV1 {
    let snapshot = json!({"workflow":{"nodes":["start"]}});
    PortableCommitContextV1 {
        frozen_snapshot: Some(PortableFrozenSnapshotV1 {
            schema_version: 1,
            snapshot_hash: portable_snapshot_hash(&snapshot).unwrap(),
            workflow_hash: hash('a'),
            portable_snapshot: snapshot,
            requirements: vec![PortableCapabilityRequirementV1 {
                logical_id: "model.primary".into(),
                version: "v1".into(),
                configuration_hash: hash('b'),
            }],
        }),
        provenance: PortableProvenanceV1 {
            git: PortableGitFactsV1 {
                head_commit: Some("c".repeat(40)),
                dirty_state_digest: Some(hash('d')),
                worktree_identity: Some(hash('e')),
                unavailable_reason: None,
            },
            workflow_revision_hash: hash('f'),
            configuration_revision_hashes: vec![],
            redaction_profile_hash: ExportPolicy.policy_hash().into(),
            artifact_refs: vec![],
            artifact_metadata: vec![],
            omissions: vec![],
        },
    }
}

fn batch(
    repository_id: &StableId,
    chat: &str,
    branch: &str,
    event_id: &str,
    expected_head: u64,
) -> CommitBatchV1 {
    CommitBatchV1 {
        backend: HistoryBackendV1::PortableProject {
            repository_id: repository_id.clone(),
        },
        chat_id: id(chat),
        run_id: id("run.1"),
        branch_id: id(branch),
        expected_head,
        expected_aggregate_version: expected_head,
        events: vec![EventV1 {
            event_id: id(event_id),
            schema_version: 1,
            kind: "message".into(),
            payload: json!({"text":event_id}),
        }],
        attempts: vec![],
        checkpoint: None,
        deduplication: None,
        outbox: vec![],
        prepared_artifacts: vec![],
    }
}

fn request(
    repository_id: &StableId,
    chat: &str,
    branch: &str,
    event: &str,
    operation: &str,
    expected_generation: u64,
    expected_head: u64,
    expected_head_hash: Option<String>,
) -> aworkit_protocol::PortablePrepareV1 {
    CanonicalCommitter::portable_request_with_context(
        &batch(repository_id, chat, branch, event, expected_head),
        serde_json::to_value(context()).unwrap(),
        id(operation),
        expected_generation,
        expected_head_hash,
    )
    .unwrap()
}

#[test]
fn acknowledgement_requires_fresh_publication_verification_and_exact_linked_journal_fences() {
    let project = TempDir::new().unwrap();
    let local = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(project.path()).unwrap());
    let journal_path = local.path().join("runtime.sqlite");
    let journal = PortableRuntimeJournal::open(&journal_path).unwrap();
    let repository_id = id("repository.1");
    let gate = PortableCommitGate::new(repository.clone(), journal.clone(), id("machine.a"), 7);
    let first = request(
        &repository_id,
        "chat.1",
        "branch.1",
        "event.1",
        "operation.1",
        0,
        0,
        None,
    );
    let first_receipt = gate.commit(&first).unwrap();
    assert_eq!(first_receipt.generation, 1);
    let second = request(
        &repository_id,
        "chat.1",
        "branch.1",
        "event.2",
        "operation.2",
        1,
        1,
        Some(first_receipt.published_head_hash),
    );
    let second_receipt = gate.commit(&second).unwrap();
    assert_eq!(second_receipt.generation, 2);
    assert!(gate.recover(&id("operation.2")).unwrap().resumable);

    drop(journal);
    let reopened = PortableRuntimeJournal::open(&journal_path).unwrap();
    assert!(matches!(
        reopened.facts(&id("operation.2")).unwrap(),
        Some(PortableRuntimeFactsV1::HeadLinked { .. })
    ));
    let stale_gate = PortableCommitGate::new(repository, reopened.clone(), id("machine.b"), 7);
    let stale = stale_gate.recover(&id("operation.2")).unwrap();
    assert!(stale.quarantined);
    assert!(matches!(
        reopened.facts(&id("operation.2")).unwrap(),
        Some(PortableRuntimeFactsV1::Quarantined { .. })
    ));
}

#[derive(Clone)]
struct FailFinalizeOnce {
    inner: PortableRuntimeJournal,
    fail: Arc<AtomicBool>,
}

impl PortableRuntimeJournalPort for FailFinalizeOnce {
    fn begin(&self, request: &PortableRuntimeBeginV1) -> Result<(), HistoryPortErrorV1> {
        self.inner.begin(request)
    }

    fn finalize(&self, request: &PortableRuntimeFinalizeV1) -> Result<(), HistoryPortErrorV1> {
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(HistoryPortErrorV1 {
                code: "injected_finalize_crash".into(),
                message: "injected crash after head publication".into(),
                retryable: true,
                inspectable_read_only: true,
            });
        }
        self.inner.finalize(request)
    }

    fn facts(
        &self,
        operation_id: &StableId,
    ) -> Result<Option<PortableRuntimeFactsV1>, HistoryPortErrorV1> {
        self.inner.facts(operation_id)
    }

    fn quarantine(&self, operation_id: &StableId, reason: &str) -> Result<(), HistoryPortErrorV1> {
        self.inner.quarantine(operation_id, reason)
    }
}

#[test]
fn all_portable_crash_points_reconcile_without_semantic_replay() {
    let project = TempDir::new().unwrap();
    let local = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(project.path()).unwrap());
    let journal = PortableRuntimeJournal::open(local.path().join("runtime.sqlite")).unwrap();
    let repository_id = id("repository.crashes");
    let gate = PortableCommitGate::new(repository.clone(), journal.clone(), id("machine.a"), 1);

    repository
        .inject_fault_once(aworkit_portable_store::CommitFaultPoint::BeforeHeadPublication)
        .unwrap();
    let before = request(
        &repository_id,
        "chat.before",
        "branch.before",
        "event.before",
        "operation.before",
        0,
        0,
        None,
    );
    assert!(matches!(
        gate.commit(&before),
        Err(PortableGateError::Canonical { .. })
    ));
    assert!(matches!(
        journal.facts(&id("operation.before")).unwrap(),
        Some(PortableRuntimeFactsV1::Pending { .. })
    ));
    assert!(gate.recover(&id("operation.before")).unwrap().quarantined);
    assert!(gate.read_head(&id("branch.before")).unwrap().is_none());

    repository
        .inject_fault_once(aworkit_portable_store::CommitFaultPoint::AfterHeadPublication)
        .unwrap();
    let uncertain = request(
        &repository_id,
        "chat.uncertain",
        "branch.uncertain",
        "event.uncertain",
        "operation.uncertain",
        0,
        0,
        None,
    );
    let reconciled = gate.commit(&uncertain).unwrap();
    assert_eq!(reconciled.generation, 1);
    assert!(matches!(
        journal.facts(&id("operation.uncertain")).unwrap(),
        Some(PortableRuntimeFactsV1::HeadLinked { .. })
    ));

    let crash_journal = PortableRuntimeJournal::open(local.path().join("finalize.sqlite")).unwrap();
    let crashing = FailFinalizeOnce {
        inner: crash_journal.clone(),
        fail: Arc::new(AtomicBool::new(true)),
    };
    let crash_gate = PortableCommitGate::new(repository.clone(), crashing, id("machine.a"), 1);
    let after_head = request(
        &repository_id,
        "chat.after-head",
        "branch.after-head",
        "event.after-head",
        "operation.after-head",
        0,
        0,
        None,
    );
    assert!(matches!(
        crash_gate.commit(&after_head),
        Err(PortableGateError::Journal { .. })
    ));
    assert!(matches!(
        crash_journal.facts(&id("operation.after-head")).unwrap(),
        Some(PortableRuntimeFactsV1::Pending { .. })
    ));
    let recovery_gate = PortableCommitGate::new(
        repository.clone(),
        crash_journal.clone(),
        id("machine.a"),
        1,
    );
    assert!(
        recovery_gate
            .recover(&id("operation.after-head"))
            .unwrap()
            .resumable
    );
    assert!(matches!(
        crash_journal.facts(&id("operation.after-head")).unwrap(),
        Some(PortableRuntimeFactsV1::HeadLinked { .. })
    ));

    let missing = PortableRuntimeJournal::open(local.path().join("missing.sqlite")).unwrap();
    let missing_gate = PortableCommitGate::new(repository, missing, id("machine.a"), 1);
    assert_eq!(
        missing_gate.recover(&id("operation.missing")).err(),
        Some(PortableGateError::Quarantined)
    );
}

#[test]
fn runtime_journal_is_exact_idempotent_generation_fenced_and_restart_durable() {
    let local = TempDir::new().unwrap();
    let path = local.path().join("journal.sqlite");
    let journal = PortableRuntimeJournal::open(&path).unwrap();
    let begin = PortableRuntimeBeginV1 {
        operation_id: id("operation.direct"),
        machine_instance_id: id("machine.a"),
        binding_generation: 9,
        expected_generation: 5,
        chat_id: id("chat.direct"),
        branch_id: id("branch.direct"),
        commit_id: id("operation.direct"),
        expected_head_hash: Some(hash('1')),
        candidate_head_hash: hash('2'),
        checkpoint_hash: hash('3'),
    };
    journal.begin(&begin).unwrap();
    journal.begin(&begin).unwrap();
    let mut conflict = begin.clone();
    conflict.candidate_head_hash = hash('4');
    assert!(journal.begin(&conflict).is_err());
    let mut wrong = PortableCommitReceiptV1 {
        operation_id: begin.operation_id.clone(),
        commit_id: begin.commit_id.clone(),
        branch_id: begin.branch_id.clone(),
        previous_head_hash: begin.expected_head_hash.clone(),
        published_head_hash: begin.candidate_head_hash.clone(),
        generation: 5,
        checkpoint_hash: begin.checkpoint_hash.clone(),
    };
    assert!(
        journal
            .finalize(&PortableRuntimeFinalizeV1 {
                operation_id: begin.operation_id.clone(),
                verified_receipt: wrong.clone(),
            })
            .is_err()
    );
    wrong.generation = 6;
    journal
        .finalize(&PortableRuntimeFinalizeV1 {
            operation_id: begin.operation_id.clone(),
            verified_receipt: wrong.clone(),
        })
        .unwrap();
    journal
        .finalize(&PortableRuntimeFinalizeV1 {
            operation_id: begin.operation_id.clone(),
            verified_receipt: wrong,
        })
        .unwrap();
    drop(journal);
    let reopened = PortableRuntimeJournal::open(path).unwrap();
    assert!(matches!(
        reopened.facts(&begin.operation_id).unwrap(),
        Some(PortableRuntimeFactsV1::HeadLinked { begin: stored, .. }) if stored == begin
    ));
}

#[test]
fn canonical_committer_routes_rich_portable_context_and_freezes_history_binding() {
    let project = TempDir::new().unwrap();
    let local = TempDir::new().unwrap();
    let repository = PortableRepository::new(PortablePaths::open(project.path()).unwrap());
    let journal = PortableRuntimeJournal::open(local.path().join("runtime.sqlite")).unwrap();
    let repository_id = id("repository.router");
    let gate = PortableCommitGate::new(repository, journal, id("machine.router"), 1);
    let local_store = LocalHistoryStore::open(local.path().join("history.sqlite")).unwrap();
    let committer = CanonicalCommitter::new(local_store);
    committer
        .register_portable_repository(repository_id.clone(), gate)
        .unwrap();
    let prepare = request(
        &repository_id,
        "chat.router",
        "branch.router",
        "event.router",
        "operation.router",
        0,
        0,
        None,
    );
    assert!(prepare.record.get("context").is_some());
    let outcome = committer
        .commit_v1(&CanonicalCommitRequestV1::Portable {
            repository_id: repository_id.clone(),
            prepare,
        })
        .unwrap();
    assert!(matches!(outcome, CanonicalCommitOutcomeV1::Portable(_)));
    assert!(matches!(
        committer.bind_chat("chat.router", HistoryBinding::LocalSqlite),
        Err(CoreCommitError::HistoryBindingConflict)
    ));
    assert!(matches!(
        committer.register_portable_repository(
            repository_id,
            PortableCommitGate::new(
                PortableRepository::new(PortablePaths::open(project.path()).unwrap()),
                PortableRuntimeJournal::open(local.path().join("other.sqlite")).unwrap(),
                id("machine.other"),
                1,
            )
        ),
        Err(CoreCommitError::RepositoryAlreadyRegistered)
    ));
}
