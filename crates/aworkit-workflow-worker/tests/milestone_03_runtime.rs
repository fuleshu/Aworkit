use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::Arc,
};

use aworkit_protocol::{
    ApprovalOutcomeV1, CapabilityOutcomeClassV1, CapabilityOutcomeV1, HistoryBackendV1,
    ProcessGeneration, RehydrationEnvelopeV1, StableId, WorkerBudgetV1, WorkerCheckpointV1,
    WorkerControlEnvelopeV1, WorkerControlKindV1, WorkerExecutorKindV1, WorkerFrozenRunSnapshotV1,
    WorkerLoopDescriptorV1, WorkerNodeV1, WorkerOutputEnvelopeV1, WorkerOutputKindV1, WorkerPortV1,
    WorkerProposalKindV1, WorkerTransitionV1, decode_frame, encode_frame,
};
use aworkit_workflow_worker::{
    agent::{AgentErrorV1, AgentLoopConfigV1, AgentLoopV1, SubagentManagerV1},
    branch::{BranchJoinCoordinator, BranchStateV1},
    context::{ChildContextSpec, ChildIntegration, ContextStore},
    gateway::{AdmissionV1, CoreGatewayV1, GatewayError},
    limits::{BudgetEnvelope, LimitLedger, Usage},
    node::{ExecutorRegistryV1, NodeError, NodeOutcomeV1, NodeTaskV1},
    plan::{ExecutionPlanV1, PlanError, canonical_snapshot_hash},
    policy::{
        AttemptDecisionV1, AttemptInputV1, AttemptLedger, AttemptPolicyV1, OutcomeClass, RetryProof,
    },
    routing::{RoutingError, choose_route, evaluate_predicate},
    scheduler::{ExternalCompletionV1, SchedulerV1, TokenStateV1},
    suspension::{
        RehydratorV1, SuspensionControllerV1, SuspensionFrameV1, SuspensionKindV1, checkpoint_hash,
    },
};
use serde_json::json;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("fixture id")
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

fn frozen_snapshot() -> WorkerFrozenRunSnapshotV1 {
    let mut snapshot = WorkerFrozenRunSnapshotV1 {
        snapshot_id: id("snapshot.v1"),
        snapshot_hash: String::new(),
        chat_id: id("chat.v1"),
        run_id: id("run.v1"),
        schema_version: 1,
        compiler_version: "aworkit-worker-v1".to_owned(),
        workflow_hash: hash('a'),
        nodes: vec![
            WorkerNodeV1 {
                node_id: id("start"),
                node_type: "pure.noop".to_owned(),
                node_version: 1,
                contribution_hash: hash('b'),
                inputs: Vec::new(),
                outputs: vec![port("out")],
                executor: WorkerExecutorKindV1::Pure,
                config: json!({"z": 1, "a": {"later": true, "first": 1}}),
                capability_ref: None,
                result_schema_ref: None,
            },
            WorkerNodeV1 {
                node_id: id("done"),
                node_type: "terminal".to_owned(),
                node_version: 1,
                contribution_hash: hash('c'),
                inputs: vec![port("in")],
                outputs: Vec::new(),
                executor: WorkerExecutorKindV1::Terminal,
                config: json!({}),
                capability_ref: None,
                result_schema_ref: None,
            },
        ],
        transitions: vec![WorkerTransitionV1 {
            transition_id: id("edge.start.done"),
            from_node: id("start"),
            from_port: "out".to_owned(),
            to_node: id("done"),
            to_port: "in".to_owned(),
            priority: 0,
            predicate: Some(json!({"always": true})),
            declared_loop_id: None,
        }],
        entry_nodes: vec![id("start")],
        loop_descriptors: Vec::new(),
        join_descriptors: Vec::new(),
        route_rules: Vec::new(),
        authority_manifest_ref: id("authority.v1"),
        authority_manifest_hash: hash('d'),
        capability_bindings: Vec::new(),
        capability_refs: Vec::new(),
        workspace_identity: json!({"root": "/fixture", "device": 1}),
        budget: WorkerBudgetV1 {
            turns: 10,
            attempts: 10,
            tool_calls: 10,
            tokens: 10_000,
            cost_micros: 1_000_000,
            actions: 100,
            depth: 2,
            fanout: 4,
            parallel: 2,
            deadline_ms: 60_000,
        },
        history_mode: HistoryBackendV1::LocalSqlite,
    };
    snapshot.snapshot_hash = canonical_snapshot_hash(&snapshot).expect("snapshot hash");
    snapshot
}

fn start_control(snapshot: WorkerFrozenRunSnapshotV1) -> WorkerControlEnvelopeV1 {
    WorkerControlEnvelopeV1 {
        message_id: id("control.start"),
        chat_id: snapshot.chat_id.clone(),
        run_id: snapshot.run_id.clone(),
        generation: ProcessGeneration(7),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        committed_cursor: 0,
        control: WorkerControlKindV1::Start(snapshot),
    }
}

fn control(
    message_id: &str,
    snapshot: &WorkerFrozenRunSnapshotV1,
    cursor: u64,
    control: WorkerControlKindV1,
) -> WorkerControlEnvelopeV1 {
    WorkerControlEnvelopeV1 {
        message_id: id(message_id),
        chat_id: snapshot.chat_id.clone(),
        run_id: snapshot.run_id.clone(),
        generation: ProcessGeneration(7),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        committed_cursor: cursor,
        control,
    }
}

#[test]
fn frozen_snapshot_hash_is_canonical_and_tamper_evident() {
    let snapshot = frozen_snapshot();
    let mut reordered = snapshot.clone();
    reordered.nodes.reverse();
    reordered.nodes[1].config =
        serde_json::from_str(r#"{"a":{"first":1,"later":true},"z":1}"#).expect("json");
    assert_eq!(
        canonical_snapshot_hash(&snapshot).expect("hash"),
        canonical_snapshot_hash(&reordered).expect("hash")
    );
    ExecutionPlanV1::compile(snapshot.clone(), &snapshot.snapshot_hash).expect("valid plan");
    let mut tampered = snapshot.clone();
    tampered.nodes[0].config = json!({"changed": true});
    assert!(ExecutionPlanV1::compile(tampered, &snapshot.snapshot_hash).is_err());

    let mut incompatible = snapshot;
    incompatible.nodes[1].inputs[0].schema_ref = Some("json:string".to_owned());
    incompatible.snapshot_hash = canonical_snapshot_hash(&incompatible).expect("rehash");
    let declared = incompatible.snapshot_hash.clone();
    assert!(matches!(
        ExecutionPlanV1::compile(incompatible, &declared),
        Err(PlanError::PortMismatch { .. })
    ));
}

#[test]
fn typed_executor_registry_is_exact_sealed_schema_checked_and_broker_only() {
    let contribution_hash = hash('1');
    let mut registry = ExecutorRegistryV1::with_audited_builtins(&contribution_hash, &hash('2'));
    registry.seal();
    assert!(matches!(
        registry.register(
            "late.executor",
            1,
            hash('3'),
            WorkerExecutorKindV1::Pure,
            Arc::new(|_, _, _| Ok(NodeOutcomeV1::Completed { value: json!(null) })),
        ),
        Err(NodeError::RegistrySealed)
    ));
    let node = WorkerNodeV1 {
        node_id: id("node.typed"),
        node_type: "aworkit.noop".to_owned(),
        node_version: 1,
        contribution_hash: contribution_hash.clone(),
        inputs: vec![WorkerPortV1 {
            name: "input".to_owned(),
            schema_ref: Some("json:string".to_owned()),
            required: true,
        }],
        outputs: vec![WorkerPortV1 {
            name: "output".to_owned(),
            schema_ref: Some("json:string".to_owned()),
            required: true,
        }],
        executor: WorkerExecutorKindV1::Pure,
        config: json!({}),
        capability_ref: None,
        result_schema_ref: Some("json:string".to_owned()),
    };
    let task = NodeTaskV1 {
        token_id: id("token.typed"),
        node_id: node.node_id.clone(),
        attempt_id: id("attempt.typed"),
        input_revision: 1,
        cancelled: false,
    };
    assert_eq!(
        registry
            .execute(
                &node,
                &task,
                &json!("valid"),
                &id("authority.typed"),
                &id("budget.typed"),
            )
            .expect("execute"),
        NodeOutcomeV1::Completed {
            value: json!("valid")
        }
    );
    assert!(matches!(
        registry.execute(
            &node,
            &task,
            &json!(42),
            &id("authority.typed"),
            &id("budget.typed"),
        ),
        Err(NodeError::SchemaViolation(_))
    ));
    let mut drifted = node.clone();
    drifted.contribution_hash = hash('4');
    assert!(matches!(
        registry.execute(
            &drifted,
            &task,
            &json!("valid"),
            &id("authority.typed"),
            &id("budget.typed"),
        ),
        Err(NodeError::ContributionMismatch)
    ));

    let brokered = WorkerNodeV1 {
        node_id: id("node.brokered"),
        node_type: "tool.lookup".to_owned(),
        node_version: 1,
        contribution_hash: hash('5'),
        inputs: vec![WorkerPortV1 {
            name: "input".to_owned(),
            schema_ref: Some("json:object".to_owned()),
            required: true,
        }],
        outputs: Vec::new(),
        executor: WorkerExecutorKindV1::Brokered,
        config: json!({"operation": "lookup"}),
        capability_ref: Some(id("capability.lookup")),
        result_schema_ref: Some("json:object".to_owned()),
    };
    let broker_task = NodeTaskV1 {
        node_id: brokered.node_id.clone(),
        ..task
    };
    let proposed = ExecutorRegistryV1::broker_proposal(
        &brokered,
        &broker_task,
        &json!({"query": "Aworkit"}),
        id("authority.typed"),
        id("budget.typed"),
    )
    .expect("broker proposal");
    assert!(matches!(proposed, NodeOutcomeV1::NeedCapability(_)));
}

#[test]
fn scheduler_progress_is_deterministic_and_commit_ack_gated() {
    let snapshot = frozen_snapshot();
    let plan = ExecutionPlanV1::compile(snapshot.clone(), &snapshot.snapshot_hash).expect("plan");
    let mut scheduler = SchedulerV1::new(plan);
    let entries = scheduler.seed_entries(1).expect("entries");
    assert_eq!(entries.len(), 1);
    let claimed = scheduler.claim_next().expect("ready token");
    let proposal = scheduler
        .propose_transition(&claimed.token_id, json!({}))
        .expect("proposal");
    assert!(
        scheduler.claim_next().is_none(),
        "destination is gated by ack"
    );
    let admission = scheduler
        .acknowledge_transition(&proposal.proposal_id, 1, 2)
        .expect("ack");
    assert!(!admission.duplicate);
    assert_eq!(
        admission.admitted_token.as_ref().map(|token| token.state),
        Some(TokenStateV1::Ready)
    );
    let duplicate = scheduler
        .acknowledge_transition(&proposal.proposal_id, 1, 2)
        .expect("idempotent ack");
    assert!(duplicate.duplicate);
    let terminal_token = scheduler.claim_next().expect("terminal ready");
    assert_eq!(
        admission
            .admitted_token
            .as_ref()
            .map(|token| &token.token_id),
        Some(&terminal_token.token_id)
    );
    let terminal = scheduler
        .propose_terminal(
            &terminal_token.token_id,
            "completed",
            json!({"result": "ok"}),
        )
        .expect("terminal proposal");
    assert!(
        scheduler
            .acknowledge_terminal(&terminal.proposal_id, 1)
            .is_err(),
        "a new acknowledgement must advance the committed cursor"
    );
    assert!(
        !scheduler
            .acknowledge_terminal(&terminal.proposal_id, 2)
            .expect("terminal commit")
            .duplicate
    );
    assert!(
        scheduler
            .acknowledge_terminal(&terminal.proposal_id, 2)
            .expect("terminal duplicate")
            .duplicate
    );
    assert!(scheduler.is_quiescent());
    scheduler
        .enqueue(id("start"), 3, "root/branch/1".to_owned())
        .expect("branch one");
    scheduler
        .enqueue(id("start"), 3, "root/branch/10".to_owned())
        .expect("branch ten");
    assert_eq!(scheduler.cancel_lineage("root/branch/1"), 1);
    let survivor = scheduler.claim_next().expect("prefix neighbor survives");
    assert_eq!(survivor.branch_lineage, "root/branch/10");
    assert_eq!(scheduler.cancel_lineage("root/branch/10"), 1);
    assert!(scheduler.is_quiescent());
    let restored = SchedulerV1::restore(
        ExecutionPlanV1::compile(snapshot.clone(), &snapshot.snapshot_hash).expect("plan"),
        scheduler.checkpoint(),
    )
    .expect("restore");
    assert_eq!(restored.checkpoint(), scheduler.checkpoint());
}

#[test]
fn suspended_wait_consumes_durable_input_before_same_scheduler_reenters_input() {
    let mut snapshot = frozen_snapshot();
    snapshot.nodes[1].node_type = "wait".to_owned();
    snapshot.nodes[1].executor = WorkerExecutorKindV1::Wait;
    snapshot.snapshot_hash = canonical_snapshot_hash(&snapshot).expect("rehash Wait graph");
    let declared = snapshot.snapshot_hash.clone();
    let plan = ExecutionPlanV1::compile(snapshot.clone(), &declared).expect("Wait plan");
    let mut scheduler = SchedulerV1::new(plan);
    scheduler.seed_entries(0).expect("initial Input");
    let input = scheduler.claim_next().expect("claim Input");
    let transition = scheduler
        .propose_transition(&input.token_id, json!({"inputCommitted":true}))
        .expect("Input proposal");
    let admitted_wait = scheduler
        .acknowledge_transition(&transition.proposal_id, 1, 1)
        .expect("Input ack")
        .admitted_token
        .expect("Wait admitted");
    let wait = scheduler.claim_next().expect("claim Wait");
    assert_eq!(wait.token_id, admitted_wait.token_id);
    scheduler.suspend(&wait.token_id).expect("suspend Wait");
    let checkpoint = scheduler.checkpoint();
    assert_eq!(checkpoint.next_token_ordinal, 3);

    let plan = ExecutionPlanV1::compile(snapshot.clone(), &declared).expect("restored Wait plan");
    let mut restored = SchedulerV1::restore(plan, checkpoint).expect("restore suspended Wait");
    restored.resume(&wait.token_id).expect("resume Wait");
    let resumed_wait = restored.claim_next().expect("claim resumed Wait");
    assert_eq!(resumed_wait.token_id, wait.token_id);
    let input_received = restored
        .propose_wait_input(&resumed_wait.token_id, json!({"inputReceived":true}))
        .expect("propose input_received");
    assert!(
        restored
            .acknowledge_terminal(&input_received.proposal_id, 2)
            .is_err(),
        "a Wait proposal cannot be acknowledged through the terminal API"
    );
    let acknowledged = restored
        .acknowledge_wait_input(&input_received.proposal_id, 2)
        .expect("ack input_received");
    assert!(!acknowledged.duplicate);
    assert!(
        restored
            .acknowledge_wait_input(&input_received.proposal_id, 2)
            .expect("duplicate input_received ack")
            .duplicate
    );
    let continued_input = restored
        .enqueue(id("start"), 2, resumed_wait.branch_lineage)
        .expect("same-scheduler Input");
    assert_ne!(continued_input.token_id, input.token_id);
    assert_eq!(restored.checkpoint().next_token_ordinal, 4);
    assert!(restored.checkpoint().tokens.iter().any(|token| {
        token.token_id == wait.token_id
            && token.state == TokenStateV1::Completed
            && token.external_completion == Some(ExternalCompletionV1::WaitInputReceived)
    }));
    let continued_input = restored.claim_next().expect("claim continued Input");
    assert_eq!(continued_input.context_revision, 2);
    let restored_again = SchedulerV1::restore(
        ExecutionPlanV1::compile(snapshot, &declared).expect("second restored plan"),
        restored.checkpoint(),
    )
    .expect("restore consumed Wait and continued Input");
    assert_eq!(restored_again.checkpoint(), restored.checkpoint());
}

#[test]
fn scheduler_reserves_frozen_loop_capacity_before_commit() {
    let mut snapshot = frozen_snapshot();
    snapshot.nodes[0].inputs = vec![port("in")];
    snapshot.nodes[1].executor = WorkerExecutorKindV1::Pure;
    snapshot.nodes[1].outputs = vec![port("back")];
    snapshot.transitions.push(WorkerTransitionV1 {
        transition_id: id("edge.done.start"),
        from_node: id("done"),
        from_port: "back".to_owned(),
        to_node: id("start"),
        to_port: "in".to_owned(),
        priority: 0,
        predicate: Some(json!({"always": true})),
        declared_loop_id: Some(id("loop.one")),
    });
    snapshot.loop_descriptors = vec![WorkerLoopDescriptorV1 {
        loop_id: id("loop.one"),
        maximum_iterations: 1,
        body_entry: id("start"),
        body_exit: id("done"),
    }];
    snapshot.snapshot_hash = canonical_snapshot_hash(&snapshot).expect("loop hash");
    let declared = snapshot.snapshot_hash.clone();
    let plan = ExecutionPlanV1::compile(snapshot, &declared).expect("bounded loop plan");
    let mut scheduler = SchedulerV1::new(plan);
    let first = scheduler
        .enqueue(id("done"), 1, "root/a".to_owned())
        .expect("first");
    let second = scheduler
        .enqueue(id("done"), 1, "root/b".to_owned())
        .expect("second");
    assert_eq!(
        scheduler.claim_next().expect("first claim").token_id,
        first.token_id
    );
    assert_eq!(
        scheduler.claim_next().expect("second claim").token_id,
        second.token_id
    );
    let proposal = scheduler
        .propose_transition(&first.token_id, json!({}))
        .expect("first iteration reserved");
    assert!(
        scheduler
            .propose_transition(&second.token_id, json!({}))
            .is_err(),
        "an in-flight iteration consumes the sole frozen slot"
    );
    scheduler
        .acknowledge_transition(&proposal.proposal_id, 1, 2)
        .expect("commit first iteration");
    assert!(
        scheduler
            .propose_transition(&second.token_id, json!({}))
            .is_err(),
        "committed loop exhaustion remains enforced"
    );
}

#[test]
fn route_predicates_have_no_truthy_or_implicit_fallback_behavior() {
    assert!(
        evaluate_predicate(
            &json!({"exists": "result.ok"}),
            &json!({"result": {"ok": false}})
        )
        .expect("predicate")
    );
    assert!(
        !evaluate_predicate(
            &json!({"eq": {"path": "result.ok", "value": true}}),
            &json!({"result": {"ok": false}})
        )
        .expect("predicate")
    );
    assert!(evaluate_predicate(&json!({"unknown": true}), &json!({})).is_err());
    assert!(matches!(
        choose_route(&id("router"), &[], &json!({})),
        Err(RoutingError::NoMatch(_))
    ));
}

#[test]
fn branch_frames_join_in_declared_order_and_integrate_once() {
    let mut branches = BranchJoinCoordinator::new();
    let declared = vec![id("branch.a"), id("branch.b")];
    let records = branches
        .open_parallel(
            id("frame.one"),
            id("token.parent"),
            id("join.node"),
            &declared,
            "ordered_array",
        )
        .expect("frame");
    assert_eq!(records[0].state, BranchStateV1::Ready);
    branches
        .complete(&id("frame.one"), &id("branch.b"), json!(2))
        .expect("out of order completion");
    branches
        .complete(&id("frame.one"), &id("branch.a"), json!(1))
        .expect("completion");
    assert_eq!(
        branches.integrate(&id("frame.one")).expect("join"),
        json!([1, 2])
    );
    assert!(branches.integrate(&id("frame.one")).is_err());
    let restored = BranchJoinCoordinator::restore(branches.checkpoint()).expect("checkpoint");
    assert!(restored.is_join_ready(&id("frame.one")));

    let for_each = branches
        .open_for_each(
            id("frame.foreach"),
            id("token.foreach"),
            id("join.foreach"),
            &[json!({"index": 0}), json!({"index": 1})],
            2,
            "ordered_array",
        )
        .expect("bounded for-each");
    assert_eq!(for_each.len(), 2);
    branches
        .complete(
            &id("frame.foreach"),
            &for_each[0].0.branch_id,
            json!("first"),
        )
        .expect("first result");
    branches
        .cancel_frame(&id("frame.foreach"))
        .expect("cancel remainder");
    assert_eq!(
        branches.integrate(&id("frame.foreach")).expect("join"),
        json!(["first", null])
    );
    assert!(
        branches
            .open_for_each(
                id("frame.unbounded"),
                id("token.unbounded"),
                id("join.unbounded"),
                &[json!(1), json!(2)],
                1,
                "ordered_array",
            )
            .is_err()
    );
}

#[test]
fn hierarchical_budget_reservations_charge_every_ancestor_once() {
    let budget = BudgetEnvelope {
        turns: 10,
        attempts: 10,
        tool_calls: 10,
        tokens: 1_000,
        cost_micros: 1_000,
        actions: 10,
        max_depth: 2,
        max_fan_out: 2,
        max_parallel: 2,
        deadline_tick: 100,
    };
    let mut ledger = LimitLedger::new("root", budget).expect("ledger");
    ledger
        .create_child("child", "root", BudgetEnvelope { turns: 5, ..budget })
        .expect("child");
    ledger
        .reserve(
            "child",
            "reservation.one",
            Usage {
                turns: 2,
                attempts: 1,
                ..Usage::default()
            },
        )
        .expect("reserve");
    assert_eq!(ledger.remaining("root").expect("root").turns, 8);
    assert_eq!(ledger.remaining("child").expect("child").turns, 3);
    ledger
        .charge(
            "reservation.one",
            "charge.one",
            Usage {
                turns: 1,
                attempts: 1,
                ..Usage::default()
            },
        )
        .expect("charge");
    assert_eq!(ledger.remaining("root").expect("root").turns, 9);
    assert_eq!(ledger.remaining("child").expect("child").turns, 4);
    let restored = LimitLedger::restore(ledger.checkpoint(), 20).expect("restore");
    assert_eq!(restored.remaining("root").expect("root").deadline_tick, 120);

    assert_eq!(ledger.next_loop_iteration("root", "bounded", 2), Ok(1));
    assert_eq!(ledger.next_loop_iteration("root", "bounded", 2), Ok(2));
    assert!(ledger.next_loop_iteration("root", "bounded", 2).is_err());
}

#[test]
fn attempt_policy_covers_retry_reconcile_evaluator_gate_fallback_and_exhaustion() {
    let full = AttemptPolicyV1 {
        policy_id: "policy.full".to_owned(),
        max_attempts: 2,
        fallback_node_id: Some("fallback".to_owned()),
        feedback_transition_id: Some("feedback".to_owned()),
        evaluator_transition_id: Some("evaluation_failed".to_owned()),
        approval_gate_id: Some("approval.required".to_owned()),
    };
    let input = |operation: &str, attempt: &str, outcome: OutcomeClass| AttemptInputV1 {
        operation_id: operation.to_owned(),
        attempt_id: attempt.to_owned(),
        attempt_ordinal: 1,
        outcome,
        retry_proof: None,
        evaluator_passed: None,
        gate_passed: None,
        cancelled: false,
    };
    let mut ledger = AttemptLedger::default();
    assert!(matches!(
        ledger
            .decide(
                &full,
                input(
                    "operation.retry",
                    "attempt.retry",
                    OutcomeClass::DefiniteNotStarted,
                ),
            )
            .expect("safe retry"),
        AttemptDecisionV1::RetryWithNewAttempt { next_ordinal: 2 }
    ));

    let mut evaluator = input(
        "operation.evaluator",
        "attempt.evaluator",
        OutcomeClass::Succeeded,
    );
    evaluator.evaluator_passed = Some(false);
    assert!(matches!(
        ledger.decide(&full, evaluator).expect("evaluator edge"),
        AttemptDecisionV1::FollowExistingEdge { ref transition_id }
            if transition_id == "evaluation_failed"
    ));

    let mut gate = input("operation.gate", "attempt.gate", OutcomeClass::Succeeded);
    gate.gate_passed = Some(false);
    assert!(matches!(
        ledger.decide(&full, gate).expect("gate"),
        AttemptDecisionV1::AwaitApproval { ref gate_id } if gate_id == "approval.required"
    ));

    let mut known = input(
        "operation.known",
        "attempt.known",
        OutcomeClass::FailedKnownStarted,
    );
    known.retry_proof = Some(RetryProof {
        invocation_id: "invocation.known".to_owned(),
        descriptor_idempotent: true,
        same_id_deduplicated: true,
        effect_absence_proven: false,
    });
    assert!(matches!(
        ledger.decide(&full, known).expect("same-id reconcile"),
        AttemptDecisionV1::ReconcileSameInvocation { ref invocation_id }
            if invocation_id == "invocation.known"
    ));

    let fallback = AttemptPolicyV1 {
        max_attempts: 1,
        feedback_transition_id: None,
        evaluator_transition_id: None,
        approval_gate_id: None,
        ..full
    };
    assert!(matches!(
        ledger
            .decide(
                &fallback,
                input(
                    "operation.exhausted",
                    "attempt.exhausted",
                    OutcomeClass::ContractFailure,
                ),
            )
            .expect("declared fallback"),
        AttemptDecisionV1::SelectExistingFallback { ref node_id } if node_id == "fallback"
    ));
}

#[test]
fn uncertain_effects_never_create_a_new_invocation_id() {
    let policy = AttemptPolicyV1 {
        policy_id: "policy.one".to_owned(),
        max_attempts: 3,
        fallback_node_id: Some("fallback".to_owned()),
        feedback_transition_id: None,
        evaluator_transition_id: None,
        approval_gate_id: None,
    };
    let input = AttemptInputV1 {
        operation_id: "operation.one".to_owned(),
        attempt_id: "attempt.one".to_owned(),
        attempt_ordinal: 1,
        outcome: OutcomeClass::OutcomeUncertain,
        retry_proof: Some(RetryProof {
            invocation_id: "invocation.one".to_owned(),
            descriptor_idempotent: true,
            same_id_deduplicated: true,
            effect_absence_proven: false,
        }),
        evaluator_passed: None,
        gate_passed: None,
        cancelled: false,
    };
    let mut ledger = AttemptLedger::default();
    assert!(matches!(
        ledger.decide(&policy, input.clone()).expect("decision"),
        AttemptDecisionV1::RequireUserDecision
    ));
    assert!(!ledger.may_send_invocation("invocation.one"));
    let checkpoint = ledger.checkpoint();
    assert_eq!(
        ledger.decide(&policy, input).expect("idempotent decision"),
        checkpoint.0[0].decision
    );
    let restored = AttemptLedger::restore(checkpoint.clone()).expect("attempt restore");
    assert!(!restored.may_send_invocation("invocation.one"));
    let mut corrupt = checkpoint;
    corrupt.0.push(corrupt.0[0].clone());
    assert!(AttemptLedger::restore(corrupt).is_err());
}

#[test]
fn suspensions_are_exact_deduplicated_checkpointable_and_cancellable() {
    let mut controller = SuspensionControllerV1::new();
    controller
        .suspend(SuspensionFrameV1 {
            suspension_id: id("suspension.input"),
            token_id: id("token.input"),
            kind: SuspensionKindV1::Input {
                input_id: id("input.expected"),
            },
            resolved: false,
        })
        .expect("input wait");
    assert!(
        controller
            .resolve_input(&id("suspension.input"), &id("input.wrong"))
            .is_err()
    );
    assert!(
        controller
            .resolve_input(&id("suspension.input"), &id("input.expected"))
            .expect("input")
    );
    assert!(
        !controller
            .resolve_input(&id("suspension.input"), &id("input.expected"))
            .expect("duplicate input")
    );

    controller
        .suspend(SuspensionFrameV1 {
            suspension_id: id("suspension.approval"),
            token_id: id("token.approval"),
            kind: SuspensionKindV1::Approval {
                approval_id: id("approval.expected"),
            },
            resolved: false,
        })
        .expect("approval wait");
    assert!(
        controller
            .resolve_approval(
                &id("suspension.approval"),
                &id("approval.expected"),
                ApprovalOutcomeV1::Approved,
            )
            .expect("approval")
    );

    controller
        .suspend(SuspensionFrameV1 {
            suspension_id: id("suspension.pause"),
            token_id: id("token.pause"),
            kind: SuspensionKindV1::Input {
                input_id: id("input.later"),
            },
            resolved: false,
        })
        .expect("pausable wait");
    assert!(
        controller
            .apply_control(&id("control.pause"), "run", false)
            .expect("pause")
    );
    let mut restored =
        SuspensionControllerV1::restore(controller.checkpoint()).expect("restore pause");
    assert!(
        !restored
            .apply_control(&id("control.pause"), "run", false)
            .expect("duplicate pause")
    );
    assert_eq!(
        restored
            .resume_pause(&id("control.resume"), "run")
            .expect("resume"),
        vec![id("token.pause")]
    );
    restored
        .suspend(SuspensionFrameV1 {
            suspension_id: id("suspension.cancel"),
            token_id: id("token.cancel"),
            kind: SuspensionKindV1::Input {
                input_id: id("input.cancelled"),
            },
            resolved: false,
        })
        .expect("cancel wait");
    restored
        .apply_control(&id("control.cancel"), "run", true)
        .expect("cancel");
    assert!(matches!(
        restored.unresolved()[0].kind,
        SuspensionKindV1::Cancelling { .. }
    ));
}

#[test]
fn model_agent_settles_turns_and_subagents_keep_context_and_budgets_isolated() {
    let budget = BudgetEnvelope {
        turns: 10,
        attempts: 10,
        tool_calls: 10,
        tokens: 1_000,
        cost_micros: 10_000,
        actions: 10,
        max_depth: 1,
        max_fan_out: 2,
        max_parallel: 2,
        deadline_tick: 100,
    };
    let mut limits = LimitLedger::new("root", budget).expect("limits");
    let config = AgentLoopConfigV1 {
        loop_id: id("agent.loop"),
        node_id: id("agent.node"),
        model_capability_ref: id("capability.model"),
        authority_manifest_ref: id("authority.agent"),
        budget_ref: id("budget.agent"),
        scope_id: "root".to_owned(),
        maximum_turns: 2,
        turn_reservation: Usage {
            turns: 1,
            attempts: 1,
            tokens: 100,
            ..Usage::default()
        },
        context_pointers: vec!["/topic".to_owned()],
        allowed_tool_capability_refs: vec![id("capability.lookup")],
    };
    let mut agent = AgentLoopV1::new(config).expect("agent");
    let first = agent
        .propose_model_turn(&json!({"topic": "public", "secret": "hidden"}), &mut limits)
        .expect("turn proposal");
    assert_eq!(first.payload["context"], json!({"/topic": "public"}));
    assert!(matches!(
        agent.propose_model_turn(&json!({}), &mut limits),
        Err(AgentErrorV1::InvocationPending)
    ));

    let checkpoint = agent.checkpoint();
    let mut agent = AgentLoopV1::restore(checkpoint).expect("agent restore");
    let limit_checkpoint = limits.checkpoint();
    let mut limits = LimitLedger::restore(limit_checkpoint, 10).expect("limit restore");
    let success = CapabilityOutcomeV1 {
        outcome_id: id("outcome.agent.first"),
        invocation_id: first.invocation_id,
        class: CapabilityOutcomeClassV1::Success,
        retry_safe_proof: false,
        payload: json!({"answer": 42}),
        usage: Some(json!({"turns": 1, "attempts": 1, "tokens": 25})),
    };
    let actual = Usage {
        turns: 1,
        attempts: 1,
        tokens: 25,
        ..Usage::default()
    };
    assert!(
        agent
            .settle_committed_outcome(&success, &mut limits, actual)
            .expect("settle")
    );
    assert!(
        !agent
            .settle_committed_outcome(&success, &mut limits, actual)
            .expect("duplicate settlement")
    );
    assert_eq!(limits.remaining("root").expect("root").tokens, 975);
    agent
        .validate_tool_capability(&id("capability.lookup"))
        .expect("allowed tool");
    assert!(
        agent
            .validate_tool_capability(&id("capability.forbidden"))
            .is_err()
    );

    let second = agent
        .propose_model_turn(&json!({"topic": "public"}), &mut limits)
        .expect("second turn");
    assert!(matches!(
        agent.cancel(),
        Err(AgentErrorV1::InvocationPending)
    ));
    agent
        .settle_committed_outcome(
            &CapabilityOutcomeV1 {
                outcome_id: id("outcome.agent.denied"),
                invocation_id: second.invocation_id,
                class: CapabilityOutcomeClassV1::Denied,
                retry_safe_proof: false,
                payload: json!({}),
                usage: Some(json!({"turns": 1, "attempts": 1, "tokens": 10})),
            },
            &mut limits,
            Usage {
                turns: 1,
                attempts: 1,
                tokens: 10,
                ..Usage::default()
            },
        )
        .expect("denied turn settlement");
    assert!(matches!(
        agent.propose_model_turn(&json!({"topic": "public"}), &mut limits),
        Err(AgentErrorV1::Cancelled)
    ));

    let mut context = ContextStore::new(json!({
        "topic": "public",
        "secret": "not delegated",
        "summaries": []
    }));
    let child_budget = BudgetEnvelope {
        max_depth: 1,
        ..budget
    };
    let mut children = SubagentManagerV1::new();
    let child = children
        .spawn(
            &mut context,
            &mut limits,
            id("child.one"),
            "root",
            child_budget,
            ChildContextSpec {
                child_id: "child.one".to_owned(),
                parent_revision: 1,
                selected_pointers: vec![vec!["topic".to_owned()]],
                instructions: json!("summarize"),
            },
        )
        .expect("child");
    assert!(
        context
            .get(child.child_root_revision)
            .expect("child root")
            .value
            .get("secret")
            .is_none()
    );
    assert!(
        children
            .spawn(
                &mut context,
                &mut limits,
                id("child.grandchild"),
                &child.scope_id,
                child_budget,
                ChildContextSpec {
                    child_id: "child.grandchild".to_owned(),
                    parent_revision: child.child_root_revision,
                    selected_pointers: Vec::new(),
                    instructions: json!("too deep"),
                },
            )
            .is_err()
    );
    children
        .complete(
            &id("child.one"),
            child.child_root_revision,
            json!({"summary": "safe"}),
        )
        .expect("complete child");
    let integrated = children
        .integrate(
            &mut context,
            &mut limits,
            &id("child.one"),
            1,
            ChildIntegration::AppendSummaryAtKey {
                key: "summaries".to_owned(),
            },
        )
        .expect("integrate child once");
    assert_eq!(
        context.get(integrated).expect("integrated").value["summaries"],
        json!([{"summary": "safe"}])
    );
    assert!(
        children
            .integrate(
                &mut context,
                &mut limits,
                &id("child.one"),
                integrated,
                ChildIntegration::ReplaceAtKey {
                    key: "again".to_owned(),
                },
            )
            .is_err()
    );
}

#[test]
fn aggregate_agent_run_reserves_maxima_and_settles_actual_provider_and_tool_usage() {
    let budget = BudgetEnvelope {
        turns: 8,
        attempts: 8,
        tool_calls: 32,
        tokens: 1_000,
        cost_micros: 10_000,
        actions: 40,
        max_depth: 0,
        max_fan_out: 1,
        max_parallel: 1,
        deadline_tick: 100,
    };
    let mut limits = LimitLedger::new("root", budget).expect("limits");
    let mut agent = AgentLoopV1::new(AgentLoopConfigV1 {
        loop_id: id("agent.aggregate-loop"),
        node_id: id("agent.aggregate-node"),
        model_capability_ref: id("capability.aggregate-model"),
        authority_manifest_ref: id("authority.aggregate"),
        budget_ref: id("budget.aggregate"),
        scope_id: "root".into(),
        maximum_turns: 8,
        turn_reservation: Usage {
            turns: 8,
            attempts: 8,
            tool_calls: 32,
            tokens: 1_000,
            cost_micros: 10_000,
            actions: 40,
        },
        context_pointers: Vec::new(),
        allowed_tool_capability_refs: vec![id("capability.aggregate-tool")],
    })
    .expect("agent");
    let proposal = agent
        .propose_model_turn(&json!({"messages":[]}), &mut limits)
        .expect("aggregate proposal");
    let outcome = CapabilityOutcomeV1 {
        outcome_id: id("outcome.aggregate-success"),
        invocation_id: proposal.invocation_id,
        class: CapabilityOutcomeClassV1::Success,
        retry_safe_proof: false,
        payload: json!({"answer":"done"}),
        usage: None,
    };
    let actual = Usage {
        turns: 2,
        attempts: 2,
        tool_calls: 3,
        tokens: 125,
        cost_micros: 0,
        actions: 5,
    };
    assert!(
        agent
            .settle_committed_run_outcome(&outcome, &mut limits, actual)
            .expect("aggregate settlement")
    );
    assert_eq!(limits.remaining("root").expect("remaining").turns, 6);
    assert_eq!(limits.remaining("root").expect("remaining").tool_calls, 29);
    assert_eq!(limits.remaining("root").expect("remaining").tokens, 875);
    assert!(
        !agent
            .settle_committed_run_outcome(&outcome, &mut limits, Usage::default())
            .expect("duplicate aggregate settlement")
    );

    let mut no_start_limits = LimitLedger::new("no-start", budget).expect("no-start limits");
    let mut no_start_agent = AgentLoopV1::new(AgentLoopConfigV1 {
        loop_id: id("agent.no-start-loop"),
        node_id: id("agent.no-start-node"),
        model_capability_ref: id("capability.no-start-model"),
        authority_manifest_ref: id("authority.no-start"),
        budget_ref: id("budget.no-start"),
        scope_id: "no-start".into(),
        maximum_turns: 8,
        turn_reservation: Usage {
            turns: 8,
            attempts: 8,
            tool_calls: 32,
            tokens: 1_000,
            cost_micros: 10_000,
            actions: 40,
        },
        context_pointers: Vec::new(),
        allowed_tool_capability_refs: Vec::new(),
    })
    .expect("no-start agent");
    let proposal = no_start_agent
        .propose_model_turn(&json!({}), &mut no_start_limits)
        .expect("no-start proposal");
    let no_start = CapabilityOutcomeV1 {
        outcome_id: id("outcome.aggregate-no-start"),
        invocation_id: proposal.invocation_id,
        class: CapabilityOutcomeClassV1::DefiniteNotStarted,
        retry_safe_proof: true,
        payload: json!({}),
        usage: None,
    };
    assert!(matches!(
        no_start_agent.settle_committed_run_outcome(
            &no_start,
            &mut no_start_limits,
            Usage {
                turns: 9,
                attempts: 9,
                actions: 9,
                ..Usage::default()
            },
        ),
        Err(AgentErrorV1::Budget(_))
    ));
    let mut mismatched = no_start.clone();
    mismatched.invocation_id = id("invocation.not-pending");
    assert!(matches!(
        no_start_agent.settle_committed_run_outcome(
            &mismatched,
            &mut no_start_limits,
            Usage::default(),
        ),
        Err(AgentErrorV1::OutcomeMismatch)
    ));
    no_start_agent
        .settle_committed_run_outcome(&no_start, &mut no_start_limits, Usage::default())
        .expect("zero-use definite no-start");
    assert_eq!(
        no_start_limits.remaining("no-start").expect("remaining"),
        budget
    );
}

#[test]
fn rehydration_validates_generation_hash_cursor_and_reconciled_outcomes() {
    let snapshot = frozen_snapshot();
    let plan = ExecutionPlanV1::compile(snapshot.clone(), &snapshot.snapshot_hash).expect("plan");
    let mut checkpoint = WorkerCheckpointV1 {
        checkpoint_id: id("checkpoint.one"),
        snapshot_hash: snapshot.snapshot_hash.clone(),
        plan_hash: plan.fingerprint().to_owned(),
        checkpoint_hash: String::new(),
        prior_generation: ProcessGeneration(7),
        committed_cursor: 10,
        proposal_sequence: 4,
        token_frontier: json!([]),
        context_heads: json!([1]),
        context_revision_dag: json!({}),
        branch_frames: json!([]),
        loop_frames: json!([]),
        budget_state: json!({}),
        attempt_state: json!({}),
        no_resend_state: json!({"invocations": ["invocation.one"]}),
        suspension_state: json!([]),
        child_frames: json!([]),
    };
    checkpoint.checkpoint_hash = checkpoint_hash(&checkpoint).expect("checkpoint hash");
    let envelope = RehydrationEnvelopeV1 {
        snapshot,
        checkpoint,
        replacement_generation: ProcessGeneration(8),
        committed_deltas: vec![json!({"cursor": 11, "kind": "already_committed"})],
        reconciled_outcomes: Vec::new(),
    };
    let restored = RehydratorV1::restore(envelope.clone()).expect("rehydrate");
    assert_eq!(restored.committed_deltas.len(), 1);
    let mut stale = envelope;
    stale.replacement_generation = ProcessGeneration(7);
    assert!(RehydratorV1::restore(stale).is_err());
}

#[test]
fn gateway_fences_identity_retransmits_same_ids_and_reserves_controls() {
    let snapshot = frozen_snapshot();
    let start = start_control(snapshot.clone());
    let mut gateway = CoreGatewayV1::with_bounds(1, 8 * 1024);
    assert_eq!(
        gateway.admit_control(&start).expect("start"),
        AdmissionV1::New
    );
    assert_eq!(
        gateway.admit_control(&start).expect("duplicate"),
        AdmissionV1::Duplicate
    );
    let mut conflicting = start.clone();
    conflicting.committed_cursor = 1;
    assert!(matches!(
        gateway.admit_control(&conflicting),
        Err(GatewayError::MessageIdentityConflict)
    ));
    let ready = gateway
        .emit(WorkerProposalKindV1::Ready {
            plan_fingerprint: hash('e'),
        })
        .expect("ready");
    assert!(matches!(
        gateway.emit(WorkerProposalKindV1::Health { facts: json!({}) }),
        Err(GatewayError::Backpressure)
    ));
    let urgent = gateway
        .emit_reserved(WorkerProposalKindV1::Terminal {
            outcome: "cancelled".to_owned(),
            facts: json!({}),
        })
        .expect("reserved control lane");
    assert_eq!(
        gateway.retransmit_pending()[0].proposal_id,
        ready.proposal_id
    );
    assert_eq!(
        gateway.retransmit_pending()[1].proposal_id,
        urgent.proposal_id
    );
    gateway
        .admit_control(&control(
            "control.ack",
            &snapshot,
            0,
            WorkerControlKindV1::CommittedAck {
                proposal_id: ready.proposal_id,
                committed_cursor: 1,
            },
        ))
        .expect("ack");
    assert_eq!(gateway.committed_cursor(), 1);
}

#[test]
fn framed_stdio_worker_is_a_real_bounded_service_until_shutdown() {
    let snapshot = frozen_snapshot();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aworkit-workflow-worker"))
        .arg("--serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn worker");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    stdin
        .write_all(&encode_frame(&start_control(snapshot.clone())).expect("frame"))
        .expect("write start");
    stdin.flush().expect("flush");
    let handshake = read_frame(&mut stdout);
    assert!(matches!(handshake.output, WorkerOutputKindV1::Handshake(_)));
    let ready = read_frame(&mut stdout);
    assert!(matches!(
        ready.output,
        WorkerOutputKindV1::Proposal(ref proposal)
            if matches!(proposal.proposal, WorkerProposalKindV1::Ready { .. })
    ));
    let heartbeat = read_frame(&mut stdout);
    assert!(matches!(heartbeat.output, WorkerOutputKindV1::Heartbeat(_)));
    assert!(
        child.try_wait().expect("status").is_none(),
        "worker stays alive"
    );
    stdin
        .write_all(
            &encode_frame(&control(
                "control.shutdown",
                &snapshot,
                0,
                WorkerControlKindV1::Shutdown {
                    control_id: id("shutdown.one"),
                },
            ))
            .expect("frame"),
        )
        .expect("write shutdown");
    stdin.flush().expect("flush");
    let heartbeat = read_frame(&mut stdout);
    assert!(matches!(heartbeat.output, WorkerOutputKindV1::Heartbeat(_)));
    let shutdown = read_frame(&mut stdout);
    assert!(matches!(
        shutdown.output,
        WorkerOutputKindV1::ShutdownAck { .. }
    ));
    drop(stdin);
    assert!(child.wait().expect("wait").success());
}

fn read_frame<R: Read>(input: &mut R) -> WorkerOutputEnvelopeV1 {
    let mut prefix = [0_u8; 4];
    input.read_exact(&mut prefix).expect("frame prefix");
    let length = u32::from_be_bytes(prefix) as usize;
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + length, 0);
    input.read_exact(&mut frame[4..]).expect("frame body");
    decode_frame(&frame).expect("decode proposal")
}
