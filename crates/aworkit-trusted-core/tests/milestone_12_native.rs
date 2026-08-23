use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    MemoryCredentialStore, NativeCredentialStore, NativeCredentialStoreStatusV1,
    RedeemLeaseRequestV1, ScopedLeaseRequestV1, SecretBroker, SecretError,
};

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test id")
}

#[test]
fn native_store_reports_only_explicit_supported_locked_or_unavailable_state() {
    assert!(matches!(
        NativeCredentialStore::new().status(),
        NativeCredentialStoreStatusV1::Available { .. }
            | NativeCredentialStoreStatusV1::Locked
            | NativeCredentialStoreStatusV1::Unavailable
    ));
}

#[test]
fn opaque_creation_update_delete_and_lease_only_retrieval_conform() {
    let mut broker = SecretBroker::with_store(Arc::new(MemoryCredentialStore::default()));
    let metadata = broker
        .create_credential(BTreeMap::from([
            ("token".to_owned(), b"very-secret-token".to_vec()),
            ("account".to_owned(), b"user@example.test".to_vec()),
        ]))
        .expect("create opaque credential");
    assert!(metadata.credential.0.as_str().starts_with("credential."));
    assert!(!format!("{metadata:?}").contains("very-secret-token"));
    assert_eq!(
        broker.describe_credential(&metadata.credential),
        Some(&metadata)
    );

    let lease = ScopedLeaseRequestV1 {
        lease_id: id("lease.m12"),
        credential: metadata.credential.clone(),
        decision_id: id("decision.m12"),
        invocation_id: id("invocation.m12"),
        run_id: id("run.m12"),
        audience_generation: ProcessGeneration(12),
        permitted_fields: BTreeSet::from(["token".to_owned()]),
        ttl: Duration::from_secs(30),
        maximum_uses: 1,
    };
    broker.issue_scoped(lease.clone()).expect("approved lease");
    let delivery = broker
        .redeem_scoped(&RedeemLeaseRequestV1 {
            lease_id: lease.lease_id.clone(),
            decision_id: lease.decision_id.clone(),
            invocation_id: lease.invocation_id.clone(),
            audience_generation: lease.audience_generation,
            requested_fields: lease.permitted_fields.clone(),
        })
        .expect("lease-bound retrieval");
    assert_eq!(
        delivery.field("token"),
        Some(b"very-secret-token".as_slice())
    );
    assert!(delivery.field("account").is_none());

    let updated = broker
        .put_credential(
            metadata.credential.clone(),
            BTreeMap::from([("token".to_owned(), b"replacement".to_vec())]),
        )
        .expect("update credential");
    assert_eq!(updated.revision, 2);
    assert!(matches!(
        broker.redeem_scoped(&RedeemLeaseRequestV1 {
            lease_id: lease.lease_id,
            decision_id: lease.decision_id,
            invocation_id: lease.invocation_id,
            audience_generation: lease.audience_generation,
            requested_fields: lease.permitted_fields,
        }),
        Err(SecretError::Unknown)
    ));

    broker
        .delete_credential(&metadata.credential)
        .expect("delete credential");
    assert!(broker.describe_credential(&metadata.credential).is_none());
}
