//! Milestone 03 executable checks using only simulated core-facing values.

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_workflow_worker::{
    AgentLoop, AttemptDecision, AttemptPolicy, Budget, ContextStore, EffectOutcome, ExecutionPlan,
    FrozenRunSnapshot, JoinStrategy, LimitController, NodeOutcome, NodeTask, PlanNode, Rehydrator,
    Reservation, RouteDecision, Scheduler, SubagentManager, SubagentRequest, Suspension,
    SuspensionController, Token, Transition,
};
use serde_json::json;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("fixture ID")
}

fn snapshot() -> FrozenRunSnapshot {
    FrozenRunSnapshot {
        snapshot_id: id("snapshot-1"),
        snapshot_hash: "sha256-fixture".to_owned(),
        generation: ProcessGeneration(4),
        nodes: vec![
            PlanNode {
                id: id("start"),
                node_type: "noop".to_owned(),
                version: 1,
                config: json!({}),
            },
            PlanNode {
                id: id("done"),
                node_type: "set_value".to_owned(),
                version: 1,
                config: json!({"result": "done"}),
            },
        ],
        transitions: vec![Transition {
            id: id("edge-start-done"),
            from: id("start"),
            from_port: "default".to_owned(),
            to: id("done"),
            to_port: "input".to_owned(),
            priority: 0,
            loop_bound: None,
        }],
    }
}

fn plan() -> ExecutionPlan {
    ExecutionPlan::compile(snapshot(), ProcessGeneration(4)).expect("valid frozen plan")
}

#[test]
fn validates_graphs_and_keeps_brokered_work_effect_free() {
    let mut invalid = snapshot();
    invalid.transitions[0].to = id("missing");
    assert!(ExecutionPlan::compile(invalid, ProcessGeneration(4)).is_err());
    let registry = aworkit_workflow_worker::ExecutorRegistry::with_builtins();
    let broker = PlanNode {
        id: id("broker"),
        node_type: "broker".to_owned(),
        version: 1,
        config: json!({"kind": "tool"}),
    };
    let outcome = registry
        .execute(
            &broker,
            &NodeTask {
                node_id: id("broker"),
                attempt: 1,
                input_revision: 1,
            },
            &json!({"query": "x"}),
        )
        .expect("registered executor");
    assert!(matches!(outcome, NodeOutcome::Proposed(proposal) if proposal.kind == "tool"));
}

#[test]
fn deterministic_routes_and_context_lineage_are_explicit() {
    let mut scheduler = Scheduler::new(plan());
    let token = scheduler.enqueue(id("start"), 1).expect("entry token");
    assert_eq!(scheduler.next(), Some(token.clone()));
    let route = scheduler.route(&token, &json!({})).expect("default route");
    assert_eq!(route.transition.id, id("edge-start-done"));
    assert_eq!(
        scheduler.admit_transition(route, 2).expect("admit").node_id,
        id("done")
    );

    let mut context = ContextStore::new(json!({"items": [1, 2]}));
    let items = context.project(1, &["items"]).expect("project array");
    let branches = context.for_each(items).expect("for each");
    assert_eq!(branches.len(), 2);
    let fork = context.fork(1, 2).expect("parallel fork");
    let joined = context
        .join(&fork, JoinStrategy::RequireEqual)
        .expect("explicit equal join");
    assert_eq!(context.get(joined).expect("joined revision").parents, fork);
}

#[test]
fn budgets_routes_attempts_and_uncertain_effects_are_bounded() {
    let mut limits = LimitController::new(Budget {
        turns: 2,
        attempts: 2,
        deadline_tick: 3,
    });
    limits
        .reserve(Reservation {
            turns: 1,
            attempts: 1,
        })
        .expect("first reservation");
    assert!(
        limits
            .reserve(Reservation {
                turns: 2,
                attempts: 1
            })
            .is_err()
    );
    let policy = AttemptPolicy {
        max_retries: 2,
        fallback: Some("fallback".to_owned()),
        requires_approval: false,
    };
    assert_eq!(
        policy.decide(0, EffectOutcome::DefiniteNotStarted),
        AttemptDecision::Retry
    );
    assert_eq!(
        policy.decide(0, EffectOutcome::OutcomeUncertain),
        AttemptDecision::WaitForApproval
    );
    assert_eq!(
        policy.decide(2, EffectOutcome::Failed),
        AttemptDecision::Fallback("fallback".to_owned())
    );
}

#[test]
fn suspension_checkpoint_replacement_and_no_replay_are_fenced() {
    let plan = plan();
    let token = Token {
        id: 7,
        node_id: id("start"),
        context_revision: 1,
    };
    let mut suspension = SuspensionController::new();
    suspension.suspend(Suspension::Input, token.clone());
    assert_eq!(suspension.resume(), Some(token.clone()));
    let checkpoint = Rehydrator::checkpoint(&plan, ProcessGeneration(4), 99, vec![token.clone()]);
    assert_eq!(
        Rehydrator::restore(&plan, &checkpoint, ProcessGeneration(4)).expect("matching recovery"),
        vec![token]
    );
    assert!(Rehydrator::restore(&plan, &checkpoint, ProcessGeneration(5)).is_err());
}

#[test]
fn model_and_subagent_state_remain_explicit_and_isolated() {
    let mut limits = LimitController::new(Budget {
        turns: 1,
        attempts: 1,
        deadline_tick: 2,
    });
    let step = AgentLoop::next_step(&mut limits, id("agent"), json!({"prompt": "fixture"}), 1)
        .expect("bounded broker proposal");
    assert_eq!(step.proposal.kind, "model");
    let mut context = ContextStore::new(json!({"parent": true}));
    let child = SubagentManager::start(
        &mut context,
        &SubagentRequest {
            parent_revision: 1,
            delegated: json!({"task": "child"}),
            depth: 0,
            max_depth: 1,
        },
    )
    .expect("child context");
    let integrated = SubagentManager::integrate(&mut context, 1, json!({"result": child}))
        .expect("declared integration");
    assert_ne!(child, integrated);
    assert!(
        SubagentManager::start(
            &mut context,
            &SubagentRequest {
                parent_revision: 1,
                delegated: json!({}),
                depth: 1,
                max_depth: 1
            }
        )
        .is_err()
    );
}

#[test]
fn route_decision_is_a_frozen_value() {
    let decision = RouteDecision {
        transition: plan().outgoing(&id("start"))[0].clone(),
    };
    assert_eq!(decision.transition.priority, 0);
}
