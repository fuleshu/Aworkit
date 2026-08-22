use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::RedactionSet;

use super::*;

fn root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("aworkit-capture-{label}-{nonce}"))
}

fn policy() -> CapturePolicy {
    CapturePolicy {
        enabled: true,
        generation: 3,
        max_capture_bytes: 1_024,
        max_chunk_bytes: 512,
        max_chunks: 8,
        global_quota_bytes: 4_096,
        ttl_ms: 100,
        expired_tombstone_ms: 50,
        quota_class: "test".into(),
    }
}

fn request(id: &str) -> CaptureRequest {
    CaptureRequest {
        capture_id: id.into(),
        source: CaptureSource::Provider,
        correlation: CaptureCorrelation {
            chat_id: Some("chat.1".into()),
            invocation_id: Some("invoke.1".into()),
            ..CaptureCorrelation::default()
        },
        created_at_epoch_ms: 1_000,
    }
}

fn redaction() -> RedactionSet {
    RedactionSet::new(3, vec!["top-secret".into()], Vec::new()).expect("redaction")
}

#[test]
fn disabled_policy_creates_no_manifest() {
    let root = root("disabled");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    assert!(
        store
            .begin(
                &request("capture.1"),
                &CapturePolicy::default(),
                &RedactionSet::default(),
            )
            .expect("begin")
            .is_none()
    );
    assert!(matches!(
        store.manifest("capture.1"),
        Err(CaptureError::UnknownCapture)
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn path_component_capture_ids_are_rejected_before_filesystem_use() {
    let root = root("path-id");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    for capture_id in [".", "..", ".hidden", "trailing."] {
        assert!(matches!(
            store.begin(&request(capture_id), &policy(), &redaction()),
            Err(CaptureError::InvalidId)
        ));
    }
    assert!(root.join("debug-captures").is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn policy_rejects_values_above_absolute_capture_bounds() {
    use super::common::{
        HARD_MAX_CAPTURE_BYTES, HARD_MAX_CHUNK_BYTES, HARD_MAX_CHUNKS, HARD_MAX_GLOBAL_QUOTA_BYTES,
    };

    let root = root("hard-policy-bounds");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let mut cases = Vec::new();
    let mut oversized = policy();
    oversized.max_capture_bytes = HARD_MAX_CAPTURE_BYTES + 1;
    cases.push(oversized);
    let mut oversized = policy();
    oversized.max_chunk_bytes = HARD_MAX_CHUNK_BYTES + 1;
    oversized.max_capture_bytes = oversized.max_chunk_bytes;
    cases.push(oversized);
    let mut oversized = policy();
    oversized.max_chunks = HARD_MAX_CHUNKS + 1;
    cases.push(oversized);
    let mut oversized = policy();
    oversized.global_quota_bytes = HARD_MAX_GLOBAL_QUOTA_BYTES + 1;
    cases.push(oversized);

    for (index, oversized) in cases.into_iter().enumerate() {
        assert!(matches!(
            store.begin(
                &request(&format!("capture.bound.{index}")),
                &oversized,
                &redaction(),
            ),
            Err(CaptureError::InvalidPolicy)
        ));
    }
    assert!(
        store
            .list_manifests(None, 10)
            .expect("manifests")
            .is_empty()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stores_only_redacted_compressed_bytes_and_reads_with_a_lease() {
    let root = root("redacted");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    store
        .begin(&request("capture.1"), &policy(), &redaction)
        .expect("begin");
    let outcome = store
        .append(
            &CaptureFrame {
                capture_id: "capture.1",
                received_at_epoch_ms: 1_001,
                payload: br#"{"message":"top-secret top-secret"}"#,
            },
            &redaction,
        )
        .expect("append");
    assert!(matches!(outcome, CaptureAppendOutcome::Appended(_)));
    let manifest = store.seal("capture.1", 1_002).expect("seal");
    assert_eq!(manifest.state, CaptureState::Available);
    assert_eq!(manifest.redaction_count, 2);

    let persisted = fs::read(store.chunk_path("capture.1", 0)).expect("chunk");
    assert!(!persisted.windows(10).any(|window| window == b"top-secret"));
    let reader = store
        .acquire_reader("capture.1", 1_003, 50)
        .expect("reader");
    let page = reader.read_page(0, 10).expect("page");
    assert_eq!(page.chunks.len(), 1);
    assert_eq!(
        std::str::from_utf8(&page.chunks[0].payload).expect("utf8"),
        r#"{"message":"[REDACTED] [REDACTED]"}"#
    );
    drop(reader);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn caller_supplied_marker_does_not_inflate_redaction_metadata() {
    let root = root("redaction-count");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    store
        .begin(&request("capture.marker"), &policy(), &redaction)
        .expect("begin");
    let outcome = store
        .append(
            &CaptureFrame {
                capture_id: "capture.marker",
                received_at_epoch_ms: 1_001,
                payload: b"literal [REDACTED] top-secret",
            },
            &redaction,
        )
        .expect("append");
    let CaptureAppendOutcome::Appended(chunk) = outcome else {
        panic!("expected append")
    };
    assert_eq!(chunk.redaction_count, 1);
    assert_eq!(
        store
            .seal("capture.marker", 1_002)
            .expect("seal")
            .redaction_count,
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn same_generation_with_a_different_redaction_set_cannot_mutate_capture() {
    let root = root("redaction-identity");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let original = redaction();
    store
        .begin(&request("capture.identity"), &policy(), &original)
        .expect("begin");
    let weaker = RedactionSet::new(3, Vec::new(), Vec::new()).expect("weaker set");
    assert!(matches!(
        store.append(
            &CaptureFrame {
                capture_id: "capture.identity",
                received_at_epoch_ms: 1_001,
                payload: br#"{"authorization":"top-secret"}"#,
            },
            &weaker,
        ),
        Err(CaptureError::RedactionIdentityMismatch)
    ));
    let manifest = store.manifest("capture.identity").expect("manifest");
    assert_eq!(manifest.state, CaptureState::Recording);
    assert_eq!(manifest.chunk_count, 0);
    assert!(!manifest.truncated);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn interrupted_recovery_waits_for_live_capture_writers() {
    let root = root("exclusive-recovery");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    store
        .begin(&request("capture.recovery"), &policy(), &redaction)
        .expect("begin");
    let live_writer = store.gate.shared().expect("live writer lease");
    let (sender, receiver) = mpsc::channel();
    let recovery = store.clone();
    let handle = thread::spawn(move || {
        sender
            .send(recovery.recover_interrupted(1_010))
            .expect("recovery result");
    });
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(live_writer);
    assert_eq!(receiver.recv().expect("recovery").expect("success"), 1);
    handle.join().expect("recovery thread");
    assert_eq!(
        store.manifest("capture.recovery").expect("manifest").state,
        CaptureState::Available
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn expired_bytes_still_count_against_physical_quota() {
    let root = root("physical-quota");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    let mut bounded = policy();
    bounded.max_chunk_bytes = 100;
    bounded.global_quota_bytes = 120;
    let first_payload = (0_u8..80)
        .map(|value| b'!' + (value % 90))
        .collect::<Vec<_>>();
    store
        .begin(&request("capture.physical.1"), &bounded, &redaction)
        .expect("begin first");
    assert!(matches!(
        store
            .append(
                &CaptureFrame {
                    capture_id: "capture.physical.1",
                    received_at_epoch_ms: 1_001,
                    payload: &first_payload,
                },
                &redaction,
            )
            .expect("append first"),
        CaptureAppendOutcome::Appended(_)
    ));
    store.seal("capture.physical.1", 1_002).expect("seal");
    store.enforce_retention(1_101).expect("expire first");

    let mut second = request("capture.physical.2");
    second.created_at_epoch_ms = 1_102;
    store
        .begin(&second, &bounded, &redaction)
        .expect("begin second");
    assert!(matches!(
        store
            .append(
                &CaptureFrame {
                    capture_id: "capture.physical.2",
                    received_at_epoch_ms: 1_103,
                    payload: &first_payload,
                },
                &redaction,
            )
            .expect("append second"),
        CaptureAppendOutcome::Truncated(_)
    ));
    assert_eq!(
        store
            .manifest("capture.physical.2")
            .expect("second manifest")
            .truncation_reason
            .as_deref(),
        Some("global_capture_quota")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn forbidden_secret_field_seals_an_empty_truncated_capture() {
    let root = root("forbidden-field");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    store
        .begin(&request("capture.2"), &policy(), &redaction)
        .expect("begin");
    let outcome = store
        .append(
            &CaptureFrame {
                capture_id: "capture.2",
                received_at_epoch_ms: 1_001,
                payload: br#"{"authorization":"top-secret"}"#,
            },
            &redaction,
        )
        .expect("append");
    let CaptureAppendOutcome::Truncated(manifest) = outcome else {
        panic!("expected truncation")
    };
    assert!(manifest.truncated);
    assert_eq!(manifest.redaction_omissions, 1);
    assert_eq!(manifest.chunk_count, 0);
    assert_eq!(
        manifest.truncation_reason.as_deref(),
        Some("forbidden_secret_field")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn quota_truncates_without_persisting_the_rejected_frame() {
    let root = root("quota");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    let mut bounded = policy();
    bounded.max_capture_bytes = 5;
    bounded.max_chunk_bytes = 5;
    store
        .begin(&request("capture.3"), &bounded, &redaction)
        .expect("begin");
    let outcome = store
        .append(
            &CaptureFrame {
                capture_id: "capture.3",
                received_at_epoch_ms: 1_001,
                payload: b"six!!!",
            },
            &redaction,
        )
        .expect("append");
    let CaptureAppendOutcome::Truncated(manifest) = outcome else {
        panic!("expected truncation")
    };
    assert_eq!(manifest.chunk_count, 0);
    assert_eq!(manifest.byte_count, 0);
    assert_eq!(manifest.state, CaptureState::Available);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn expiry_and_corruption_leave_queryable_tombstones() {
    let root = root("tombstones");
    let store = DebugCaptureStore::for_store_root(&root).expect("store");
    let redaction = redaction();
    store
        .begin(&request("capture.expired"), &policy(), &redaction)
        .expect("begin");
    store
        .append(
            &CaptureFrame {
                capture_id: "capture.expired",
                received_at_epoch_ms: 1_001,
                payload: b"safe",
            },
            &redaction,
        )
        .expect("append");
    store.seal("capture.expired", 1_002).expect("seal");
    let reader = store
        .acquire_reader("capture.expired", 1_050, 200)
        .expect("reader");
    let report = store.enforce_retention(1_101).expect("expire");
    assert_eq!(report.expired, 1);
    assert_eq!(
        store.manifest("capture.expired").expect("manifest").state,
        CaptureState::Expired
    );
    assert_eq!(store.enforce_retention(1_152).expect("leased").purged, 0);
    drop(reader);
    assert_eq!(store.enforce_retention(1_152).expect("purge").purged, 1);
    assert_eq!(
        store.manifest("capture.expired").expect("manifest").state,
        CaptureState::Purged
    );

    let mut later = request("capture.corrupt");
    later.created_at_epoch_ms = 2_000;
    store.begin(&later, &policy(), &redaction).expect("begin");
    store
        .append(
            &CaptureFrame {
                capture_id: "capture.corrupt",
                received_at_epoch_ms: 2_001,
                payload: b"safe",
            },
            &redaction,
        )
        .expect("append");
    store.seal("capture.corrupt", 2_002).expect("seal");
    fs::write(store.chunk_path("capture.corrupt", 0), b"broken").expect("corrupt");
    assert_eq!(
        store.verify("capture.corrupt").expect("verify").state,
        CaptureState::CorruptDiscarded
    );
    assert!(!store.capture_directory("capture.corrupt").exists());
    store.enforce_retention(2_003).expect("purge corrupt");
    assert_eq!(
        store.manifest("capture.corrupt").expect("manifest").state,
        CaptureState::Purged
    );

    let mut stale = request("capture.stale");
    stale.created_at_epoch_ms = 3_000;
    store
        .begin(&stale, &policy(), &redaction)
        .expect("begin stale");
    store.enforce_retention(3_101).expect("expire stale");
    let stale = store.manifest("capture.stale").expect("stale manifest");
    assert_eq!(stale.state, CaptureState::Expired);
    assert!(stale.truncated);
    assert_eq!(
        stale.truncation_reason.as_deref(),
        Some("stale_recording_ttl")
    );
    store.enforce_retention(3_152).expect("purge stale");
    assert_eq!(
        store.manifest("capture.stale").expect("manifest").state,
        CaptureState::Purged
    );
    let _ = fs::remove_dir_all(root);
}
