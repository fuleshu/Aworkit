//! Noncanonical asynchronous rotating diagnostics.
//!
//! Redaction happens on the caller thread. The bounded queue consequently owns
//! only sanitized records, and deterministic flush/shutdown commands are FIFO
//! fences over every previously accepted record.

mod model;
mod reader;
mod writer;

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::{RedactionSet, maintenance::MaintenanceGate};

pub use model::{
    DiagnosticCorrelation, DiagnosticCursor, DiagnosticDropReason, DiagnosticError,
    DiagnosticHealth, DiagnosticInput, DiagnosticLogConfig, DiagnosticPage, DiagnosticRecord,
    DiagnosticRecordId, DiagnosticRetentionReport, DiagnosticSegmentMetadata,
    DiagnosticSegmentState, DiagnosticSeverity, DiagnosticUnavailableRange, DiagnosticWriteOutcome,
};
use model::{MAX_FIELD_VALUE_BYTES, MAX_FIELDS, MAX_PAGE_SIZE, valid_id, validate_config};
use writer::WriterState;

/// Background writer with a bounded, severity-aware sanitized-record queue.
pub struct DiagnosticLogStore {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    writer_lock: Option<File>,
}

impl DiagnosticLogStore {
    /// Opens a store using the current wall clock for a fresh active segment.
    pub fn for_store_root(
        root: impl AsRef<Path>,
        config: DiagnosticLogConfig,
    ) -> Result<Self, DiagnosticError> {
        Self::open_at(root, config, now_epoch_ms()?)
    }

    /// Deterministic constructor used by startup recovery and tests.
    pub fn open_at(
        root: impl AsRef<Path>,
        config: DiagnosticLogConfig,
        opened_at_epoch_ms: u64,
    ) -> Result<Self, DiagnosticError> {
        validate_config(&config)?;
        fs::create_dir_all(root.as_ref())?;
        let local_root = fs::canonicalize(root.as_ref())?;
        let gate = MaintenanceGate::for_root(&local_root)?;
        let diagnostic_root = local_root.join("diagnostics");
        fs::create_dir_all(&diagnostic_root)?;
        let writer_lock = acquire_writer_lock(&diagnostic_root)?;
        let queue_capacity = config.queue_capacity;
        let state = {
            let _lease = gate.shared()?;
            WriterState::open(diagnostic_root, config, opened_at_epoch_ms)?
        };
        let shared = Arc::new(Shared {
            queue: Mutex::new(QueueState::default()),
            wake: Condvar::new(),
            gate,
            queue_capacity,
            closed: AtomicBool::new(false),
            worker_alive: AtomicBool::new(true),
            accepted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            pending_drop_notice: AtomicU64::new(0),
            write_failures: AtomicU64::new(0),
            corrupt_segments: AtomicU64::new(0),
            last_epoch_ms: AtomicU64::new(opened_at_epoch_ms),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("aworkit-diagnostic-writer".to_owned())
            .spawn(move || worker_loop(&worker_shared, state))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            writer_lock: Some(writer_lock),
        })
    }

    /// Attempts a nonblocking enqueue after synchronous redaction. Raw input is
    /// never cloned or retained by the queue.
    #[must_use]
    pub fn try_append(
        &self,
        input: &DiagnosticInput,
        redaction: &RedactionSet,
    ) -> DiagnosticWriteOutcome {
        // Once close or worker exit is already published, return the stable
        // terminal outcome without racing a now-irrelevant queue lock. The
        // authoritative check remains under the lock below for concurrent
        // shutdown that has not yet become visible here.
        if self.shared.closed.load(Ordering::Acquire)
            || !self.shared.worker_alive.load(Ordering::Acquire)
        {
            return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::StoreClosed);
        }
        self.shared
            .last_epoch_ms
            .fetch_max(input.occurred_at_epoch_ms, Ordering::Relaxed);
        let pending = match sanitize(input, redaction) {
            Ok(pending) => pending,
            Err(SanitizeFailure::Redaction) => {
                self.record_drop();
                return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::RedactionRejected);
            }
            Err(SanitizeFailure::Invalid) => {
                self.record_drop();
                return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::InvalidRecord);
            }
        };
        let mut queue = match self.shared.queue.try_lock() {
            Ok(queue) => queue,
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
                self.record_drop();
                return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::QueueContended);
            }
        };
        // This is the authoritative close check. Shutdown publishes `closed`
        // before taking the same lock and appending its FIFO fence, so no item
        // can be accepted behind that fence.
        if self.shared.closed.load(Ordering::Acquire)
            || !self.shared.worker_alive.load(Ordering::Acquire)
        {
            return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::StoreClosed);
        }

        if queue.items.iter().any(|item| {
            matches!(item, QueueItem::Record(existing)
                if existing.fingerprint == pending.fingerprint
                    && existing.record.severity == pending.record.severity)
        }) {
            self.record_drop();
            return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::Repetitive);
        }
        if queue.record_count >= self.shared.queue_capacity {
            let lowest = queue
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| match item {
                    QueueItem::Record(record) => Some((index, record.record.severity)),
                    QueueItem::Control(_) => None,
                })
                .min_by_key(|(_, severity)| *severity);
            if let Some((index, severity)) = lowest
                && severity < pending.record.severity
            {
                queue.items.remove(index);
                queue.record_count = queue.record_count.saturating_sub(1);
                self.record_drop();
            } else {
                self.record_drop();
                return DiagnosticWriteOutcome::Dropped(DiagnosticDropReason::QueueFull);
            }
        }
        queue.items.push_back(QueueItem::Record(pending));
        queue.record_count = queue.record_count.saturating_add(1);
        self.shared.accepted.fetch_add(1, Ordering::Relaxed);
        drop(queue);
        self.shared.wake.notify_one();
        DiagnosticWriteOutcome::Accepted
    }

    /// FIFO durability fence for every record accepted before this call.
    pub fn flush(&self) -> Result<(), DiagnosticError> {
        self.flush_at(now_epoch_ms()?)
    }

    pub fn flush_at(&self, now_epoch_ms: u64) -> Result<(), DiagnosticError> {
        let (sender, receiver) = mpsc::channel();
        self.enqueue_control(Control::Flush {
            now_epoch_ms,
            reply: sender,
        })?;
        receiver
            .recv()
            .map_err(|_| DiagnosticError::WorkerStopped)?
    }

    /// Reads a verified range after every prior accepted queue item is applied.
    pub fn read_page(
        &self,
        cursor: Option<&DiagnosticCursor>,
        limit: u32,
    ) -> Result<DiagnosticPage, DiagnosticError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(DiagnosticError::InvalidPage);
        }
        let (sender, receiver) = mpsc::channel();
        self.enqueue_control(Control::Read {
            cursor: cursor.cloned(),
            limit,
            reply: sender,
        })?;
        receiver
            .recv()
            .map_err(|_| DiagnosticError::WorkerStopped)?
    }

    pub fn segments(&self) -> Result<Vec<DiagnosticSegmentMetadata>, DiagnosticError> {
        let (sender, receiver) = mpsc::channel();
        self.enqueue_control(Control::Segments { reply: sender })?;
        receiver
            .recv()
            .map_err(|_| DiagnosticError::WorkerStopped)?
    }

    pub fn enforce_retention_at(
        &self,
        now_epoch_ms: u64,
    ) -> Result<DiagnosticRetentionReport, DiagnosticError> {
        let (sender, receiver) = mpsc::channel();
        self.enqueue_control(Control::Retention {
            now_epoch_ms,
            reply: sender,
        })?;
        receiver
            .recv()
            .map_err(|_| DiagnosticError::WorkerStopped)?
    }

    #[must_use]
    pub fn health(&self) -> DiagnosticHealth {
        DiagnosticHealth {
            accepted: self.shared.accepted.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            write_failures: self.shared.write_failures.load(Ordering::Relaxed),
            corrupt_segments: self.shared.corrupt_segments.load(Ordering::Relaxed),
        }
    }

    /// Flushes and joins the worker. Repeating shutdown is harmless.
    pub fn shutdown(&mut self) -> Result<(), DiagnosticError> {
        if self.worker.is_none() {
            return self.release_writer_lock();
        }
        self.shared.closed.store(true, Ordering::Release);
        let (sender, receiver) = mpsc::channel();
        let mut shutdown = Some(Control::Shutdown { reply: sender });
        let wait_for_reply = {
            let mut queue = self
                .shared
                .queue
                .lock()
                .map_err(|_| DiagnosticError::Poisoned)?;
            if self.shared.worker_alive.load(Ordering::Acquire) {
                queue.items.push_back(QueueItem::Control(
                    shutdown.take().ok_or(DiagnosticError::WorkerStopped)?,
                ));
                true
            } else {
                false
            }
        };
        if wait_for_reply {
            self.shared.wake.notify_one();
        }
        drop(shutdown);
        let worker = self.worker.take().ok_or(DiagnosticError::WorkerStopped)?;
        let response = if wait_for_reply {
            receiver.recv().map_err(|_| DiagnosticError::WorkerStopped)
        } else {
            Err(DiagnosticError::WorkerStopped)
        };
        let joined = worker.join().map_err(|_| DiagnosticError::WorkerPanicked);
        let unlocked = self.release_writer_lock();
        joined?;
        unlocked?;
        response?
    }

    fn enqueue_control(&self, control: Control) -> Result<(), DiagnosticError> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| DiagnosticError::Poisoned)?;
        // See `try_append`: the check must occur under the queue lock or a
        // control could otherwise be inserted behind `Shutdown` and wait
        // forever for a worker that already exited.
        if self.shared.closed.load(Ordering::Acquire)
            || !self.shared.worker_alive.load(Ordering::Acquire)
        {
            return Err(DiagnosticError::Closed);
        }
        queue.items.push_back(QueueItem::Control(control));
        drop(queue);
        self.shared.wake.notify_one();
        Ok(())
    }

    fn record_drop(&self) {
        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        self.shared
            .pending_drop_notice
            .fetch_add(1, Ordering::Relaxed);
    }

    fn release_writer_lock(&mut self) -> Result<(), DiagnosticError> {
        if let Some(file) = self.writer_lock.take() {
            FileExt::unlock(&file)?;
        }
        Ok(())
    }
}

impl Drop for DiagnosticLogStore {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Shared {
    queue: Mutex<QueueState>,
    wake: Condvar,
    gate: MaintenanceGate,
    queue_capacity: usize,
    closed: AtomicBool,
    worker_alive: AtomicBool,
    accepted: AtomicU64,
    dropped: AtomicU64,
    pending_drop_notice: AtomicU64,
    write_failures: AtomicU64,
    corrupt_segments: AtomicU64,
    last_epoch_ms: AtomicU64,
}

struct WorkerExitGuard {
    shared: Arc<Shared>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let mut queue = match self.shared.queue.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.shared.worker_alive.store(false, Ordering::Release);
        self.shared.closed.store(true, Ordering::Release);
        while let Some(item) = queue.items.pop_front() {
            match item {
                QueueItem::Record(_) => {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                }
                QueueItem::Control(control) => reply_worker_stopped(control),
            }
        }
        queue.record_count = 0;
        drop(queue);
        self.shared.wake.notify_all();
    }
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<QueueItem>,
    record_count: usize,
}

// Sanitized records stay inline to avoid another allocation on this bounded,
// high-frequency path; the size difference is deliberate.
#[allow(clippy::large_enum_variant)]
enum QueueItem {
    Record(PendingRecord),
    Control(Control),
}

struct PendingRecord {
    record: DiagnosticRecord,
    fingerprint: [u8; 32],
}

#[allow(clippy::large_enum_variant)]
enum Control {
    Flush {
        now_epoch_ms: u64,
        reply: mpsc::Sender<Result<(), DiagnosticError>>,
    },
    Read {
        cursor: Option<DiagnosticCursor>,
        limit: u32,
        reply: mpsc::Sender<Result<DiagnosticPage, DiagnosticError>>,
    },
    Segments {
        reply: mpsc::Sender<Result<Vec<DiagnosticSegmentMetadata>, DiagnosticError>>,
    },
    Retention {
        now_epoch_ms: u64,
        reply: mpsc::Sender<Result<DiagnosticRetentionReport, DiagnosticError>>,
    },
    Shutdown {
        reply: mpsc::Sender<Result<(), DiagnosticError>>,
    },
    #[cfg(test)]
    StopForTest { entered: mpsc::Sender<()> },
}

#[allow(clippy::too_many_lines)]
fn worker_loop(shared: &Arc<Shared>, mut writer: WriterState) {
    let _exit = WorkerExitGuard {
        shared: Arc::clone(&shared),
    };
    loop {
        let item = {
            let Ok(mut queue) = shared.queue.lock() else {
                return;
            };
            while queue.items.is_empty() {
                queue = match shared.wake.wait(queue) {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
            }
            let item = queue.items.pop_front();
            if matches!(item, Some(QueueItem::Record(_))) {
                queue.record_count = queue.record_count.saturating_sub(1);
            }
            item
        };
        if let Some(QueueItem::Record(record)) = item {
            let Ok(_lease) = shared.gate.shared() else {
                shared.write_failures.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            write_drop_notice(
                shared.as_ref(),
                &mut writer,
                record.record.occurred_at_epoch_ms,
            );
            if writer.write(record.record).is_err() {
                shared.write_failures.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        }
        let Some(QueueItem::Control(control)) = item else {
            continue;
        };
        let _lease = match shared.gate.shared() {
            Ok(lease) => lease,
            Err(error) => {
                let is_shutdown = matches!(control, Control::Shutdown { .. });
                reply_gate_error(control, &error);
                if is_shutdown {
                    return;
                }
                continue;
            }
        };
        match control {
            Control::Flush {
                now_epoch_ms,
                reply,
            } => {
                write_drop_notice(shared.as_ref(), &mut writer, now_epoch_ms);
                let _ = reply.send(writer.flush(now_epoch_ms));
            }
            Control::Read {
                cursor,
                limit,
                reply,
            } => {
                write_drop_notice(
                    shared.as_ref(),
                    &mut writer,
                    shared.last_epoch_ms.load(Ordering::Relaxed),
                );
                let result = writer.read_page(cursor.as_ref(), limit);
                if result.as_ref().is_ok_and(|page| {
                    page.unavailable_ranges
                        .iter()
                        .any(|range| range.state == DiagnosticSegmentState::Corrupt)
                }) {
                    shared.corrupt_segments.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
            }
            Control::Segments { reply } => {
                let _ = reply.send(Ok(writer.segments()));
            }
            Control::Retention {
                now_epoch_ms,
                reply,
            } => {
                let _ = reply.send(writer.enforce_retention(now_epoch_ms));
            }
            Control::Shutdown { reply } => {
                write_drop_notice(
                    shared.as_ref(),
                    &mut writer,
                    shared.last_epoch_ms.load(Ordering::Relaxed),
                );
                let result = writer.flush(shared.last_epoch_ms.load(Ordering::Relaxed));
                let _ = reply.send(result);
                return;
            }
            #[cfg(test)]
            Control::StopForTest { entered } => {
                let _ = entered.send(());
                return;
            }
        }
    }
}

fn write_drop_notice(shared: &Shared, writer: &mut WriterState, occurred_at_epoch_ms: u64) {
    let dropped = shared.pending_drop_notice.swap(0, Ordering::AcqRel);
    if dropped == 0 {
        return;
    }
    let mut fields = BTreeMap::new();
    fields.insert("count".to_owned(), dropped.to_string());
    let notice = DiagnosticRecord {
        record_id: DiagnosticRecordId {
            writer_generation: String::new(),
            sequence: 0,
        },
        occurred_at_epoch_ms,
        monotonic_offset_ns: 0,
        severity: DiagnosticSeverity::Warning,
        component: "diagnostic_store".to_owned(),
        code: "records_dropped".to_owned(),
        message: "bounded diagnostic queue dropped records".to_owned(),
        correlation: DiagnosticCorrelation::default(),
        fields,
        redaction_count: 0,
        redaction_generation: 0,
        redaction_set_id: "internal".to_owned(),
    };
    if writer.write(notice).is_err() {
        shared.write_failures.fetch_add(1, Ordering::Relaxed);
    }
}

fn reply_gate_error(control: Control, error: &std::io::Error) {
    let diagnostic = || DiagnosticError::Io(std::io::Error::new(error.kind(), error.to_string()));
    match control {
        Control::Flush { reply, .. } | Control::Shutdown { reply } => {
            let _ = reply.send(Err(diagnostic()));
        }
        Control::Read { reply, .. } => {
            let _ = reply.send(Err(diagnostic()));
        }
        Control::Segments { reply } => {
            let _ = reply.send(Err(diagnostic()));
        }
        Control::Retention { reply, .. } => {
            let _ = reply.send(Err(diagnostic()));
        }
        #[cfg(test)]
        Control::StopForTest { entered } => {
            let _ = entered.send(());
        }
    }
}

fn reply_worker_stopped(control: Control) {
    match control {
        Control::Flush { reply, .. } | Control::Shutdown { reply } => {
            let _ = reply.send(Err(DiagnosticError::WorkerStopped));
        }
        Control::Read { reply, .. } => {
            let _ = reply.send(Err(DiagnosticError::WorkerStopped));
        }
        Control::Segments { reply } => {
            let _ = reply.send(Err(DiagnosticError::WorkerStopped));
        }
        Control::Retention { reply, .. } => {
            let _ = reply.send(Err(DiagnosticError::WorkerStopped));
        }
        #[cfg(test)]
        Control::StopForTest { entered } => {
            let _ = entered.send(());
        }
    }
}

enum SanitizeFailure {
    Redaction,
    Invalid,
}

fn sanitize(
    input: &DiagnosticInput,
    redaction: &RedactionSet,
) -> Result<PendingRecord, SanitizeFailure> {
    if !valid_id(&input.component)
        || !valid_id(&input.code)
        || input.fields.len() > MAX_FIELDS
        || [
            input.correlation.chat_id.as_deref(),
            input.correlation.event_id.as_deref(),
            input.correlation.attempt_id.as_deref(),
            input.correlation.invocation_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_id(value))
    {
        return Err(SanitizeFailure::Invalid);
    }
    // Free-form caller text is deliberately absent from the durable contract:
    // the human-readable message is derived only from the validated event code.
    let message = format!("diagnostic event: {}", input.code);
    let mut redaction_count = 0_u64;
    let mut fields = BTreeMap::new();
    for (name, value) in &input.fields {
        redaction
            .validate_field_name(name)
            .map_err(|_| SanitizeFailure::Redaction)?;
        if !allowed_field(name) || value.len() > MAX_FIELD_VALUE_BYTES || value.contains('\0') {
            return Err(SanitizeFailure::Invalid);
        }
        let value = redaction
            .redact_payload(value.as_bytes())
            .map_err(|_| SanitizeFailure::Redaction)?;
        redaction_count = redaction_count.saturating_add(value.replacements());
        let value =
            String::from_utf8(value.into_bytes()).map_err(|_| SanitizeFailure::Redaction)?;
        if !valid_field_value(name, &value) {
            return Err(SanitizeFailure::Invalid);
        }
        fields.insert(name.clone(), value);
    }
    let mut digest = Sha256::new();
    digest.update(input.severity.as_str());
    digest.update(input.component.as_bytes());
    digest.update(input.code.as_bytes());
    digest.update(message.as_bytes());
    digest.update(redaction.generation().to_le_bytes());
    digest.update(redaction.identity().as_bytes());
    for correlation in [
        input.correlation.chat_id.as_deref(),
        input.correlation.event_id.as_deref(),
        input.correlation.attempt_id.as_deref(),
        input.correlation.invocation_id.as_deref(),
    ] {
        digest.update(correlation.unwrap_or_default().as_bytes());
        digest.update([0]);
    }
    for (name, value) in &fields {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    let fingerprint: [u8; 32] = digest.finalize().into();
    Ok(PendingRecord {
        record: DiagnosticRecord {
            record_id: DiagnosticRecordId {
                writer_generation: String::new(),
                sequence: 0,
            },
            occurred_at_epoch_ms: input.occurred_at_epoch_ms,
            monotonic_offset_ns: input.monotonic_offset_ns,
            severity: input.severity,
            component: input.component.clone(),
            code: input.code.clone(),
            message,
            correlation: input.correlation.clone(),
            fields,
            redaction_count,
            redaction_generation: redaction.generation(),
            redaction_set_id: redaction.identity().to_owned(),
        },
        fingerprint,
    })
}

fn allowed_field(name: &str) -> bool {
    matches!(
        name,
        "operation"
            | "phase"
            | "status"
            | "error_code"
            | "path_kind"
            | "count"
            | "duration_ms"
            | "queue_depth"
            | "migration"
            | "ipc_method"
            | "process_kind"
    )
}

fn valid_field_value(name: &str, value: &str) -> bool {
    match name {
        "count" | "duration_ms" | "queue_depth" => value.parse::<u64>().is_ok(),
        _ => {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-' | b'[' | b']')
                })
        }
    }
}

fn now_epoch_ms() -> Result<u64, DiagnosticError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DiagnosticError::InvalidRecord)?
            .as_millis(),
    )
    .map_err(|_| DiagnosticError::NumericOverflow)
}

fn acquire_writer_lock(root: &Path) -> Result<File, DiagnosticError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(root.join(".writer.lock"))?;
    // Lock contention surfaces as `WouldBlock` on Unix and as the raw
    // ERROR_LOCK_VIOLATION code on Windows; compare against the canonical
    // contended error identity for both platforms.
    let contended = fs2::lock_contended_error();
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(error)
            if error.kind() == contended.kind()
                && error.raw_os_error() == contended.raw_os_error() =>
        {
            Err(DiagnosticError::WriterActive)
        }
        Err(error) => Err(error.into()),
    }
}
