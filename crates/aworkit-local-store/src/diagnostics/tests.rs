use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;

fn root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("aworkit-diagnostics-{label}-{nonce}"))
}

fn config(generation: &str) -> DiagnosticLogConfig {
    DiagnosticLogConfig {
        writer_generation: generation.to_owned(),
        queue_capacity: 16,
        max_segment_bytes: 4 * 1024,
        max_segments: 4,
        max_age_ms: 100,
        max_total_bytes: 32 * 1024,
    }
}

fn input(message: impl Into<String>) -> DiagnosticInput {
    let message = message.into();
    let operation = message
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '/' | '-')
            {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    let mut fields = BTreeMap::new();
    fields.insert("operation".to_owned(), operation);
    DiagnosticInput {
        occurred_at_epoch_ms: 1_000,
        monotonic_offset_ns: 10,
        severity: DiagnosticSeverity::Info,
        component: "trusted_core".to_owned(),
        code: "ipc_health".to_owned(),
        correlation: DiagnosticCorrelation {
            chat_id: Some("chat.1".to_owned()),
            ..DiagnosticCorrelation::default()
        },
        fields,
    }
}

fn append_when_queue_available(
    store: &DiagnosticLogStore,
    input: &DiagnosticInput,
    redaction: &RedactionSet,
) -> DiagnosticWriteOutcome {
    for _ in 0..1_000 {
        let outcome = store.try_append(input, redaction);
        if outcome != DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::QueueContended) {
            return outcome;
        }
        thread::yield_now();
    }
    panic!("diagnostic fixture queue remained contended");
}

#[test]
fn queues_only_redacted_allowlisted_records() {
    let root = root("redaction");
    let mut store = DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
    let redaction =
        RedactionSet::new(4, vec!["top-secret".to_owned()], Vec::new()).expect("redaction");
    let mut record = input("connection rejected top-secret");
    record
        .fields
        .insert("operation".to_owned(), "call_top-secret".to_owned());
    assert_eq!(
        append_when_queue_available(&store, &record, &redaction),
        DiagnosticWriteOutcome::Accepted
    );

    let mut forbidden = input("unsafe field");
    forbidden
        .fields
        .insert("authorization".to_owned(), "Bearer top-secret".to_owned());
    assert_eq!(
        store.try_append(&forbidden, &redaction),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::RedactionRejected)
    );
    let mut prompt = input("prompt payload");
    prompt
        .fields
        .insert("prompt_body".to_owned(), "not allowed".to_owned());
    assert_eq!(
        store.try_append(&prompt, &redaction),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::InvalidRecord)
    );

    store.flush_at(1_001).expect("flush");
    let page = store.read_page(None, 32).expect("page");
    let retained = page
        .records
        .iter()
        .find(|record| record.code == "ipc_health")
        .expect("retained record");
    assert_eq!(retained.message, "diagnostic event: ipc_health");
    assert_eq!(retained.fields["operation"], "call_[REDACTED]");
    assert_eq!(retained.redaction_count, 1);
    assert!(
        page.records
            .iter()
            .any(|record| record.code == "records_dropped")
    );

    store.shutdown().expect("shutdown");
    for entry in fs::read_dir(root.join("diagnostics")).expect("diagnostic files") {
        let bytes = fs::read(entry.expect("entry").path()).expect("bytes");
        assert!(!bytes.windows(10).any(|window| window == b"top-secret"));
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn free_form_prompt_and_embedded_credential_text_never_enter_the_queue() {
    let root = root("prompt-safe");
    let mut store = DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
    let mut prompt = input("safe.operation");
    prompt.fields.insert(
        "operation".to_owned(),
        "Explain the entire hidden prompt and reasoning".to_owned(),
    );
    assert_eq!(
        store.try_append(&prompt, &RedactionSet::default()),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::InvalidRecord)
    );
    let mut credential = input("safe.operation");
    credential.fields.insert(
        "operation".to_owned(),
        r#"{"frame":"Authorization: Bearer unknown-token"}"#.to_owned(),
    );
    assert_eq!(
        store.try_append(&credential, &RedactionSet::default()),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::RedactionRejected)
    );
    store.flush_at(1_001).expect("flush");
    let records = store.read_page(None, 16).expect("page").records;
    assert!(!records.is_empty());
    assert!(
        records
            .iter()
            .all(|record| record.code == "records_dropped")
    );
    store.shutdown().expect("shutdown");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn flush_and_shutdown_are_fifo_durability_fences() {
    let root = root("shutdown");
    {
        let mut store =
            DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
        for index in 0..8 {
            let mut record = input(format!("record {index}"));
            record.occurred_at_epoch_ms = 1_000 + index;
            assert_eq!(
                append_when_queue_available(&store, &record, &RedactionSet::default()),
                DiagnosticWriteOutcome::Accepted
            );
        }
        store.flush_at(1_010).expect("flush");
        let records = store.read_page(None, 32).expect("page").records;
        assert_eq!(
            records
                .iter()
                .filter(|record| record.code == "ipc_health")
                .count(),
            8
        );
        let durable_record_count = records.len();
        store.shutdown().expect("shutdown");

        let mut reopened =
            DiagnosticLogStore::open_at(&root, config("writer.2"), 2_000).expect("reopen");
        let page = reopened.read_page(None, 32).expect("recovered page");
        assert_eq!(page.records.len(), durable_record_count);
        assert_eq!(
            page.records
                .iter()
                .filter(|record| record.code == "ipc_health")
                .count(),
            8
        );
        assert!(
            page.records
                .windows(2)
                .all(|pair| pair[0].record_id.sequence + 1 == pair[1].record_id.sequence)
        );
        reopened.shutdown().expect("shutdown");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn close_is_authoritative_under_the_queue_lock() {
    let root = root("shutdown-race");
    let store =
        Arc::new(DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store"));
    let queue = store.shared.queue.lock().expect("queue");
    let (started_sender, started_receiver) = mpsc::channel();
    let caller = Arc::clone(&store);
    let flush = thread::spawn(move || {
        started_sender.send(()).expect("started");
        caller.flush_at(1_001)
    });
    started_receiver.recv().expect("caller started");

    // Model shutdown's ordering while the control caller is waiting for the
    // queue: close first, then acquire the queue to append its terminal fence.
    store.shared.closed.store(true, Ordering::Release);
    drop(queue);
    assert!(matches!(
        flush.join().expect("flush thread"),
        Err(DiagnosticError::Closed)
    ));
    assert_eq!(
        store.try_append(&input("after close"), &RedactionSet::default()),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::StoreClosed)
    );
    assert!(store.shared.queue.lock().expect("queue").items.is_empty());

    let mut store = Arc::try_unwrap(store).unwrap_or_else(|_| panic!("sole store owner"));
    store.shutdown().expect("shutdown");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn one_writer_lock_prevents_live_segment_recovery_by_a_second_store() {
    let root = root("writer-lock");
    let mut first =
        DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("first writer");
    assert!(matches!(
        DiagnosticLogStore::open_at(&root, config("writer.2"), 1_001),
        Err(DiagnosticError::WriterActive)
    ));
    first.shutdown().expect("first shutdown");
    let mut second =
        DiagnosticLogStore::open_at(&root, config("writer.2"), 1_002).expect("second writer");
    second.shutdown().expect("second shutdown");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn worker_exit_closes_and_drains_pending_controls() {
    let root = root("worker-exit");
    let mut store = DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (flush_sender, flush_receiver) = mpsc::channel();
    {
        let mut queue = store.shared.queue.lock().expect("queue");
        queue
            .items
            .push_back(QueueItem::Control(Control::StopForTest {
                entered: entered_sender,
            }));
        queue.items.push_back(QueueItem::Control(Control::Flush {
            now_epoch_ms: 1_001,
            reply: flush_sender,
        }));
    }
    store.shared.wake.notify_one();
    entered_receiver.recv().expect("worker entered stop");
    assert!(matches!(
        flush_receiver.recv().expect("drained response"),
        Err(DiagnosticError::WorkerStopped)
    ));
    assert_eq!(
        store.try_append(&input("after.stop"), &RedactionSet::default()),
        DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::StoreClosed)
    );
    assert!(matches!(
        store.shutdown(),
        Err(DiagnosticError::WorkerStopped)
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rotation_expiry_and_corruption_keep_unavailable_metadata() {
    let root = root("retention");
    let mut retention_config = config("writer.1");
    retention_config.queue_capacity = 64;
    let mut store = DiagnosticLogStore::open_at(&root, retention_config, 1_000).expect("store");
    for index in 0..20 {
        let mut record = input(format!("rotation.{index}"));
        record.occurred_at_epoch_ms = 1_000 + index;
        assert_eq!(
            append_when_queue_available(&store, &record, &RedactionSet::default()),
            DiagnosticWriteOutcome::Accepted
        );
    }
    store.flush_at(1_010).expect("flush");
    let available = store
        .segments()
        .expect("segments")
        .into_iter()
        .find(|segment| segment.state == DiagnosticSegmentState::Available)
        .expect("closed generation");
    fs::write(
        root.join("diagnostics").join(&available.file_name),
        b"corrupt",
    )
    .expect("corrupt");
    let page = store.read_page(None, 32).expect("page degrades");
    assert!(page.unavailable_ranges.iter().any(|range| {
        range.state == DiagnosticSegmentState::Corrupt && range.reason == "integrity_failure"
    }));
    assert!(store.segments().expect("segments").iter().any(|segment| {
        segment.segment_id == available.segment_id
            && segment.state == DiagnosticSegmentState::Corrupt
    }));

    let report = store.enforce_retention_at(1_200).expect("retention");
    assert!(report.expired_segments >= 1);
    let segments = store.segments().expect("segments");
    assert!(segments.iter().any(|segment| {
        segment.state == DiagnosticSegmentState::Expired
            && segment.unavailable_reason.as_deref() == Some("age_retention")
    }));
    let page = store.read_page(None, 32).expect("tombstone page");
    assert!(!page.unavailable_ranges.is_empty());
    store.shutdown().expect("shutdown");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn recovery_reconciles_a_published_closed_segment_before_manifest_commit() {
    let root = root("rotation-crash");
    let mut store = DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
    assert_eq!(
        append_when_queue_available(&store, &input("before.crash"), &RedactionSet::default(),),
        DiagnosticWriteOutcome::Accepted
    );
    store.shutdown().expect("shutdown");

    let manifest_path = root.join("diagnostics").join("manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    let open_name = manifest["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .find(|segment| segment["state"] == "open")
        .and_then(|segment| segment["fileName"].as_str())
        .expect("open name");
    let open_path = root.join("diagnostics").join(open_name);
    let raw = fs::read(&open_path).expect("open bytes");
    let closed_name = open_name.replace(".jsonl", ".rle");
    fs::write(
        root.join("diagnostics").join(closed_name),
        crate::bounded_codec::compress(&raw),
    )
    .expect("published closed bytes");
    fs::remove_file(open_path).expect("crash removed open bytes");

    let mut reopened =
        DiagnosticLogStore::open_at(&root, config("writer.2"), 2_000).expect("reopen");
    assert!(
        reopened
            .read_page(None, 16)
            .expect("recovered page")
            .records
            .iter()
            .any(|record| {
                record
                    .fields
                    .get("operation")
                    .is_some_and(|value| value == "before.crash")
            })
    );
    reopened.shutdown().expect("shutdown");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn corrupt_manifest_file_names_fail_before_any_external_path_is_touched() {
    let root = root("manifest-path");
    let mut store = DiagnosticLogStore::open_at(&root, config("writer.1"), 1_000).expect("store");
    store.shutdown().expect("shutdown");
    let victim = root.parent().expect("parent").join(format!(
        "aworkit-diagnostic-victim-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::write(&victim, b"keep").expect("victim");
    let manifest_path = root.join("diagnostics").join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["segments"][0]["fileName"] =
        serde_json::Value::String(victim.to_string_lossy().into_owned());
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("encode"),
    )
    .expect("tamper");
    assert!(matches!(
        DiagnosticLogStore::open_at(&root, config("writer.2"), 2_000),
        Err(DiagnosticError::CorruptManifest)
    ));
    assert_eq!(fs::read(&victim).expect("victim remains"), b"keep");
    let _ = fs::remove_file(victim);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn diagnostic_configuration_has_absolute_age_and_quota_caps() {
    let root = root("config-caps");
    let mut oversized_age = config("writer.age");
    oversized_age.max_age_ms = 31 * 24 * 60 * 60 * 1_000;
    assert!(matches!(
        DiagnosticLogStore::open_at(&root, oversized_age, 1_000),
        Err(DiagnosticError::InvalidConfig)
    ));
    let mut oversized_total = config("writer.total");
    oversized_total.max_total_bytes = 1024 * 1024 * 1024 + 1;
    assert!(matches!(
        DiagnosticLogStore::open_at(&root, oversized_total, 1_000),
        Err(DiagnosticError::InvalidConfig)
    ));
    let _ = fs::remove_dir_all(root);
}
