use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use aworkit_local_store::LocalHistoryStore;
use aworkit_protocol::{
    CheckpointV1, CommitBatchV1, CommitOutcomeV1, DedupV1, EventV1, HistoryBackendV1, OutboxV1,
    ProcessGeneration, StableId, WorkerBudgetV1, WorkerCheckpointV1, WorkerControlEnvelopeV1,
    WorkerControlKindV1, WorkerExecutorKindV1, WorkerFrozenRunSnapshotV1, WorkerJoinDescriptorV1,
    WorkerNodeV1, WorkerOutputKindV1, WorkerPortV1, WorkerProposalKindV1, WorkerTransitionV1,
    decode_frame, encode_frame,
};
use aworkit_trusted_core::{
    ApprovalDecisionV1, ApprovalEngineV1, ApprovalGrantV1, ApprovalRequirement,
    AuthorityManifestV1, CanonicalCommitter, CapabilityBindingV1, CommitRequest,
    CoreServiceRequestKindV1, CoreServiceRequestV1, CoreServiceResponseKindV1,
    CoreServiceResponseV1, DesktopApi, DesktopApiError, DesktopCommand, DocumentWatchResultV1,
    HistoryBinding, LocalRecovery, ProcessWorkerSupervisorV1, ProjectCoordinator,
    ProjectDocumentKindV1, ProjectDocumentPort, ProjectDocumentV1, ProjectPortErrorV1,
    RecoveryDecisionV1, RecoveryEventV1, RecoveryFactsV1, RecoveryHistoryPort, RecoveryPortErrorV1,
    RunAggregateV1, RunCommandKindV1, RunCommandOutcomeV1, RunCommandV1, RunStateV1,
    SnapshotFreezerV1, SnapshotRequestV1, StoredProjectDocumentV1, WaitReason, WorkspaceBindingV1,
    workflow_graph_hash_v1,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable id")
}

fn hash(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}

fn port(name: &str) -> WorkerPortV1 {
    WorkerPortV1 {
        name: name.to_owned(),
        schema_ref: Some("json:any".to_owned()),
        required: true,
    }
}

fn temp_root(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("aworkit-m04-{name}-"))
        .tempdir()
        .expect("tempdir")
}

fn freeze_snapshot() -> (tempfile::TempDir, WorkerFrozenRunSnapshotV1) {
    let root = temp_root("snapshot");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let coordinator = ProjectCoordinator::open(root.path().join("state")).expect("coordinator");
    let workspace = coordinator
        .resolve_workspace_v1(&workspace)
        .expect("workspace identity");
    let nodes = vec![
        WorkerNodeV1 {
            node_id: id("start"),
            node_type: "pure.noop".to_owned(),
            node_version: 1,
            contribution_hash: hash('a'),
            inputs: Vec::new(),
            outputs: vec![port("out")],
            executor: WorkerExecutorKindV1::Pure,
            config: json!({}),
            capability_ref: None,
            result_schema_ref: None,
        },
        WorkerNodeV1 {
            node_id: id("done"),
            node_type: "terminal".to_owned(),
            node_version: 1,
            contribution_hash: hash('b'),
            inputs: vec![port("in")],
            outputs: Vec::new(),
            executor: WorkerExecutorKindV1::Terminal,
            config: json!({}),
            capability_ref: None,
            result_schema_ref: None,
        },
    ];
    let transitions = vec![WorkerTransitionV1 {
        transition_id: id("edge.start.done"),
        from_node: id("start"),
        from_port: "out".to_owned(),
        to_node: id("done"),
        to_port: "in".to_owned(),
        priority: 0,
        predicate: Some(json!({"always": true})),
        declared_loop_id: None,
    }];
    let workflow_hash = workflow_graph_hash_v1(&nodes, &transitions, &[id("start")], &[], &[], &[])
        .expect("workflow hash");
    let (snapshot, _) = SnapshotFreezerV1::freeze(
        &coordinator,
        SnapshotRequestV1 {
            snapshot_id: id("snapshot.one"),
            chat_id: id("chat.one"),
            run_id: id("run.one"),
            workflow_hash,
            nodes,
            transitions,
            entry_nodes: vec![id("start")],
            loop_descriptors: Vec::new(),
            join_descriptors: Vec::new(),
            route_rules: Vec::new(),
            workspace,
            capability_bindings: Vec::new(),
            budget: WorkerBudgetV1 {
                turns: 10,
                attempts: 10,
                tool_calls: 10,
                tokens: 1_000,
                cost_micros: 10_000,
                actions: 100,
                depth: 2,
                fanout: 4,
                parallel: 2,
                deadline_ms: 60_000,
            },
            history_mode: HistoryBackendV1::LocalSqlite,
        },
    )
    .expect("freeze");
    (root, snapshot)
}

fn request_from_snapshot(
    snapshot: &WorkerFrozenRunSnapshotV1,
    workspace: WorkspaceBindingV1,
) -> SnapshotRequestV1 {
    SnapshotRequestV1 {
        snapshot_id: snapshot.snapshot_id.clone(),
        chat_id: snapshot.chat_id.clone(),
        run_id: snapshot.run_id.clone(),
        workflow_hash: snapshot.workflow_hash.clone(),
        nodes: snapshot.nodes.clone(),
        transitions: snapshot.transitions.clone(),
        entry_nodes: snapshot.entry_nodes.clone(),
        loop_descriptors: snapshot.loop_descriptors.clone(),
        join_descriptors: snapshot.join_descriptors.clone(),
        route_rules: snapshot.route_rules.clone(),
        workspace,
        capability_bindings: Vec::new(),
        budget: snapshot.budget.clone(),
        history_mode: snapshot.history_mode.clone(),
    }
}

fn apply_run(run: &mut RunAggregateV1, command_id: &str, command: RunCommandKindV1) {
    run.handle(RunCommandV1 {
        command_id: id(command_id),
        expected_version: run.version,
        command,
    })
    .expect("legal run transition");
}

fn atomic_active_run(chat_id: &str, run_id: &str) -> RunAggregateV1 {
    let mut run = RunAggregateV1::new(id(chat_id), id(run_id));
    apply_run(
        &mut run,
        "atomic.start",
        RunCommandKindV1::StartResolved {
            input_id: id("atomic.input"),
            snapshot_id: id("atomic.snapshot"),
            snapshot_hash: hash('a'),
        },
    );
    apply_run(
        &mut run,
        "atomic.ready",
        RunCommandKindV1::WorkerReady {
            generation: ProcessGeneration(1),
        },
    );
    run
}

fn recovery_lifecycle(
    snapshot: &WorkerFrozenRunSnapshotV1,
    checkpoint_hash: &str,
    uncertain_invocations: Vec<StableId>,
) -> Vec<aworkit_trusted_core::CommittedRunEventV1> {
    let mut run = RunAggregateV1::new(snapshot.chat_id.clone(), snapshot.run_id.clone());
    apply_run(
        &mut run,
        "recovery.first",
        RunCommandKindV1::AcceptFirstInput {
            input_id: id("recovery.input"),
        },
    );
    apply_run(
        &mut run,
        "recovery.freeze",
        RunCommandKindV1::FreezeSnapshot {
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_hash: snapshot.snapshot_hash.clone(),
        },
    );
    apply_run(
        &mut run,
        "recovery.ready",
        RunCommandKindV1::WorkerReady {
            generation: ProcessGeneration(1),
        },
    );
    apply_run(
        &mut run,
        "recovery.crash",
        RunCommandKindV1::WorkerCrashed {
            generation: ProcessGeneration(1),
            checkpoint_hash: Some(checkpoint_hash.to_owned()),
            uncertain_invocations,
        },
    );
    run.events().to_vec()
}

#[test]
fn desktop_transaction_is_atomic_idempotent_and_cursor_bounded() {
    let api = DesktopApi::default();
    let command = DesktopCommand {
        command_id: id("command.one"),
        expected_version: 0,
        name: "chat.complete".to_owned(),
        payload: json!({"target": "chat.one"}),
    };
    let rejected = api.transact_committed(
        &command,
        id("event.one"),
        "chat.completed",
        json!({}),
        || Err("illegal lifecycle transition".to_owned()),
    );
    assert!(matches!(rejected, Err(DesktopApiError::DomainRejected(_))));
    assert_eq!(api.snapshot_after(0).expect("snapshot").version, 0);

    let calls = Cell::new(0);
    let committed = api
        .transact_committed(
            &command,
            id("event.one"),
            "chat.completed",
            json!({}),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .expect("commit");
    assert!(!committed.duplicate);
    assert_eq!(committed.receipt.current_version, 1);
    let duplicate = api
        .transact_committed(
            &command,
            id("event.one"),
            "chat.completed",
            json!({}),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .expect("duplicate");
    assert!(duplicate.duplicate);
    assert_eq!(
        calls.get(),
        1,
        "duplicate command never reruns domain logic"
    );

    let mut changed = command.clone();
    changed.payload = json!({"target": "other"});
    assert!(matches!(
        api.transact_committed(
            &changed,
            id("event.one"),
            "chat.completed",
            json!({}),
            || Ok(())
        ),
        Err(DesktopApiError::CommandIdentityConflict)
    ));
    assert!(matches!(
        api.snapshot_page_after(2, 1),
        Err(DesktopApiError::CursorAhead { .. })
    ));
    assert!(matches!(
        api.snapshot_page_after(0, 0),
        Err(DesktopApiError::InvalidPageLimit)
    ));
    assert_eq!(api.snapshot_page_after(0, 1).expect("page").events.len(), 1);
}

#[test]
fn run_lifecycle_folds_exactly_and_recovery_requires_a_new_generation() {
    let mut run = RunAggregateV1::new(id("chat.lifecycle"), id("run.lifecycle"));
    let first = RunCommandV1 {
        command_id: id("command.first"),
        expected_version: 0,
        command: RunCommandKindV1::AcceptFirstInput {
            input_id: id("input.first"),
        },
    };
    assert!(matches!(
        run.handle(first.clone()).expect("first"),
        RunCommandOutcomeV1::Applied(_)
    ));
    assert!(matches!(
        run.handle(first).expect("duplicate"),
        RunCommandOutcomeV1::Duplicate(_)
    ));
    run.handle(RunCommandV1 {
        command_id: id("command.freeze"),
        expected_version: 1,
        command: RunCommandKindV1::FreezeSnapshot {
            snapshot_id: id("snapshot.lifecycle"),
            snapshot_hash: hash('c'),
        },
    })
    .expect("freeze");
    run.handle(RunCommandV1 {
        command_id: id("command.ready"),
        expected_version: 2,
        command: RunCommandKindV1::WorkerReady {
            generation: ProcessGeneration(1),
        },
    })
    .expect("ready");
    run.handle(RunCommandV1 {
        command_id: id("command.wait"),
        expected_version: 3,
        command: RunCommandKindV1::Wait {
            reason: WaitReason::Approval,
        },
    })
    .expect("wait");
    let before = run.clone();
    assert!(
        run.handle(RunCommandV1 {
            command_id: id("command.illegal"),
            expected_version: 4,
            command: RunCommandKindV1::Complete,
        })
        .is_err()
    );
    assert_eq!(run, before, "illegal command is side-effect free");
    run.handle(RunCommandV1 {
        command_id: id("command.approve"),
        expected_version: 4,
        command: RunCommandKindV1::ResolveApproval {
            approval_id: id("approval.one"),
            approved: true,
        },
    })
    .expect("approval");
    run.handle(RunCommandV1 {
        command_id: id("command.crash"),
        expected_version: 5,
        command: RunCommandKindV1::WorkerCrashed {
            generation: ProcessGeneration(1),
            checkpoint_hash: Some(hash('d')),
            uncertain_invocations: Vec::new(),
        },
    })
    .expect("crash");
    assert_eq!(run.state, RunStateV1::Rehydrating);
    assert!(
        run.handle(RunCommandV1 {
            command_id: id("command.stale-rehydrate"),
            expected_version: 6,
            command: RunCommandKindV1::RehydrationReady {
                generation: ProcessGeneration(1),
            },
        })
        .is_err()
    );
    run.handle(RunCommandV1 {
        command_id: id("command.rehydrate"),
        expected_version: 6,
        command: RunCommandKindV1::RehydrationReady {
            generation: ProcessGeneration(2),
        },
    })
    .expect("rehydrate");
    let folded =
        RunAggregateV1::fold(run.chat_id.clone(), run.run_id.clone(), run.events()).expect("fold");
    assert_eq!(folded.state, RunStateV1::Active);
    assert_eq!(folded.snapshot_hash, run.snapshot_hash);
    assert_eq!(folded.generation, Some(ProcessGeneration(2)));
}

#[test]
fn first_input_rejection_keeps_the_draft_editable_and_success_freezes_atomically() {
    let mut run = RunAggregateV1::new(id("chat.atomic"), id("run.atomic"));
    apply_run(
        &mut run,
        "atomic.reject",
        RunCommandKindV1::RejectStart {
            code: "workspace_drift".to_owned(),
        },
    );
    assert_eq!(run.state, RunStateV1::BlockedDraft);
    assert!(run.snapshot_hash.is_none());
    assert!(run.queued_inputs.is_empty());

    apply_run(
        &mut run,
        "atomic.resolve",
        RunCommandKindV1::StartResolved {
            input_id: id("atomic.input"),
            snapshot_id: id("atomic.snapshot"),
            snapshot_hash: hash('4'),
        },
    );
    assert_eq!(run.state, RunStateV1::Starting);
    assert_eq!(run.snapshot_hash.as_deref(), Some(hash('4').as_str()));
    assert_eq!(run.queued_inputs, vec![id("atomic.input")]);
    let before = run.clone();
    assert!(
        run.handle(RunCommandV1 {
            command_id: id("atomic.refreeze"),
            expected_version: run.version,
            command: RunCommandKindV1::StartResolved {
                input_id: id("atomic.other"),
                snapshot_id: id("atomic.other.snapshot"),
                snapshot_hash: hash('5'),
            },
        })
        .is_err()
    );
    assert_eq!(run, before, "frozen identity cannot be replaced");
}

#[test]
fn replacement_generation_restores_wait_pause_and_cancellation_positions() {
    let cases = [
        ("wait", RunStateV1::WaitingInput),
        ("approval", RunStateV1::WaitingApproval),
        ("pause", RunStateV1::Paused),
        ("cancel", RunStateV1::Cancelling),
    ];
    for (case, expected) in cases {
        let mut run = atomic_active_run(
            &format!("chat.rehydrate.{case}"),
            &format!("run.rehydrate.{case}"),
        );
        match expected {
            RunStateV1::WaitingInput => apply_run(
                &mut run,
                &format!("{case}.wait"),
                RunCommandKindV1::Wait {
                    reason: WaitReason::Input,
                },
            ),
            RunStateV1::WaitingApproval => apply_run(
                &mut run,
                &format!("{case}.wait"),
                RunCommandKindV1::Wait {
                    reason: WaitReason::Approval,
                },
            ),
            RunStateV1::Paused => {
                apply_run(
                    &mut run,
                    &format!("{case}.request"),
                    RunCommandKindV1::RequestPause,
                );
                apply_run(
                    &mut run,
                    &format!("{case}.quiesced"),
                    RunCommandKindV1::WorkerQuiesced {
                        checkpoint_hash: hash('6'),
                    },
                );
            }
            RunStateV1::Cancelling => apply_run(
                &mut run,
                &format!("{case}.request"),
                RunCommandKindV1::RequestCancel,
            ),
            _ => unreachable!(),
        }
        apply_run(
            &mut run,
            &format!("{case}.crash"),
            RunCommandKindV1::WorkerCrashed {
                generation: ProcessGeneration(1),
                checkpoint_hash: Some(hash('7')),
                uncertain_invocations: Vec::new(),
            },
        );
        assert_eq!(run.state, RunStateV1::Rehydrating);
        apply_run(
            &mut run,
            &format!("{case}.ready"),
            RunCommandKindV1::RehydrationReady {
                generation: ProcessGeneration(2),
            },
        );
        assert_eq!(run.state, expected, "restore target for {case}");
        let folded = RunAggregateV1::fold(run.chat_id.clone(), run.run_id.clone(), run.events())
            .expect("fold restored state");
        assert_eq!(folded.state, expected);
    }

    let mut denied = atomic_active_run("chat.approval.denied", "run.approval.denied");
    apply_run(
        &mut denied,
        "denied.wait",
        RunCommandKindV1::Wait {
            reason: WaitReason::Approval,
        },
    );
    apply_run(
        &mut denied,
        "denied.resolve",
        RunCommandKindV1::ResolveApproval {
            approval_id: id("approval.denied"),
            approved: false,
        },
    );
    assert_eq!(
        denied.state,
        RunStateV1::Active,
        "denial returns to frozen routing instead of dead-ending the Chat"
    );
}

#[test]
fn run_lifecycle_covers_queue_attempt_wait_pause_cancel_retry_fork_and_continue() {
    let mut run = RunAggregateV1::new(id("chat.transitions"), id("run.transitions"));
    apply_run(
        &mut run,
        "transition.first",
        RunCommandKindV1::AcceptFirstInput {
            input_id: id("input.initial"),
        },
    );
    apply_run(
        &mut run,
        "transition.freeze",
        RunCommandKindV1::FreezeSnapshot {
            snapshot_id: id("snapshot.transitions"),
            snapshot_hash: hash('1'),
        },
    );
    apply_run(
        &mut run,
        "transition.ready",
        RunCommandKindV1::WorkerReady {
            generation: ProcessGeneration(1),
        },
    );
    apply_run(
        &mut run,
        "transition.deliver.initial",
        RunCommandKindV1::DeliverInput {
            input_id: id("input.initial"),
        },
    );
    apply_run(
        &mut run,
        "transition.queue",
        RunCommandKindV1::QueueInput {
            input_id: id("input.queued"),
        },
    );
    apply_run(
        &mut run,
        "transition.wait.input",
        RunCommandKindV1::Wait {
            reason: WaitReason::Input,
        },
    );
    apply_run(
        &mut run,
        "transition.deliver.queued",
        RunCommandKindV1::DeliverInput {
            input_id: id("input.queued"),
        },
    );
    apply_run(
        &mut run,
        "transition.attempt.begin",
        RunCommandKindV1::BeginAttempt {
            attempt_id: id("attempt.transitions"),
            operation_id: id("operation.transitions"),
        },
    );
    apply_run(
        &mut run,
        "transition.attempt.finish",
        RunCommandKindV1::FinishAttempt {
            attempt_id: id("attempt.transitions"),
            outcome: "success".to_owned(),
        },
    );
    apply_run(
        &mut run,
        "transition.pause.request",
        RunCommandKindV1::RequestPause,
    );
    apply_run(
        &mut run,
        "transition.pause.quiesced",
        RunCommandKindV1::WorkerQuiesced {
            checkpoint_hash: hash('2'),
        },
    );
    apply_run(&mut run, "transition.resume", RunCommandKindV1::Resume);
    apply_run(
        &mut run,
        "transition.cancel.request",
        RunCommandKindV1::RequestCancel,
    );
    apply_run(
        &mut run,
        "transition.cancel.stopped",
        RunCommandKindV1::WorkerStopped,
    );
    apply_run(
        &mut run,
        "transition.fork",
        RunCommandKindV1::Fork {
            child_chat_id: id("chat.transitions.child"),
        },
    );
    let folded =
        RunAggregateV1::fold(run.chat_id.clone(), run.run_id.clone(), run.events()).expect("fold");
    assert_eq!(folded.state, RunStateV1::Cancelled);
    assert_eq!(folded.queued_inputs, Vec::<StableId>::new());
    assert_eq!(
        folded
            .active_attempts
            .get("attempt.transitions")
            .and_then(|attempt| attempt.outcome.as_deref()),
        Some("success")
    );
    assert_eq!(folded.child_chat_ids, vec![id("chat.transitions.child")]);

    let mut retry = RunAggregateV1::new(id("chat.retry"), id("run.retry"));
    apply_run(
        &mut retry,
        "retry.first",
        RunCommandKindV1::AcceptFirstInput {
            input_id: id("retry.input"),
        },
    );
    apply_run(
        &mut retry,
        "retry.freeze",
        RunCommandKindV1::FreezeSnapshot {
            snapshot_id: id("retry.snapshot"),
            snapshot_hash: hash('3'),
        },
    );
    apply_run(
        &mut retry,
        "retry.ready",
        RunCommandKindV1::WorkerReady {
            generation: ProcessGeneration(1),
        },
    );
    apply_run(
        &mut retry,
        "retry.fail",
        RunCommandKindV1::Fail {
            code: "transient".to_owned(),
            retryable: true,
        },
    );
    apply_run(&mut retry, "retry.request", RunCommandKindV1::Retry);
    apply_run(
        &mut retry,
        "retry.ready.second",
        RunCommandKindV1::RehydrationReady {
            generation: ProcessGeneration(2),
        },
    );
    apply_run(&mut retry, "retry.complete", RunCommandKindV1::Complete);
    assert_eq!(retry.state, RunStateV1::Completed);

    let mut continued = RunAggregateV1::new(id("chat.continued"), id("run.continued"));
    apply_run(
        &mut continued,
        "continue.parent",
        RunCommandKindV1::ContinueFrom {
            parent_chat_id: run.chat_id.clone(),
        },
    );
    assert_eq!(continued.parent_chat_id, Some(run.chat_id));
    assert_eq!(continued.state, RunStateV1::Draft);
}

#[test]
fn approval_grants_are_exact_expiring_and_single_use() {
    let binding = CapabilityBindingV1 {
        capability_id: id("capability.shell"),
        adapter_id: id("adapter.shell"),
        adapter_version: "1.0.0".to_owned(),
        descriptor_hash: hash('e'),
        enabled: true,
        compatible: true,
        approval: ApprovalRequirement::PerInvocation,
        allowed_node_types: vec!["tool.shell".to_owned()],
    };
    let manifest = AuthorityManifestV1 {
        manifest_id: id("manifest.approval"),
        manifest_hash: hash('f'),
        capability_bindings: vec![binding.clone()],
        summary: "approval".to_owned(),
    };
    let grant = ApprovalGrantV1 {
        approval_id: id("approval.grant"),
        invocation_id: id("invocation.one"),
        authority_manifest_ref: manifest.manifest_id.clone(),
        expires_at_tick: 10,
        constraints: json!({}),
    };
    let mut engine = ApprovalEngineV1::default();
    assert!(matches!(
        engine.authorize(&binding, &manifest, &id("invocation.one"), 1, Some(&grant)),
        ApprovalDecisionV1::Approved { .. }
    ));
    assert_eq!(
        engine.authorize(&binding, &manifest, &id("invocation.one"), 1, Some(&grant)),
        ApprovalDecisionV1::AlreadyConsumed
    );
    let mut expired = grant;
    expired.approval_id = id("approval.expired");
    assert_eq!(
        engine.authorize(
            &binding,
            &manifest,
            &id("invocation.one"),
            10,
            Some(&expired)
        ),
        ApprovalDecisionV1::Expired
    );
}

#[test]
fn snapshot_freezing_discloses_authority_and_rejects_unresolved_or_drifting_inputs() {
    let (root, snapshot) = freeze_snapshot();
    let workspace_path = root.path().join("workspace");
    let coordinator =
        ProjectCoordinator::open(root.path().join("freeze-state")).expect("freeze coordinator");
    let workspace = coordinator
        .resolve_workspace_v1(&workspace_path)
        .expect("workspace");
    let (_, manifest) = SnapshotFreezerV1::freeze(
        &coordinator,
        request_from_snapshot(&snapshot, workspace.clone()),
    )
    .expect("pure workflow freezes without capabilities");
    assert_eq!(
        manifest.summary,
        "0 exact capability binding(s); 0 require per-invocation approval"
    );

    let mut unresolved = request_from_snapshot(&snapshot, workspace.clone());
    unresolved.nodes[0].executor = WorkerExecutorKindV1::Brokered;
    unresolved.nodes[0].capability_ref = Some(id("capability.missing"));
    unresolved.workflow_hash = workflow_graph_hash_v1(
        &unresolved.nodes,
        &unresolved.transitions,
        &unresolved.entry_nodes,
        &unresolved.loop_descriptors,
        &unresolved.join_descriptors,
        &unresolved.route_rules,
    )
    .expect("workflow hash");
    assert!(SnapshotFreezerV1::freeze(&coordinator, unresolved).is_err());

    fs::rename(&workspace_path, root.path().join("workspace-old")).expect("retain old inode");
    fs::create_dir(&workspace_path).expect("replacement workspace");
    assert!(
        SnapshotFreezerV1::freeze(&coordinator, request_from_snapshot(&snapshot, workspace),)
            .is_err(),
        "the same path with a different filesystem identity fails closed"
    );
}

#[test]
fn ordered_join_branch_order_is_part_of_the_workflow_identity() {
    let join = WorkerJoinDescriptorV1 {
        join_id: id("join.ordered"),
        node_id: id("join.node"),
        expected_branches: vec![id("branch.a"), id("branch.b")],
        merge_policy: "ordered_array".to_owned(),
    };
    let forward =
        workflow_graph_hash_v1(&[], &[], &[], &[], &[join.clone()], &[]).expect("forward hash");
    let mut reversed = join;
    reversed.expected_branches.reverse();
    let backward =
        workflow_graph_hash_v1(&[], &[], &[], &[], &[reversed], &[]).expect("reverse hash");
    assert_ne!(forward, backward);
}

#[derive(Clone, Default)]
struct MemoryDocuments {
    documents: Arc<Mutex<BTreeMap<String, StoredProjectDocumentV1>>>,
}

impl ProjectDocumentPort for MemoryDocuments {
    fn load(
        &self,
        _kind: ProjectDocumentKindV1,
        document_id: &StableId,
    ) -> Result<Option<StoredProjectDocumentV1>, ProjectPortErrorV1> {
        Ok(self
            .documents
            .lock()
            .expect("documents")
            .get(document_id.as_str())
            .cloned())
    }

    fn save(
        &self,
        kind: ProjectDocumentKindV1,
        document_id: &StableId,
        expected_version: Option<u64>,
        document: &ProjectDocumentV1,
    ) -> Result<StoredProjectDocumentV1, ProjectPortErrorV1> {
        let mut documents = self.documents.lock().expect("documents");
        let current = documents
            .get(document_id.as_str())
            .map(|stored| stored.version);
        if current != expected_version {
            return Err(ProjectPortErrorV1 {
                code: "version_conflict".to_owned(),
                message: "version conflict".to_owned(),
                retryable: false,
            });
        }
        let version = current.unwrap_or(0) + 1;
        let content_hash = format!(
            "{:x}",
            Sha256::digest(serde_jcs::to_vec(document).expect("canonical"))
        );
        let stored = StoredProjectDocumentV1 {
            kind,
            document_id: document_id.clone(),
            version,
            content_hash,
            document: document.clone(),
        };
        documents.insert(document_id.as_str().to_owned(), stored.clone());
        Ok(stored)
    }

    fn list(
        &self,
        kind: ProjectDocumentKindV1,
        after_id: Option<&StableId>,
        limit: u32,
    ) -> Result<Vec<StoredProjectDocumentV1>, ProjectPortErrorV1> {
        Ok(self
            .documents
            .lock()
            .expect("documents")
            .values()
            .filter(|stored| {
                stored.kind == kind
                    && after_id.is_none_or(|after| stored.document_id.as_str() > after.as_str())
            })
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

#[test]
fn project_documents_round_trip_and_reject_secret_material() {
    let root = temp_root("documents");
    let coordinator = ProjectCoordinator::with_document_port(
        root.path().join("state"),
        MemoryDocuments::default(),
    )
    .expect("coordinator");
    let document_id = id("workflow.one");
    let document = ProjectDocumentV1 {
        schema_version: 1,
        body: json!({"nodes": [], "credentialRef": "credential.one"}),
    };
    let stored = coordinator
        .save_document_v1(
            ProjectDocumentKindV1::Workflow,
            &document_id,
            None,
            &document,
        )
        .expect("save");
    assert!(matches!(
        coordinator
            .watch_document_v1(
                ProjectDocumentKindV1::Workflow,
                &document_id,
                Some(stored.version)
            )
            .expect("watch"),
        DocumentWatchResultV1::Unchanged { .. }
    ));
    let exported = coordinator
        .export_document_v1(ProjectDocumentKindV1::Workflow, &document_id)
        .expect("export");
    assert_eq!(
        serde_json::from_slice::<ProjectDocumentV1>(&exported).expect("decode"),
        document
    );
    assert!(
        coordinator
            .save_document_v1(
                ProjectDocumentKindV1::ProjectConfig,
                &id("config.secret"),
                None,
                &ProjectDocumentV1 {
                    schema_version: 1,
                    body: json!({"apiKey": "cleartext"}),
                },
            )
            .is_err()
    );
}

#[derive(Clone)]
struct FixedRecovery(RecoveryFactsV1);

impl RecoveryHistoryPort for FixedRecovery {
    fn load_recovery_facts(
        &self,
        _chat_id: &StableId,
        _branch_id: &StableId,
    ) -> Result<RecoveryFactsV1, RecoveryPortErrorV1> {
        Ok(self.0.clone())
    }
}

#[test]
fn recovery_builds_only_a_fenced_logical_rehydration_envelope() {
    let (_root, snapshot) = freeze_snapshot();
    let mut checkpoint = WorkerCheckpointV1 {
        checkpoint_id: id("checkpoint.one"),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        plan_hash: hash('7'),
        checkpoint_hash: String::new(),
        prior_generation: ProcessGeneration(1),
        committed_cursor: 3,
        proposal_sequence: 2,
        token_frontier: json!([]),
        context_heads: json!([1]),
        context_revision_dag: json!({}),
        branch_frames: json!([]),
        loop_frames: json!([]),
        budget_state: json!({}),
        attempt_state: json!({}),
        no_resend_state: json!({}),
        suspension_state: json!([]),
        child_frames: json!([]),
    };
    let mut canonical = checkpoint.clone();
    canonical.checkpoint_hash.clear();
    checkpoint.checkpoint_hash = format!(
        "{:x}",
        Sha256::digest(serde_jcs::to_vec(&canonical).expect("checkpoint"))
    );
    let lifecycle_events = recovery_lifecycle(&snapshot, &checkpoint.checkpoint_hash, Vec::new());
    let facts = RecoveryFactsV1 {
        snapshot,
        checkpoint: Some(checkpoint),
        lifecycle_events,
        events: vec![RecoveryEventV1 {
            sequence: 1,
            kind: "started".to_owned(),
            payload: json!({}),
        }],
        committed_deltas: vec![json!({"cursor": 4, "kind": "committed"})],
        reconciled_outcomes: Vec::new(),
        uncertain_invocation_ids: Vec::new(),
        pending_delivery_count: 2,
        prior_generation: ProcessGeneration(1),
    };
    let recovery = LocalRecovery::new(FixedRecovery(facts.clone()));
    let RecoveryDecisionV1::SpawnReplacement {
        aggregate,
        envelope,
        pending_delivery_count,
    } = recovery
        .recover_v1(&id("chat.one"), &id("main"))
        .expect("recover")
    else {
        panic!("expected replacement");
    };
    assert_eq!(envelope.replacement_generation, ProcessGeneration(2));
    assert_eq!(aggregate.state, RunStateV1::Rehydrating);
    assert_eq!(
        aggregate.snapshot_hash,
        Some(envelope.snapshot.snapshot_hash.clone())
    );
    assert_eq!(pending_delivery_count, 2);
    assert!(envelope.reconciled_outcomes.is_empty());

    let mut blocked = facts;
    let uncertain = id("invocation.uncertain");
    blocked.uncertain_invocation_ids = vec![uncertain.clone()];
    blocked.lifecycle_events = recovery_lifecycle(
        &blocked.snapshot,
        &blocked
            .checkpoint
            .as_ref()
            .expect("checkpoint")
            .checkpoint_hash,
        vec![uncertain],
    );
    assert!(matches!(
        LocalRecovery::new(FixedRecovery(blocked))
            .recover_v1(&id("chat.one"), &id("main"))
            .expect("blocked"),
        RecoveryDecisionV1::Blocked {
            aggregate: ref blocked,
            ..
        } if blocked.state == RunStateV1::Blocked
    ));
}

#[test]
fn canonical_local_commit_routes_through_the_process_neutral_port() {
    let root = temp_root("history");
    let store = LocalHistoryStore::open(root.path().join("history.sqlite")).expect("store");
    let committer = CanonicalCommitter::new(store);
    let batch = CommitBatchV1 {
        backend: HistoryBackendV1::LocalSqlite,
        chat_id: id("chat.commit"),
        run_id: id("run.commit"),
        branch_id: id("main"),
        expected_head: 0,
        expected_aggregate_version: 0,
        events: vec![EventV1 {
            event_id: id("event.commit"),
            schema_version: 1,
            kind: "started".to_owned(),
            payload: json!({"snapshotHash": hash('8')}),
        }],
        attempts: Vec::new(),
        checkpoint: Some(CheckpointV1 {
            reducer_version: "run-v1".to_owned(),
            state_hash: hash('9'),
            frozen_snapshot_ref: Some(id("snapshot.commit")),
        }),
        deduplication: Some(DedupV1 {
            key_type: "command".to_owned(),
            key: id("command.commit"),
        }),
        outbox: vec![OutboxV1 {
            outbox_id: id("outbox.commit"),
            destination: "worker".to_owned(),
            schema_version: 1,
            payload: json!({"control": "resume"}),
        }],
        prepared_artifacts: Vec::new(),
    };
    let committed = committer
        .commit(&CommitRequest {
            binding: HistoryBinding::LocalSqlite,
            batch: batch.clone(),
        })
        .expect("commit");
    let CommitOutcomeV1::Committed(receipt) = committed else {
        panic!("first append must commit");
    };
    assert_eq!(receipt.head_sequence, 1);
    assert_eq!(receipt.aggregate_version, 1);
    assert_eq!(receipt.checkpoint_hash, Some(hash('9')));
    let CommitOutcomeV1::Existing(existing) = committer
        .commit(&CommitRequest {
            binding: HistoryBinding::LocalSqlite,
            batch: batch.clone(),
        })
        .expect("idempotent retry")
    else {
        panic!("exact retry must return the existing receipt");
    };
    assert_eq!(existing, receipt);

    let mut reused_key = batch.clone();
    reused_key.events[0].payload = json!({"snapshotHash": hash('0')});
    assert!(
        committer
            .commit(&CommitRequest {
                binding: HistoryBinding::LocalSqlite,
                batch: reused_key,
            })
            .is_err()
    );
    let mut stale_head = batch;
    stale_head.deduplication = None;
    stale_head.events[0].event_id = id("event.stale-head");
    stale_head.outbox.clear();
    assert!(
        committer
            .commit(&CommitRequest {
                binding: HistoryBinding::LocalSqlite,
                batch: stale_head,
            })
            .is_err()
    );

    let pending = committer.pending_delivery(10).expect("outbox");
    assert_eq!(pending.len(), 1);
    assert!(
        committer
            .mark_delivered_v1(&pending[0].outbox_id, pending[0].delivery_cursor + 1)
            .is_err()
    );
    committer
        .mark_delivered_v1(&pending[0].outbox_id, pending[0].delivery_cursor)
        .expect("cursor-fenced delivery ack");
    assert!(committer.pending_delivery(10).expect("drained").is_empty());
    assert!(
        committer
            .bind_chat(
                "chat.commit",
                HistoryBinding::PortableProject {
                    repository_id: "other".to_owned(),
                }
            )
            .is_err()
    );
}

#[test]
fn supervisor_launches_validates_and_cleans_up_a_real_worker() {
    let Some(executable) = std::env::var_os("AWORKIT_WORKER_BIN") else {
        eprintln!("AWORKIT_WORKER_BIN absent; executable integration is run by qa/milestone-04.sh");
        return;
    };
    let (_root, snapshot) = freeze_snapshot();
    let mut supervisor = ProcessWorkerSupervisorV1::new(executable, 1).expect("supervisor");
    let handshake = supervisor
        .spawn_start(snapshot.clone(), 0, Duration::from_secs(5))
        .expect("spawn");
    assert_eq!(handshake.snapshot_hash, snapshot.snapshot_hash);
    let ready = supervisor
        .receive(&snapshot.chat_id, Duration::from_secs(5))
        .expect("ready");
    assert!(matches!(
        ready.output,
        WorkerOutputKindV1::Proposal(ref proposal)
            if matches!(&proposal.proposal, WorkerProposalKindV1::Ready { plan_fingerprint }
                if plan_fingerprint == &handshake.plan_fingerprint)
    ));
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("heartbeat")
            .output,
        WorkerOutputKindV1::Heartbeat(_)
    ));
    assert!(
        supervisor
            .send_control(&WorkerControlEnvelopeV1 {
                message_id: id("control.stale"),
                chat_id: snapshot.chat_id.clone(),
                run_id: snapshot.run_id.clone(),
                generation: ProcessGeneration(0),
                snapshot_hash: snapshot.snapshot_hash.clone(),
                committed_cursor: 0,
                control: WorkerControlKindV1::Cancel {
                    control_id: id("cancel.stale"),
                    scope: "run".to_owned(),
                },
            })
            .is_err()
    );
    supervisor
        .mark_crashed(&snapshot.chat_id, ProcessGeneration(1))
        .expect("record crash and terminate generation one");

    let mut checkpoint = WorkerCheckpointV1 {
        checkpoint_id: id("checkpoint.supervisor"),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        plan_hash: handshake.plan_fingerprint,
        checkpoint_hash: String::new(),
        prior_generation: ProcessGeneration(1),
        committed_cursor: 0,
        proposal_sequence: 1,
        token_frontier: json!([]),
        context_heads: json!([1]),
        context_revision_dag: json!({}),
        branch_frames: json!([]),
        loop_frames: json!([]),
        budget_state: json!({}),
        attempt_state: json!({}),
        no_resend_state: json!({}),
        suspension_state: json!([]),
        child_frames: json!([]),
    };
    let mut canonical = checkpoint.clone();
    canonical.checkpoint_hash.clear();
    checkpoint.checkpoint_hash = format!(
        "{:x}",
        Sha256::digest(serde_jcs::to_vec(&canonical).expect("checkpoint"))
    );
    let lifecycle_events = recovery_lifecycle(&snapshot, &checkpoint.checkpoint_hash, Vec::new());
    let RecoveryDecisionV1::SpawnReplacement { envelope, .. } =
        LocalRecovery::new(FixedRecovery(RecoveryFactsV1 {
            snapshot: snapshot.clone(),
            checkpoint: Some(checkpoint.clone()),
            lifecycle_events,
            events: vec![RecoveryEventV1 {
                sequence: 1,
                kind: "worker_crashed".to_owned(),
                payload: json!({}),
            }],
            committed_deltas: Vec::new(),
            reconciled_outcomes: Vec::new(),
            uncertain_invocation_ids: Vec::new(),
            pending_delivery_count: 0,
            prior_generation: ProcessGeneration(1),
        }))
        .recover_v1(&snapshot.chat_id, &id("main"))
        .expect("rebuild aggregate and recovery envelope")
    else {
        panic!("expected replacement");
    };
    let restore = WorkerControlEnvelopeV1 {
        message_id: id("control.restore"),
        chat_id: snapshot.chat_id.clone(),
        run_id: snapshot.run_id.clone(),
        generation: ProcessGeneration(2),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        committed_cursor: checkpoint.committed_cursor,
        control: WorkerControlKindV1::Restore(envelope),
    };
    let mut stale_restore = restore.clone();
    stale_restore.generation = ProcessGeneration(1);
    assert!(
        supervisor
            .spawn_restore(stale_restore, Duration::from_secs(5))
            .is_err(),
        "a stale restore is rejected without consuming generation two"
    );
    let restored = supervisor
        .spawn_restore(restore, Duration::from_secs(5))
        .expect("replacement worker");
    assert_eq!(restored.generation, ProcessGeneration(2));
    assert_eq!(restored.snapshot_hash, snapshot.snapshot_hash);
    let rehydrated = supervisor
        .receive(&snapshot.chat_id, Duration::from_secs(5))
        .expect("rehydration ready");
    assert!(matches!(
        rehydrated.output,
        WorkerOutputKindV1::Proposal(ref proposal)
            if matches!(&proposal.proposal, WorkerProposalKindV1::RehydrationReady {
                checkpoint_hash
            } if checkpoint_hash == &checkpoint.checkpoint_hash)
    ));
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("replacement heartbeat")
            .output,
        WorkerOutputKindV1::Heartbeat(_)
    ));
    assert_eq!(
        supervisor
            .check_health_within(&snapshot.chat_id, Duration::from_secs(5))
            .expect("fresh heartbeat"),
        ProcessGeneration(2)
    );
    supervisor
        .send_control(&WorkerControlEnvelopeV1 {
            message_id: id("control.pause.committed"),
            chat_id: snapshot.chat_id.clone(),
            run_id: snapshot.run_id.clone(),
            generation: ProcessGeneration(2),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            committed_cursor: 0,
            control: WorkerControlKindV1::Pause {
                control_id: id("pause.committed"),
                scope: "run".to_owned(),
            },
        })
        .expect("committed pause control");
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("paused")
            .output,
        WorkerOutputKindV1::Proposal(ref proposal)
            if matches!(&proposal.proposal, WorkerProposalKindV1::Suspension { .. })
    ));
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("pause heartbeat")
            .output,
        WorkerOutputKindV1::Heartbeat(_)
    ));
    supervisor
        .send_control(&WorkerControlEnvelopeV1 {
            message_id: id("control.cancel.committed"),
            chat_id: snapshot.chat_id.clone(),
            run_id: snapshot.run_id.clone(),
            generation: ProcessGeneration(2),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            committed_cursor: 0,
            control: WorkerControlKindV1::Cancel {
                control_id: id("cancel.committed"),
                scope: "run".to_owned(),
            },
        })
        .expect("committed cancel control");
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("cancelled")
            .output,
        WorkerOutputKindV1::Proposal(ref proposal)
            if matches!(&proposal.proposal, WorkerProposalKindV1::Terminal {
                outcome,
                ..
            } if outcome == "cancelled")
    ));
    assert!(matches!(
        supervisor
            .receive(&snapshot.chat_id, Duration::from_secs(5))
            .expect("cancel heartbeat")
            .output,
        WorkerOutputKindV1::Heartbeat(_)
    ));
    supervisor
        .shutdown(&snapshot.chat_id, 0, Duration::from_secs(5))
        .expect("bounded shutdown");
}

#[test]
fn trusted_core_binary_is_a_real_framed_service() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aworkit-trusted-core"))
        .arg("--serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("core process");
    let mut input = child.stdin.take().expect("stdin");
    let mut output = child.stdout.take().expect("stdout");
    let request = CoreServiceRequestV1 {
        message_id: id("service.ping"),
        generation: ProcessGeneration(1),
        request: CoreServiceRequestKindV1::Ping,
    };
    input
        .write_all(&encode_frame(&request).expect("frame"))
        .expect("write");
    input.flush().expect("flush");
    let pong: CoreServiceResponseV1 = read_service_frame(&mut output);
    assert!(matches!(pong.response, CoreServiceResponseKindV1::Pong));
    assert!(child.try_wait().expect("status").is_none());
    input
        .write_all(
            &encode_frame(&CoreServiceRequestV1 {
                message_id: id("service.shutdown"),
                generation: ProcessGeneration(1),
                request: CoreServiceRequestKindV1::Shutdown,
            })
            .expect("frame"),
        )
        .expect("write");
    input.flush().expect("flush");
    let shutdown: CoreServiceResponseV1 = read_service_frame(&mut output);
    assert!(matches!(
        shutdown.response,
        CoreServiceResponseKindV1::ShutdownAck
    ));
    drop(input);
    assert!(child.wait().expect("wait").success());
}

fn read_service_frame<R: Read>(input: &mut R) -> CoreServiceResponseV1 {
    let mut prefix = [0_u8; 4];
    input.read_exact(&mut prefix).expect("prefix");
    let length = u32::from_be_bytes(prefix) as usize;
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + length, 0);
    input.read_exact(&mut frame[4..]).expect("body");
    decode_frame(&frame).expect("response")
}
