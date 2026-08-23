use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    ApprovalRequirement, ApprovalResponseV1, ApprovedDispatchV1, ApprovedHostDispatchPortV1,
    AuthorityManifest, BrokerDecisionV1, BrokerError, CapabilityBinding,
    CommittedWorkerResultPortV1, DeliveryAcceptanceV1, DurableInvocationBroker,
    InvocationLeasePortV1, InvocationLedgerEventV1, InvocationLedgerPortV1, MemoryInvocationLedger,
    RedeemLeaseRequestV1, ScopedLeaseRequestV1, SecretBroker, SecretError, SecretLeaseAuditKindV1,
    WorkerInvocationProposalV1, WorkerResultOutboxV1,
};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn fields(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn scoped_secret_leases_enforce_every_audience_ttl_field_use_and_revocation_fence() {
    let mut broker = SecretBroker::with_store(Arc::new(
        aworkit_trusted_core::MemoryCredentialStore::default(),
    ));
    let credential = aworkit_trusted_core::CredentialRef(id("credential.model"));
    let metadata = broker
        .put_credential(
            credential.clone(),
            BTreeMap::from([
                ("api_key".into(), b"top-secret".to_vec()),
                ("tenant".into(), b"acme".to_vec()),
            ]),
        )
        .expect("credential");
    assert_eq!(metadata.field_names, fields(&["api_key", "tenant"]));
    assert_eq!(metadata.revision, 1);

    let lease_id = id("lease.scoped.1");
    let decision_id = id("decision.1");
    let invocation_id = id("invocation.1");
    let run_id = id("run.1");
    broker
        .issue_scoped(ScopedLeaseRequestV1 {
            lease_id: lease_id.clone(),
            credential: credential.clone(),
            decision_id: decision_id.clone(),
            invocation_id: invocation_id.clone(),
            run_id: run_id.clone(),
            audience_generation: ProcessGeneration(3),
            permitted_fields: fields(&["api_key"]),
            ttl: Duration::from_secs(30),
            maximum_uses: 1,
        })
        .expect("lease");

    let request =
        |generation, decision: StableId, invocation: StableId, selected| RedeemLeaseRequestV1 {
            lease_id: lease_id.clone(),
            decision_id: decision,
            invocation_id: invocation,
            audience_generation: ProcessGeneration(generation),
            requested_fields: selected,
        };
    assert_eq!(
        broker
            .redeem_scoped(&request(
                4,
                decision_id.clone(),
                invocation_id.clone(),
                fields(&["api_key"]),
            ))
            .err(),
        Some(SecretError::Audience)
    );
    assert_eq!(
        broker
            .redeem_scoped(&request(
                3,
                id("decision.other"),
                invocation_id.clone(),
                fields(&["api_key"]),
            ))
            .err(),
        Some(SecretError::InvocationMismatch)
    );
    assert_eq!(
        broker
            .redeem_scoped(&request(
                3,
                decision_id.clone(),
                invocation_id.clone(),
                fields(&["tenant"]),
            ))
            .err(),
        Some(SecretError::FieldDenied)
    );
    let delivery = broker
        .redeem_scoped(&request(
            3,
            decision_id.clone(),
            invocation_id.clone(),
            fields(&["api_key"]),
        ))
        .expect("redeem");
    assert_eq!(delivery.field("api_key"), Some(b"top-secret".as_slice()));
    assert_eq!(
        broker
            .redeem_scoped(&request(
                3,
                decision_id,
                invocation_id,
                fields(&["api_key"]),
            ))
            .err(),
        Some(SecretError::Used)
    );

    let expiring = id("lease.expiring");
    broker
        .issue_scoped(ScopedLeaseRequestV1 {
            lease_id: expiring.clone(),
            credential: credential.clone(),
            decision_id: id("decision.expiring"),
            invocation_id: id("invocation.expiring"),
            run_id: run_id.clone(),
            audience_generation: ProcessGeneration(3),
            permitted_fields: fields(&["api_key"]),
            ttl: Duration::from_millis(1),
            maximum_uses: 1,
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(3));
    assert_eq!(
        broker
            .redeem_scoped(&RedeemLeaseRequestV1 {
                lease_id: expiring,
                decision_id: id("decision.expiring"),
                invocation_id: id("invocation.expiring"),
                audience_generation: ProcessGeneration(3),
                requested_fields: fields(&["api_key"]),
            })
            .err(),
        Some(SecretError::Expired)
    );
    assert_eq!(
        broker
            .issue_scoped(ScopedLeaseRequestV1 {
                lease_id: id("lease.too-long"),
                credential: credential.clone(),
                decision_id: id("decision.long"),
                invocation_id: id("invocation.long"),
                run_id: run_id.clone(),
                audience_generation: ProcessGeneration(3),
                permitted_fields: fields(&["api_key"]),
                ttl: Duration::from_secs(16 * 60),
                maximum_uses: 1,
            })
            .err(),
        Some(SecretError::Expired)
    );

    let revoked = id("lease.revoked.by.replace");
    broker
        .issue_scoped(ScopedLeaseRequestV1 {
            lease_id: revoked.clone(),
            credential: credential.clone(),
            decision_id: id("decision.replace"),
            invocation_id: id("invocation.replace"),
            run_id,
            audience_generation: ProcessGeneration(3),
            permitted_fields: fields(&["api_key"]),
            ttl: Duration::from_secs(30),
            maximum_uses: 1,
        })
        .unwrap();
    assert_eq!(
        broker
            .put_credential(
                credential,
                BTreeMap::from([("api_key".into(), b"replacement".to_vec())]),
            )
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        broker
            .redeem_scoped(&RedeemLeaseRequestV1 {
                lease_id: revoked.clone(),
                decision_id: id("decision.replace"),
                invocation_id: id("invocation.replace"),
                audience_generation: ProcessGeneration(3),
                requested_fields: fields(&["api_key"]),
            })
            .err(),
        Some(SecretError::Unknown)
    );
    assert!(broker.audit().iter().any(|entry| {
        entry.lease_id == revoked && entry.kind == SecretLeaseAuditKindV1::Revoked
    }));
}

fn manifest(approval: ApprovalRequirement, suffix: &str) -> AuthorityManifest {
    AuthorityManifest {
        manifest_id: id(&format!("manifest.{suffix}")),
        capability_bindings: vec![CapabilityBinding {
            capability_id: id("capability.shell"),
            adapter_version: "1".into(),
            enabled: true,
            compatible: true,
            approval,
        }],
        summary: "frozen".into(),
    }
}

fn proposal(suffix: &str) -> WorkerInvocationProposalV1 {
    WorkerInvocationProposalV1 {
        proposal_id: id(&format!("proposal.{suffix}")),
        run_id: id("run.1"),
        node_id: id("node.1"),
        attempt: 1,
        capability_id: id("capability.shell"),
        payload_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

#[derive(Clone)]
struct LeasePort {
    issued: Arc<Mutex<Vec<StableId>>>,
    revoked: Arc<Mutex<Vec<StableId>>>,
}

impl InvocationLeasePortV1 for LeasePort {
    fn issue_for_dispatch(
        &self,
        _proposal: &WorkerInvocationProposalV1,
        _manifest: &AuthorityManifest,
        _invocation_id: &StableId,
    ) -> Result<Vec<StableId>, BrokerError> {
        let lease = id("lease.dispatch.1");
        self.issued.lock().unwrap().push(lease.clone());
        Ok(vec![lease])
    }

    fn revoke_uncommitted(&self, lease_ids: &[StableId]) -> Result<(), BrokerError> {
        self.revoked.lock().unwrap().extend_from_slice(lease_ids);
        Ok(())
    }
}

#[test]
fn approval_and_lease_are_frozen_idempotent_and_rolled_back_on_failed_atomic_dispatch() {
    let ledger = MemoryInvocationLedger::default();
    let leases = LeasePort {
        issued: Arc::new(Mutex::new(Vec::new())),
        revoked: Arc::new(Mutex::new(Vec::new())),
    };
    let broker = DurableInvocationBroker::new(Arc::new(ledger.clone()), 50)
        .with_lease_port(Arc::new(leases.clone()));
    let authority = manifest(ApprovalRequirement::PerInvocation, "approval");
    let proposed = proposal("approval");
    let challenge = match broker.propose(&authority, proposed.clone(), 100).unwrap() {
        BrokerDecisionV1::AwaitingApproval(challenge) => challenge,
        other => panic!("unexpected decision: {other:?}"),
    };
    assert_eq!(
        broker.propose(&authority, proposed, 120).unwrap(),
        BrokerDecisionV1::AwaitingApproval(challenge.clone())
    );

    ledger.fail_next_write();
    let response = ApprovalResponseV1 {
        invocation_id: challenge.invocation_id.clone(),
        nonce: challenge.nonce.clone(),
        approved: true,
        now_epoch_millis: 120,
    };
    assert_eq!(
        broker.resolve_approval(&authority, &response).err(),
        Some(BrokerError::CommitFailed)
    );
    assert_eq!(
        leases.revoked.lock().unwrap().as_slice(),
        &[id("lease.dispatch.1")]
    );
    let dispatch = match broker.resolve_approval(&authority, &response).unwrap() {
        BrokerDecisionV1::DispatchReady(dispatch) => dispatch,
        other => panic!("unexpected decision: {other:?}"),
    };
    assert_eq!(dispatch.lease_ids, vec![id("lease.dispatch.1")]);
    let authorized = ledger.events(&dispatch.invocation_id).unwrap();
    assert!(authorized.iter().any(|event| {
        matches!(event, InvocationLedgerEventV1::Authorized(value) if value == &dispatch)
    }));

    let expired_ledger = MemoryInvocationLedger::default();
    let expired_broker = DurableInvocationBroker::new(Arc::new(expired_ledger), 10);
    let expired_authority = manifest(ApprovalRequirement::PerInvocation, "expired");
    let challenge = match expired_broker
        .propose(&expired_authority, proposal("expired"), 1_000)
        .unwrap()
    {
        BrokerDecisionV1::AwaitingApproval(challenge) => challenge,
        other => panic!("unexpected decision: {other:?}"),
    };
    assert_eq!(
        expired_broker
            .resolve_approval(
                &expired_authority,
                &ApprovalResponseV1 {
                    invocation_id: challenge.invocation_id,
                    nonce: challenge.nonce,
                    approved: true,
                    now_epoch_millis: 1_010,
                },
            )
            .unwrap(),
        BrokerDecisionV1::Denied
    );
}

#[derive(Clone)]
struct Host {
    responses: Arc<Mutex<VecDeque<DeliveryAcceptanceV1>>>,
    calls: Arc<Mutex<Vec<ApprovedDispatchV1>>>,
}

impl ApprovedHostDispatchPortV1 for Host {
    fn dispatch(&self, dispatch: &ApprovedDispatchV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        self.calls.lock().unwrap().push(dispatch.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(BrokerError::Unavailable)
    }
}

#[derive(Clone)]
struct Worker {
    responses: Arc<Mutex<VecDeque<DeliveryAcceptanceV1>>>,
    calls: Arc<Mutex<Vec<WorkerResultOutboxV1>>>,
}

impl CommittedWorkerResultPortV1 for Worker {
    fn deliver(&self, result: &WorkerResultOutboxV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        self.calls.lock().unwrap().push(result.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(BrokerError::Unavailable)
    }
}

fn ready(
    broker: &DurableInvocationBroker,
    authority: &AuthorityManifest,
    suffix: &str,
) -> ApprovedDispatchV1 {
    match broker.propose(authority, proposal(suffix), 100).unwrap() {
        BrokerDecisionV1::DispatchReady(dispatch) => dispatch,
        other => panic!("unexpected decision: {other:?}"),
    }
}

#[test]
fn dispatch_progress_settlement_and_worker_delivery_follow_durable_order() {
    let ledger = MemoryInvocationLedger::default();
    let broker = DurableInvocationBroker::new(Arc::new(ledger.clone()), 100);
    let authority = manifest(ApprovalRequirement::Never, "ordered");
    let dispatch = ready(&broker, &authority, "ordered");
    assert!(ledger.pending_worker_results().unwrap().is_empty());
    let host = Host {
        responses: Arc::new(Mutex::new(VecDeque::from([DeliveryAcceptanceV1::Accepted]))),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(broker.deliver_dispatches(&host).unwrap(), 1);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert_eq!(
        broker
            .commit_progress(&dispatch.invocation_id, 2, "sha256:two".into())
            .err(),
        Some(BrokerError::ProgressSequence)
    );
    broker
        .commit_progress(&dispatch.invocation_id, 1, "sha256:one".into())
        .unwrap();
    broker
        .commit_progress(&dispatch.invocation_id, 1, "sha256:one".into())
        .unwrap();
    broker
        .settle(&dispatch.invocation_id, "sha256:outcome".into(), false)
        .unwrap();
    let pending = ledger.pending_worker_results().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].uncertain);
    let worker = Worker {
        responses: Arc::new(Mutex::new(VecDeque::from([DeliveryAcceptanceV1::Accepted]))),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(broker.deliver_worker_results(&worker).unwrap(), 1);
    assert_eq!(worker.calls.lock().unwrap().as_slice(), pending.as_slice());
    assert!(ledger.pending_worker_results().unwrap().is_empty());
}

#[test]
fn ambiguous_or_crashed_host_dispatch_is_committed_uncertain_and_never_replayed() {
    let ledger = MemoryInvocationLedger::default();
    let broker = DurableInvocationBroker::new(Arc::new(ledger.clone()), 100);
    let authority = manifest(ApprovalRequirement::Never, "ambiguous");
    let dispatch = ready(&broker, &authority, "ambiguous");
    let host = Host {
        responses: Arc::new(Mutex::new(VecDeque::from([
            DeliveryAcceptanceV1::Ambiguous,
            DeliveryAcceptanceV1::Accepted,
        ]))),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(
        broker.deliver_dispatches(&host).err(),
        Some(BrokerError::AmbiguousDispatch)
    );
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert!(ledger.pending_dispatches().unwrap().is_empty());
    assert_eq!(broker.deliver_dispatches(&host).unwrap(), 0);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    let result = ledger.pending_worker_results().unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].uncertain);
    assert_eq!(
        broker
            .commit_progress(&dispatch.invocation_id, 1, "sha256:late".into())
            .err(),
        Some(BrokerError::NotAccepted)
    );

    let crash_ledger = MemoryInvocationLedger::default();
    let crash_broker = DurableInvocationBroker::new(Arc::new(crash_ledger.clone()), 100);
    let crash_authority = manifest(ApprovalRequirement::Never, "crash");
    let crash_dispatch = ready(&crash_broker, &crash_authority, "crash");
    crash_ledger
        .append_atomic(
            &[InvocationLedgerEventV1::DispatchAttempted {
                invocation_id: crash_dispatch.invocation_id,
            }],
            None,
        )
        .unwrap();
    let never_called = Host {
        responses: Arc::new(Mutex::new(VecDeque::from([DeliveryAcceptanceV1::Accepted]))),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(
        crash_broker.deliver_dispatches(&never_called).err(),
        Some(BrokerError::AmbiguousDispatch)
    );
    assert!(never_called.calls.lock().unwrap().is_empty());
    assert!(crash_ledger.pending_dispatches().unwrap().is_empty());
    assert!(crash_ledger.pending_worker_results().unwrap()[0].uncertain);
}
