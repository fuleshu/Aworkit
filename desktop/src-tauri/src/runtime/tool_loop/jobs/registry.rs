//! Durable job identities and cursors, with bounded in-memory native runners.
//!
//! A job is owned by one Chat. Its runner is either an OS process session
//! (shell/Python) or an in-process delegated child agent run. Both kinds share
//! the same persisted identity, ownership scope, running/terminal snapshot,
//! captured output cursor, keep/stop/input control surface and completion
//! barrier, so the model observes and controls them through the same tools.
use aworkit_capability_host::{
    CancellationToken, ProcessOutput, ProcessOutputCursor, ProcessSession, ProcessSnapshot,
    ProcessSpecV1,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Which runner executes a job. Legacy records decode as `Process`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[default]
    Process,
    Subagent,
}

/// What the tool layer needs to route a control that is child-specific.
#[derive(Clone, Debug)]
pub struct ChildJobInfo {
    pub child_id: String,
    pub running: bool,
}

/// Captured byte counts recorded when an Agent discarded a job's output.
///
/// The record keeps the sizes so `job_list` and Run details can still explain
/// what the job produced after the bytes themselves are gone.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardedOutput {
    /// Captured stdout bytes removed from the job directory.
    pub stdout_bytes: u64,
    /// Captured stderr bytes removed from the job directory.
    pub stderr_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: String,
    owner: String,
    invocation: String,
    /// The Agent/child invocation that launched this job; old jobs remain root-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default)]
    kind: JobKind,
    command: Vec<String>,
    snapshot: ProcessSnapshot,
    cursor: ProcessOutputCursor,
    collected: bool,
    kept: Option<String>,
    interrupted: bool,
    /// The Agent declared this job resolved without needing to read its output.
    #[serde(default)]
    released: bool,
    /// The note the caller gave when releasing, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_reason: Option<String>,
    /// Byte counts captured when the output was discarded on release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    discarded: Option<DiscardedOutput>,
    /// Terminal child outcome for a delegated child job; absent for processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child: Option<Value>,
}
struct Entry {
    record: Record,
    runner: Option<Runner>,
    cancellation: CancellationToken,
}
#[derive(Clone)]
enum Runner {
    Process(Arc<ProcessSession>),
    Child(Arc<ChildJobHandle>),
}
struct State {
    connection: Connection,
    entries: BTreeMap<String, Entry>,
}

/// In-memory state of one delegated child run registered as a job.
#[derive(Default)]
struct ChildJobState {
    running: bool,
    error: Option<String>,
    /// Terminal child outcome once the run settles.
    child: Option<Value>,
    /// Latest live progress snapshot while the run is still working.
    progress: Option<Value>,
    /// Steering messages queued for the child's next step boundary.
    steering: Vec<String>,
    started: Option<Instant>,
    /// The last committed step the child reported, and when it reported it.
    /// A child that stops producing events is a stall, not progress.
    last_sequence: u64,
    last_activity: Option<Instant>,
    /// Live counters, so a cancellation reports the work that really happened
    /// instead of the zeros of the frame the child started from.
    model_turns: u32,
    input_tokens: u64,
    output_tokens: u64,
}

/// How one live delegated child is spending its time and budget.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChildLiveV1 {
    pub running: bool,
    pub last_sequence: u64,
    pub age_seconds: u64,
    pub model_turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Shared handle of one in-process child run. The registry owns the durable
/// record; this handle owns the live run, its cancellation and its steering.
pub struct ChildJobHandle {
    cancellation: CancellationToken,
    directory: PathBuf,
    state: Mutex<ChildJobState>,
}

impl ChildJobHandle {
    /// Cancels the child run. The child loop aborts cooperatively.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// True while the child loop is still running.
    pub fn is_running(&self) -> bool {
        self.state.lock().map(|state| state.running).unwrap_or(false)
    }

    /// Records one live progress line and the child's latest counters.
    pub fn progress(&self, line: &str, child: Value) {
        if let Ok(mut state) = self.state.lock() {
            state.progress = Some(child);
            state.last_activity = Some(Instant::now());
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("stdout.log"))
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Records one committed child step. The sequence is what a stall report
    /// names, so it must advance only when the child really produced a turn.
    pub fn record_turn(&self, sequence: u32) {
        if let Ok(mut state) = self.state.lock() {
            state.last_sequence = state.last_sequence.max(u64::from(sequence));
            state.model_turns = state.model_turns.max(sequence);
            state.last_activity = Some(Instant::now());
        }
    }

    /// Adds one accepted turn's token usage to the live counters.
    pub fn record_usage(&self, input_tokens: u64, output_tokens: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.input_tokens = state.input_tokens.saturating_add(input_tokens);
            state.output_tokens = state.output_tokens.saturating_add(output_tokens);
            state.last_activity = Some(Instant::now());
        }
    }

    /// The live counters and last committed sequence, for a settlement report or
    /// a stall report that must describe real work.
    pub fn live(&self) -> ChildLiveV1 {
        let Ok(state) = self.state.lock() else {
            return ChildLiveV1::default();
        };
        let since = state
            .last_activity
            .or(state.started)
            .map(|instant| instant.elapsed().as_secs())
            .unwrap_or(0);
        ChildLiveV1 {
            running: state.running,
            last_sequence: state.last_sequence,
            age_seconds: since,
            model_turns: state.model_turns,
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
        }
    }

    /// Queues a steering message for the child's next step boundary.
    pub fn steer(&self, text: String) {
        if let Ok(mut state) = self.state.lock() {
            state.steering.push(text);
        }
    }

    /// Drains queued steering messages at a step boundary.
    pub fn drain_steering(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.steering))
            .unwrap_or_default()
    }

    /// Latest live progress snapshot, if the run reported one.
    pub fn progress_value(&self) -> Option<Value> {
        self.state.lock().ok().and_then(|state| state.progress.clone())
    }

    /// Records the terminal outcome exactly once, from the run thread.
    pub fn finish(&self, result: Result<Value, String>) {
        if let Ok(mut state) = self.state.lock() {
            state.running = false;
            match result {
                Ok(child) => state.child = Some(child),
                Err(error) => state.error = Some(error),
            }
        }
    }
}

pub struct JobRegistry {
    root: PathBuf,
    state: Mutex<State>,
}

impl JobRegistry {
    pub fn open(root: PathBuf) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(&root).map_err(err)?;
        let connection = Connection::open(root.join("jobs.sqlite3")).map_err(err)?;
        connection
            .busy_timeout(Duration::from_millis(250))
            .map_err(err)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, record TEXT NOT NULL);").map_err(err)?;
        let mut entries = BTreeMap::new();
        {
            let mut statement = connection.prepare("SELECT record FROM jobs").map_err(err)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(err)?;
            for row in rows {
                let mut record: Record = serde_json::from_str(&row.map_err(err)?).map_err(err)?;
                if record.snapshot.running {
                    record.snapshot.running = false;
                    record.snapshot.error = Some(
                        "Aworkit restarted; the previous job session was interrupted and will not be replayed. Cleanup cannot be reconstructed from a PID."
                            .into(),
                    );
                    record.interrupted = true;
                    record.kept = None;
                    record.collected = false;
                }
                // Output may have grown after the last persisted lifecycle
                // snapshot, including immediately before an application crash.
                record.snapshot.stdout_bytes =
                    std::fs::metadata(root.join(&record.id).join("stdout.log"))
                        .map_or(0, |m| m.len());
                record.snapshot.stderr_bytes =
                    std::fs::metadata(root.join(&record.id).join("stderr.log"))
                        .map_or(0, |m| m.len());
                save(&connection, &record)?;
                entries.insert(
                    record.id.clone(),
                    Entry {
                        record,
                        runner: None,
                        cancellation: CancellationToken::default(),
                    },
                );
            }
        }
        let registry = Arc::new(Self {
            root,
            state: Mutex::new(State {
                connection,
                entries,
            }),
        });
        let weak = Arc::downgrade(&registry);
        thread::Builder::new()
            .name("aworkit-job-supervisor".into())
            .spawn(move || {
                loop {
                    let Some(registry) = weak.upgrade() else {
                        break;
                    };
                    if let Err(error) = registry.refresh() {
                        eprintln!("aworkit job supervision: {error}");
                    }
                    drop(registry);
                    thread::sleep(Duration::from_millis(100));
                }
            })
            .map_err(err)?;
        Ok(registry)
    }

    fn refresh(&self) -> Result<(), String> {
        let mut state = self.state.lock().map_err(err)?;
        let State {
            connection,
            entries,
        } = &mut *state;
        for entry in entries.values_mut() {
            let Some(runner) = entry.runner.clone() else {
                continue;
            };
            let (snapshot, child) = match runner {
                Runner::Process(session) => {
                    if entry.cancellation.is_cancelled() {
                        session.stop();
                    }
                    (session.snapshot().map_err(err)?, None)
                }
                Runner::Child(handle) => {
                    if entry.cancellation.is_cancelled() {
                        handle.cancel();
                    }
                    let live = handle.state.lock().map_err(err)?;
                    let snapshot = ProcessSnapshot {
                        running: live.running,
                        error: live.error.clone(),
                        elapsed_ms: live
                            .started
                            .map_or(0, |start| start.elapsed().as_millis() as u64),
                        stdout_bytes: std::fs::metadata(handle.directory.join("stdout.log"))
                            .map_or(0, |m| m.len()),
                        stderr_bytes: std::fs::metadata(handle.directory.join("stderr.log"))
                            .map_or(0, |m| m.len()),
                        ..entry.record.snapshot.clone()
                    };
                    (snapshot, live.child.clone())
                }
            };
            if snapshot.running {
                entry.record.snapshot = snapshot;
                continue;
            }
            let mut record = entry.record.clone();
            record.snapshot = snapshot;
            if record.kind == JobKind::Subagent {
                record.child = child;
            }
            // Keep the runner attached until the terminal state is durable, so
            // a failed save can be retried on the next refresh.
            save(connection, &record)?;
            entry.record = record;
            entry.runner = None;
        }
        Ok(())
    }

    /// Rebind retained jobs to the next owner pass, so Stop remains Chat-scoped.
    pub fn attach(&self, owner: &str, cancellation: CancellationToken) {
        if let Ok(mut state) = self.state.lock() {
            for entry in state
                .entries
                .values_mut()
                .filter(|e| e.record.owner == owner)
            {
                entry.cancellation = cancellation.clone();
            }
        }
    }

    pub fn stop_unkept(&self, owner: &str) {
        self.stop_unkept_scoped(owner, None);
    }

    pub fn stop_unkept_scoped(&self, owner: &str, scope: Option<&str>) {
        if let Ok(state) = self.state.lock() {
            for entry in state.entries.values().filter(|e| {
                e.record.owner == owner
                    && e.record.kept.is_none()
                    && scope.is_none_or(|scope| e.record.scope.as_deref() == Some(scope))
            }) {
                stop_runner(&entry.runner);
            }
        }
    }

    pub fn stop_all(&self, owner: &str) {
        if let Ok(state) = self.state.lock() {
            for entry in state.entries.values().filter(|e| e.record.owner == owner) {
                stop_runner(&entry.runner);
            }
        }
    }

    #[cfg(test)]
    pub fn start(
        &self,
        owner: &str,
        invocation: &str,
        spec: &ProcessSpecV1,
        interactive: bool,
        cancellation: CancellationToken,
    ) -> Result<String, String> {
        self.start_scoped(owner, invocation, None, spec, interactive, cancellation)
    }

    /// Scope is supplied by the trusted invocation context, never model arguments.
    pub fn start_scoped(
        &self,
        owner: &str,
        invocation: &str,
        scope: Option<&str>,
        spec: &ProcessSpecV1,
        interactive: bool,
        cancellation: CancellationToken,
    ) -> Result<String, String> {
        self.refresh()?;
        let id = job_id(owner, invocation);
        let mut state = self.state.lock().map_err(err)?;
        if state.entries.contains_key(&id) {
            return Ok(id);
        }
        admit_new_job(&self.root, &mut state, owner)?;
        if cancellation.is_cancelled() {
            return Err("job launch cancelled".into());
        }
        let mut record = Record {
            id: id.clone(),
            owner: owner.into(),
            invocation: invocation.into(),
            scope: scope.map(str::to_owned),
            kind: JobKind::Process,
            command: spec.arguments.clone(),
            snapshot: ProcessSnapshot {
                running: true,
                ..Default::default()
            },
            cursor: Default::default(),
            collected: false,
            kept: None,
            interrupted: false,
            released: false,
            release_reason: None,
            discarded: None,
            child: None,
        };
        // Commit identity before the side effect. A crash in this gap is uncertain, never replayed.
        save(&state.connection, &record)?;
        let session = match ProcessSession::start(spec, &self.root.join(&id), interactive) {
            Ok(session) => Some(Arc::new(session)),
            Err(error) => {
                record.snapshot.running = false;
                record.snapshot.error = Some(error.to_string());
                None
            }
        };
        if let Some(session) = &session {
            record.snapshot = session.snapshot().map_err(err)?;
        }
        let persisted = save(&state.connection, &record);
        if persisted.is_err()
            && let Some(session) = &session
        {
            session.stop();
        }
        state.entries.insert(
            id.clone(),
            Entry {
                record,
                runner: session.map(Runner::Process),
                cancellation,
            },
        );
        persisted?;
        Ok(id)
    }

    /// Registers one delegated child run as a background job and starts it on a
    /// private thread. Returns as soon as the job identity is durable, so the
    /// delegating Agent can keep working and query the child's state.
    pub fn start_child_scoped<F>(
        self: &Arc<Self>,
        owner: &str,
        invocation: &str,
        scope: Option<&str>,
        child_id: &str,
        cancellation: CancellationToken,
        run: F,
    ) -> Result<String, String>
    where
        F: FnOnce(Arc<ChildJobHandle>, CancellationToken) -> Result<Value, String> + Send + 'static,
    {
        self.refresh()?;
        let id = job_id(owner, invocation);
        let mut state = self.state.lock().map_err(err)?;
        if state.entries.contains_key(&id) {
            return Ok(id);
        }
        admit_new_job(&self.root, &mut state, owner)?;
        if cancellation.is_cancelled() {
            return Err("subagent launch cancelled".into());
        }
        let directory = self.root.join(&id);
        std::fs::create_dir_all(&directory).map_err(err)?;
        let token = CancellationToken::default();
        let record = Record {
            id: id.clone(),
            owner: owner.into(),
            invocation: invocation.into(),
            scope: scope.map(str::to_owned),
            kind: JobKind::Subagent,
            command: vec![child_id.to_owned()],
            snapshot: ProcessSnapshot {
                running: true,
                ..Default::default()
            },
            cursor: Default::default(),
            collected: false,
            kept: None,
            interrupted: false,
            released: false,
            release_reason: None,
            discarded: None,
            child: Some(json!({"childId": child_id, "status": "running"})),
        };
        save(&state.connection, &record)?;
        let handle = Arc::new(ChildJobHandle {
            cancellation: token.clone(),
            directory,
            state: Mutex::new(ChildJobState {
                running: true,
                started: Some(Instant::now()),
                ..Default::default()
            }),
        });
        state.entries.insert(
            id.clone(),
            Entry {
                record,
                runner: Some(Runner::Child(handle.clone())),
                cancellation,
            },
        );
        drop(state);
        let weak = Arc::downgrade(self);
        let thread_handle = handle.clone();
        let spawned = thread::Builder::new()
            .name(format!("aworkit-child-{id}"))
            .spawn(move || {
                let result = run(thread_handle.clone(), token);
                thread_handle.finish(result);
                if let Some(registry) = weak.upgrade() {
                    let _ = registry.refresh();
                }
            });
        if let Err(error) = spawned {
            handle.finish(Err(format!("child run thread could not start: {error}")));
            let _ = self.refresh();
            return Err(format!("child run thread could not start: {error}"));
        }
        Ok(id)
    }

    pub fn output(
        &self,
        owner: &str,
        id: &str,
        cursor: Option<ProcessOutputCursor>,
        maximum: usize,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.output_window(owner, id, cursor, maximum, wait, false, cancellation)
    }

    /// Reads the end of a job's captured streams instead of the next window.
    ///
    /// A tail window needs no cursor: it answers "how did this end?" for a log
    /// the Agent never intended to read in full, and reports how many bytes
    /// preceded the window.
    pub fn tail(
        &self,
        owner: &str,
        id: &str,
        maximum: usize,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.output_window(owner, id, None, maximum, wait, true, cancellation)
    }

    fn output_window(
        &self,
        owner: &str,
        id: &str,
        cursor: Option<ProcessOutputCursor>,
        maximum: usize,
        wait: Duration,
        tail: bool,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.refresh()?;
        let (runner, record) = {
            let state = self.state.lock().map_err(err)?;
            let entry = owned(&state, owner, id)?;
            (entry.runner.clone(), entry.record.clone())
        };
        if tail {
            return self.tail_window(owner, id, maximum, wait, cancellation);
        }
        let cursor = cursor.unwrap_or(record.cursor);
        let directory = self.root.join(id);
        let stdout_path = directory.join("stdout.log");
        let stderr_path = directory.join("stderr.log");
        let stdout_size = std::fs::metadata(&stdout_path).map_or(0, |m| m.len());
        let stderr_size = std::fs::metadata(&stderr_path).map_or(0, |m| m.len());
        if cursor.stdout > stdout_size || cursor.stderr > stderr_size {
            return Err(
                "output cursor exceeds captured output. Omit cursor to read new output from the last acknowledged position, or reuse a cursor returned for this job."
                    .into(),
            );
        }
        let output = if let Some(Runner::Process(session)) = runner {
            session
                .output(cursor, maximum, wait, cancellation)
                .map_err(err)?
        } else {
            // A delegated child writes progress to the same captured logs, so a
            // soft wait and a cursor read behave exactly like a process job.
            if record.snapshot.running && !wait.is_zero() {
                self.await_settlement(owner, id, wait, cancellation)?;
            }
            let record = {
                let state = self.state.lock().map_err(err)?;
                state
                    .entries
                    .get(id)
                    .ok_or("job unavailable")?
                    .record
                    .clone()
            };
            let stdout = read(&stdout_path, cursor.stdout, maximum)?;
            let stderr = read(
                &stderr_path,
                cursor.stderr,
                maximum.saturating_sub(stdout.len()),
            )?;
            let next = ProcessOutputCursor {
                stdout: cursor.stdout + stdout.len() as u64,
                stderr: cursor.stderr + stderr.len() as u64,
            };
            let snapshot = ProcessSnapshot {
                stdout_bytes: std::fs::metadata(&stdout_path).map_or(0, |m| m.len()),
                stderr_bytes: std::fs::metadata(&stderr_path).map_or(0, |m| m.len()),
                ..record.snapshot
            };
            let more = next.stdout < snapshot.stdout_bytes || next.stderr < snapshot.stderr_bytes;
            ProcessOutput {
                snapshot,
                stdout,
                stderr,
                next,
                more,
            }
        };
        self.output_result(owner, id, &directory, output, 0, false)
    }

    /// Reads the last bytes of both captured streams of one job.
    ///
    /// A tail read is a window over the captured files, so a delegated child and
    /// a process report identically. Two non-empty streams split the page
    /// evenly; a single non-empty stream gets all of it.
    fn tail_window(
        &self,
        owner: &str,
        id: &str,
        maximum: usize,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let running = {
            let state = self.state.lock().map_err(err)?;
            owned(&state, owner, id)?.record.snapshot.running
        };
        if running && !wait.is_zero() {
            self.await_settlement(owner, id, wait, cancellation)?;
        }
        let (record, directory) = {
            let state = self.state.lock().map_err(err)?;
            let entry = owned(&state, owner, id)?;
            (entry.record.clone(), self.root.join(id))
        };
        let captured = captured_tail(&directory, maximum)?;
        let snapshot = ProcessSnapshot {
            stdout_bytes: captured.stdout_total,
            stderr_bytes: captured.stderr_total,
            ..record.snapshot
        };
        self.output_result(
            owner,
            id,
            &directory,
            ProcessOutput {
                snapshot,
                stdout: captured.stdout,
                stderr: captured.stderr,
                next: captured.next,
                // A tail window ends at the end of the captured bytes; the
                // terminal check in output_result still reports newer output.
                more: false,
            },
            captured.omitted,
            true,
        )
    }

    /// Waits until a job stops running or the soft wait elapses.
    ///
    /// The deadline bounds the wait, never the job: a still-running job is
    /// reported by the caller as running.
    fn await_settlement(
        &self,
        owner: &str,
        id: &str,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline && !cancellation.is_cancelled() {
            self.refresh()?;
            let running = {
                let state = self.state.lock().map_err(err)?;
                owned(&state, owner, id)?.record.snapshot.running
            };
            if !running {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.refresh()
    }

    /// Render a captured read without changing lifecycle state. The supervisor
    /// may have persisted the exit and detached the runner while we read logs.
    fn output_result(
        &self,
        owner: &str,
        id: &str,
        directory: &Path,
        output: ProcessOutput,
        omitted: u64,
        tail: bool,
    ) -> Result<Value, String> {
        let state = self.state.lock().map_err(err)?;
        let entry = owned(&state, owner, id)?;
        let record = entry.record.clone();
        let live = match &entry.runner {
            Some(Runner::Child(handle)) => handle.progress_value(),
            _ => None,
        };
        drop(state);
        let ProcessOutput {
            snapshot,
            stdout,
            stderr,
            next,
            more,
        } = output;
        let (snapshot, more) = if snapshot.running && !record.snapshot.running {
            let terminal = record.snapshot.clone();
            let more = next.stdout < terminal.stdout_bytes || next.stderr < terminal.stderr_bytes;
            (terminal, more)
        } else {
            (snapshot, more)
        };
        Ok(json!({
            "jobId":id,
            "kind":record.kind,
            "running":snapshot.running,
            "status":job_status(&record, &snapshot),
            "exitCode":snapshot.exit_code,
            "rootExited":snapshot.root_exited,
            "treeEmpty":snapshot.tree_empty,
            "elapsedMs":snapshot.elapsed_ms,
            "stdout":String::from_utf8_lossy(&stdout),
            "stderr":String::from_utf8_lossy(&stderr),
            "cursor":next,
            "moreOutput":more,
            // The captured totals let the caller size the next read instead of
            // paging blindly; remainingBytes is what is still unread.
            "stdoutBytes":snapshot.stdout_bytes,
            "stderrBytes":snapshot.stderr_bytes,
            "remainingBytes":snapshot.stdout_bytes.saturating_sub(next.stdout)
                + snapshot.stderr_bytes.saturating_sub(next.stderr),
            "mode":if tail { "tail" } else { "read" },
            "omittedBytes":omitted,
            "error":snapshot.error,
            "inputError":snapshot.input_error,
            "kept":record.kept,
            "released":record.released,
            "outputDirectory":directory,
            "child":live.or(record.child),
        }))
    }

    #[cfg(test)]
    pub fn control(
        &self,
        owner: &str,
        operation: &str,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.control_scoped(owner, None, operation, args, cancellation)
    }

    pub fn control_scoped(
        &self,
        owner: &str,
        scope: Option<&str>,
        operation: &str,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.refresh()?;
        if operation == "job_list" {
            return self.list_scoped(owner, scope);
        }
        let id = args["jobId"].as_str().ok_or("jobId missing")?;
        if let Some(scope) = scope {
            let state = self.state.lock().map_err(err)?;
            if owned(&state, owner, id)?.record.scope.as_deref() != Some(scope) {
                return Err(
                    "job is not owned by this subagent; ask the parent to handle it".into(),
                );
            }
        }
        if operation == "job_output" {
            let cursor: Option<ProcessOutputCursor> = args
                .get("cursor")
                .map(|c| serde_json::from_value(c.clone()))
                .transpose()
                .map_err(err)?;
            let maximum = args["maximumBytes"].as_u64().unwrap_or(65536) as usize;
            let wait = Duration::from_millis(args["waitMs"].as_u64().unwrap_or(0));
            // `tail` reads the end of the captured streams; `read` continues
            // from the cursor. Both share the same bounded page.
            if args["mode"].as_str() == Some("tail") {
                return self.tail(owner, id, maximum, wait, cancellation);
            }
            return self.output(owner, id, cursor, maximum, wait, cancellation);
        }
        if operation == "job_release" {
            // Resolution needs no runner: a finished job whose process is gone
            // is exactly the case that must stay releasable.
            return self.release(
                owner,
                id,
                args["reason"].as_str(),
                args["discardOutput"].as_bool().unwrap_or(false),
            );
        }
        let runner = {
            let state = self.state.lock().map_err(err)?;
            owned(&state, owner, id)?.runner.clone()
        };
        match worker_of(runner) {
            WorkControlV1::Child(handle) => match operation {
                "job_input" => {
                    handle.steer(
                        args["text"]
                            .as_str()
                            .ok_or("text missing")?
                            .to_owned(),
                    );
                    Ok(json!({"jobId":id,"kind":"subagent","inputQueued":true,"mode":"steered"}))
                }
                "job_stop" => {
                    handle.cancel();
                    self.await_terminal(owner, id)?;
                    self.output(owner, id, None, 65536, Duration::ZERO, cancellation)
                }
                "job_keep" => {
                    if !handle.is_running() {
                        return self.keep_finished(owner, id);
                    }
                    let reason = args["reason"].as_str().ok_or("reason missing")?;
                    self.keep(owner, id, reason)?;
                    Ok(
                        json!({"jobId":id,"kind":"subagent","kept":true,"reason":reason,"lifetime":"until stopped or Aworkit exits"}),
                    )
                }
                _ => Err("unknown subagent job operation".into()),
            },
            WorkControlV1::Process(session) => {
                let Some(session) = session else {
                    if operation == "job_stop" {
                        return self.output(owner, id, None, 65536, Duration::ZERO, cancellation);
                    }
                    if operation == "job_keep" {
                        return self.keep_finished(owner, id);
                    }
                    return Err("job is no longer attached to a live process".into());
                };
                match operation {
                    "job_input" => {
                        session
                            .input(
                                args["text"]
                                    .as_str()
                                    .ok_or("text missing")?
                                    .as_bytes()
                                    .to_vec(),
                                args["closeStdin"].as_bool().unwrap_or(false),
                            )
                            .map_err(err)?;
                        Ok(json!({"jobId":id,"inputQueued":true,"stdinClosing":args["closeStdin"] == true}))
                    }
                    "job_stop" => {
                        session.stop();
                        let start = Instant::now();
                        while session.snapshot().map_err(err)?.running
                            && start.elapsed() < Duration::from_millis(2300)
                        {
                            thread::sleep(Duration::from_millis(20));
                        }
                        self.output(owner, id, None, 65536, Duration::ZERO, cancellation)
                    }
                    "job_keep" => {
                        if !session.snapshot().map_err(err)?.running {
                            return self.keep_finished(owner, id);
                        }
                        let reason = args["reason"].as_str().ok_or("reason missing")?;
                        self.keep(owner, id, reason)?;
                        Ok(
                            json!({"jobId":id,"kept":true,"reason":reason,"lifetime":"until stopped or Aworkit exits"}),
                        )
                    }
                    _ => Err("unknown job operation".into()),
                }
            }
            WorkControlV1::Gone => {
                if operation == "job_stop" {
                    return self.output(owner, id, None, 65536, Duration::ZERO, cancellation);
                }
                // A retained record whose runner is gone (a restart, or a
                // finished job) still answers the disposition questions.
                if operation == "job_keep" {
                    return self.keep_finished(owner, id);
                }
                Err("job is no longer attached to a live process".into())
            }
        }
    }

    /// Answers a keep request for a job that is already finished.
    ///
    /// A finished job is retained for the Chat already, so there is nothing to
    /// keep; the honest answer is a success that names the verb the Agent
    /// actually needs. A failure here made a legitimate question look like a
    /// broken tool and pushed one recorded run into editing the host's logs.
    fn keep_finished(&self, owner: &str, id: &str) -> Result<Value, String> {
        let state = self.state.lock().map_err(err)?;
        let record = owned(&state, owner, id)?.record.clone();
        Ok(json!({
            "jobId": id,
            "kept": false,
            "alreadyFinished": true,
            "status": job_status(&record, &record.snapshot),
            "hint": "a finished job is retained for this Chat; call job_release to acknowledge it without reading its output",
        }))
    }

    /// Records that the Agent is done with a finished job.
    ///
    /// `discard` deletes the captured logs and keeps their byte counts, so a job
    /// whose output nobody wants stops occupying disk and can never be read by
    /// accident. A running job must be stopped or kept first.
    fn release(
        &self,
        owner: &str,
        id: &str,
        reason: Option<&str>,
        discard: bool,
    ) -> Result<Value, String> {
        let mut state = self.state.lock().map_err(err)?;
        let State {
            connection,
            entries,
        } = &mut *state;
        let entry = entries
            .get_mut(id)
            .filter(|entry| entry.record.owner == owner)
            .ok_or("job does not exist in this Chat")?;
        if entry.record.snapshot.running {
            return Err(
                "job is still running; use job_stop to cancel it or job_keep to leave it running, then release it"
                    .into(),
            );
        }
        let already = entry.record.released;
        let mut record = entry.record.clone();
        if discard && record.discarded.is_none() {
            let directory = self.root.join(id);
            let discarded = DiscardedOutput {
                stdout_bytes: std::fs::metadata(directory.join("stdout.log"))
                    .map_or(0, |m| m.len()),
                stderr_bytes: std::fs::metadata(directory.join("stderr.log"))
                    .map_or(0, |m| m.len()),
            };
            for name in ["stdout.log", "stderr.log"] {
                let path = directory.join(name);
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(err(error));
                }
            }
            // The bytes are gone: report no captured bytes and a cursor at the
            // start, so a later read cannot claim to resume an old offset.
            record.snapshot.stdout_bytes = 0;
            record.snapshot.stderr_bytes = 0;
            record.cursor = ProcessOutputCursor::default();
            record.discarded = Some(discarded);
        }
        record.released = true;
        if let Some(reason) = reason {
            record.release_reason = Some(reason.to_owned());
        }
        save(connection, &record)?;
        let status = job_status(&record, &record.snapshot);
        let discarded = record.discarded.clone();
        entry.record = record;
        Ok(json!({
            "jobId": id,
            "released": true,
            "alreadyReleased": already,
            "status": status,
            "discardedOutput": discarded,
        }))
    }

    fn keep(&self, owner: &str, id: &str, reason: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(err)?;
        let entry = state.entries.get_mut(id).ok_or("job unavailable")?;
        if entry.record.owner != owner {
            return Err("job does not exist in this Chat".into());
        }
        entry.record.kept = Some(reason.to_owned());
        let record = entry.record.clone();
        save(&state.connection, &record)
    }

    fn await_terminal(&self, owner: &str, id: &str) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(2300) {
            self.refresh()?;
            let running = {
                let state = self.state.lock().map_err(err)?;
                owned(&state, owner, id)?.record.snapshot.running
            };
            if !running {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Advance collection only after the normal broker has durably recorded the
    /// returned output. A crash before that boundary leaves it readable again.
    pub fn acknowledge(&self, owner: &str, value: &Value) -> Result<(), String> {
        let Some(id) = value["jobId"].as_str() else {
            return Ok(());
        };
        let Some(cursor) = value.get("cursor") else {
            return Ok(());
        };
        let cursor: ProcessOutputCursor = serde_json::from_value(cursor.clone()).map_err(err)?;
        let mut state = self.state.lock().map_err(err)?;
        let State {
            connection,
            entries,
        } = &mut *state;
        let entry = entries
            .get_mut(id)
            .filter(|entry| entry.record.owner == owner)
            .ok_or("job does not exist in this Chat")?;
        let mut record = entry.record.clone();
        // Parallel or explicit rereads may finish in a different order. A
        // committed result can advance collection, but never undo it.
        record.cursor.stdout = record.cursor.stdout.max(cursor.stdout);
        record.cursor.stderr = record.cursor.stderr.max(cursor.stderr);
        record.collected |= value["running"] == false && value["moreOutput"] == false;
        save(connection, &record)?;
        entry.record = record;
        Ok(())
    }

    pub fn list_scoped(&self, owner: &str, scope: Option<&str>) -> Result<Value, String> {
        let state = self.state.lock().map_err(err)?;
        Ok(
            json!({
                "jobs": state
                    .entries
                    .values()
                    .filter(|e| {
                        e.record.owner == owner
                            && scope.is_none_or(|s| e.record.scope.as_deref() == Some(s))
                    })
                    .map(|e| json!({
                        "jobId": e.record.id,
                        "kind": e.record.kind,
                        "running": e.record.snapshot.running,
                        "status": job_status(&e.record, &e.record.snapshot),
                        "childId": e.record.child.as_ref().and_then(|child| child["childId"].as_str()),
                        "exitCode": e.record.snapshot.exit_code,
                        "error": e.record.snapshot.error,
                        "collected": e.record.collected,
                        "kept": e.record.kept,
                        "released": e.record.released,
                        // Captured sizes stay listed after a discard, so a
                        // resolved job can still be explained without its bytes.
                        "stdoutBytes": e.record.snapshot.stdout_bytes,
                        "stderrBytes": e.record.snapshot.stderr_bytes,
                        "discardedOutput": e.record.discarded,
                        "cursor": e.record.cursor,
                    }))
                    .collect::<Vec<_>>()
            }),
        )
    }

    /// The child identity behind a job, when the job is a delegated child run.
    pub fn child_job(&self, owner: &str, id: &str) -> Result<Option<ChildJobInfo>, String> {        let state = self.state.lock().map_err(err)?;
        let entry = owned(&state, owner, id)?;
        if entry.record.kind != JobKind::Subagent {
            return Ok(None);
        }
        let child_id = entry
            .record
            .child
            .as_ref()
            .and_then(|child| child["childId"].as_str())
            .map(str::to_owned);
        Ok(child_id.map(|child_id| ChildJobInfo {
            child_id,
            running: entry.record.snapshot.running,
        }))
    }

    /// Whether a delegated child of this Chat currently has a live job. A
    /// frame that still says `running` without a live job was interrupted by a
    /// restart and must never be read as running.
    pub fn child_running(&self, owner: &str, child_id: &str) -> bool {
        self.running_child_job(owner, child_id).is_some()
    }

    /// The live counters of one delegated child, including when it last produced
    /// a step. A frame that still says `running` without a live job has been
    /// interrupted; a live job whose sequence has not advanced is a stall.
    pub fn child_live(&self, owner: &str, child_id: &str) -> Option<ChildLiveV1> {
        let _ = self.refresh();
        let state = self.state.lock().ok()?;
        state
            .entries
            .values()
            .find(|entry| {
                entry.record.owner == owner
                    && entry.record.kind == JobKind::Subagent
                    && entry
                        .record
                        .child
                        .as_ref()
                        .and_then(|child| child["childId"].as_str())
                        == Some(child_id)
            })
            .and_then(|entry| match &entry.runner {
                Some(Runner::Child(handle)) => Some(handle.live()),
                _ => None,
            })
    }

    /// The live job id of one delegated child, when it has one.
    pub fn running_child_job(&self, owner: &str, child_id: &str) -> Option<String> {
        let _ = self.refresh();
        let state = self.state.lock().ok()?;
        state
            .entries
            .values()
            .find(|entry| {
                entry.record.owner == owner
                    && entry.record.kind == JobKind::Subagent
                    && entry.record.snapshot.running
                    && entry
                        .record
                        .child
                        .as_ref()
                        .and_then(|child| child["childId"].as_str())
                        == Some(child_id)
            })
            .map(|entry| entry.record.id.clone())
    }

    /// A runtime gate, independent of model promises to clean up later.
    pub fn completion_notice(&self, owner: &str) -> Result<Option<String>, String> {
        self.completion_notice_scoped(owner, None)
    }

    /// One bounded summary of the jobs this owner has not resolved.
    ///
    /// Jobs are listed in id order so the text is stable between turns; the
    /// rendering itself is [`render_job_notice`], which caps the inventory and
    /// states how many jobs it omitted. A notice never fails a request and never
    /// ends a turn.
    pub fn completion_notice_scoped(
        &self,
        owner: &str,
        scope: Option<&str>,
    ) -> Result<Option<String>, String> {
        self.refresh()?;
        let state = self.state.lock().map_err(err)?;
        let mut unresolved = state
            .entries
            .values()
            .filter(|e| {
                e.record.owner == owner
                    && scope.is_none_or(|scope| e.record.scope.as_deref() == Some(scope))
                    && if e.record.snapshot.running {
                        e.record.kept.is_none()
                    } else {
                        // A terminal job is resolved by reading it to the end or
                        // by releasing it; only an unresolved one is reported.
                        !e.record.collected && !e.record.released
                    }
            })
            .map(|e| {
                format!(
                    "{}: {}",
                    e.record.id,
                    if e.record.snapshot.running {
                        "still running"
                    } else {
                        "finished; output not collected"
                    }
                )
            })
            .collect::<Vec<_>>();
        if unresolved.is_empty() {
            return Ok(None);
        }
        unresolved.sort_unstable();
        Ok(Some(render_job_notice(&unresolved)))
    }
}

/// Byte budget for the listed job inventory. The dispatch bound is 4 KiB for the
/// whole notice, which also carries the frame, the trailer and any other runtime
/// notice, so the inventory stays well below it.
const MAXIMUM_NOTICE_INVENTORY_BYTES: usize = 2048;
/// Room for the omitted-count sentence, whose digits vary.
const OMISSION_RESERVE: usize = 96;
const NOTICE_FRAME: &str = "Before finishing this response, resolve these jobs: ";
const NOTICE_TRAILER: &str = ". Use job_output to read/wait (mode=tail shows the end of a long log), job_input to steer a running subagent or send stdin to a process, job_stop to cancel, job_release to acknowledge a finished job without reading it, or job_keep with an explicit reason if leaving background work running is intended. Do not start a replacement for an existing job. A soft wait is not a failure.";

/// Render one bounded job-inventory summary from already-rendered job lines.
///
/// The notice is a report, not an enumeration: the listed inventory is capped and
/// the message states how many jobs it omitted, so a run with hundreds of
/// uncollected jobs produces the same bounded text instead of one that grows with
/// the job count. Pure so the bound is testable without starting processes.
pub(crate) fn render_job_notice(unresolved: &[String]) -> String {
    let budget = MAXIMUM_NOTICE_INVENTORY_BYTES
        .saturating_sub(NOTICE_TRAILER.len().saturating_add(OMISSION_RESERVE));
    let mut listed: Vec<&str> = Vec::new();
    let mut used = 0usize;
    let mut omitted = 0usize;
    for (index, line) in unresolved.iter().enumerate() {
        let separator = if index == 0 { 0 } else { 2 };
        if used.saturating_add(separator).saturating_add(line.len()) > budget {
            omitted = unresolved.len().saturating_sub(index);
            break;
        }
        used = used.saturating_add(separator).saturating_add(line.len());
        listed.push(line.as_str());
    }
    let mut notice = format!("{NOTICE_FRAME}{}{NOTICE_TRAILER}", listed.join("; "));
    if omitted > 0 {
        notice.push_str(&format!(
            " {omitted} more unresolved job(s) are not listed; call job_list to see them all."
        ));
    }
    notice
}

/// What a control operation should do with an entry's runner.
enum WorkControlV1 {
    Child(Arc<ChildJobHandle>),
    Process(Option<Arc<ProcessSession>>),
    Gone,
}

fn worker_of(runner: Option<Runner>) -> WorkControlV1 {
    match runner {
        Some(Runner::Child(handle)) => WorkControlV1::Child(handle),
        Some(Runner::Process(session)) => WorkControlV1::Process(Some(session)),
        None => WorkControlV1::Gone,
    }
}

fn stop_runner(runner: &Option<Runner>) {
    match runner {
        Some(Runner::Process(session)) => session.stop(),
        Some(Runner::Child(handle)) => handle.cancel(),
        None => {}
    }
}

fn job_id(owner: &str, invocation: &str) -> String {
    format!(
        "job.{:x}",
        Sha256::digest(format!("{owner}\0{invocation}").as_bytes())
    )
}

fn job_status(record: &Record, snapshot: &ProcessSnapshot) -> &'static str {
    if record.interrupted {
        "interrupted"
    } else if snapshot.running {
        "running"
    } else if snapshot.stopped {
        "stopped"
    } else if snapshot.error.is_some() {
        "failed"
    } else if record.child.as_ref().and_then(|child| child["status"].as_str()) == Some("cancelled") {
        "stopped"
    } else if record.child.is_some() {
        "completed"
    } else {
        "exited"
    }
}

/// Retains at most 128 captured jobs. Only terminal records the Agent resolved
/// are evicted; running or unresolved work can never disappear here.
fn admit_new_job(root: &Path, state: &mut State, owner: &str) -> Result<(), String> {
    while state.entries.len() >= 128 {
        let old = state
            .entries
            .iter()
            .find(|(_, e)| {
                !e.record.snapshot.running && (e.record.collected || e.record.released)
            })
            .map(|(id, _)| id.clone())
            .ok_or("job history is full; collect finished jobs first")?;
        if old.len() != 68
            || !old.starts_with("job.")
            || !old[4..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid persisted job identity".into());
        }
        let path = root.join(&old);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(err)?;
        }
        state
            .connection
            .execute("DELETE FROM jobs WHERE id=?1", [&old])
            .map_err(err)?;
        state.entries.remove(&old);
    }
    if state
        .entries
        .values()
        .filter(|e| e.record.snapshot.running)
        .count()
        >= 32
        || state
            .entries
            .values()
            .filter(|e| e.record.owner == owner && e.record.snapshot.running)
            .count()
            >= 8
    {
        return Err(
            "running job capacity reached (8 per Chat, 32 total); stop or finish a job first".into(),
        );
    }
    Ok(())
}

fn owned<'a>(state: &'a State, owner: &str, id: &str) -> Result<&'a Entry, String> {
    state
        .entries
        .get(id)
        .filter(|entry| entry.record.owner == owner)
        .ok_or_else(|| "job does not exist in this Chat".into())
}
fn save(connection: &Connection, record: &Record) -> Result<(), String> {
    connection.execute("INSERT INTO jobs(id,record) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record", params![record.id, serde_json::to_string(record).map_err(err)?]).map_err(err)?;
    Ok(())
}
/// One tail window over a job's captured streams and the bytes it skipped.
struct CapturedTailV1 {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    next: ProcessOutputCursor,
    omitted: u64,
    stdout_total: u64,
    stderr_total: u64,
}

/// Reads the end of a job's captured stdout and stderr.
///
/// A tail is the cheap way to see how a long command ended, so it never pages
/// from the start: each non-empty stream is read backwards from its end, two
/// non-empty streams split the page evenly, and the number of bytes before the
/// window is reported so the omission is explicit.
fn captured_tail(directory: &Path, maximum: usize) -> Result<CapturedTailV1, String> {
    let stdout_path = directory.join("stdout.log");
    let stderr_path = directory.join("stderr.log");
    let stdout_total = std::fs::metadata(&stdout_path).map_or(0, |m| m.len());
    let stderr_total = std::fs::metadata(&stderr_path).map_or(0, |m| m.len());
    let (stdout_budget, stderr_budget) = match (stdout_total > 0, stderr_total > 0) {
        (true, true) => (maximum / 2, maximum - maximum / 2),
        (true, false) => (maximum, 0),
        (false, true) => (0, maximum),
        (false, false) => (0, 0),
    };
    let stdout = read_tail(&stdout_path, stdout_budget)?;
    let stderr = read_tail(&stderr_path, stderr_budget)?;
    let omitted = stdout_total.saturating_sub(stdout.len() as u64)
        + stderr_total.saturating_sub(stderr.len() as u64);
    Ok(CapturedTailV1 {
        next: ProcessOutputCursor {
            stdout: stdout_total,
            stderr: stderr_total,
        },
        stdout,
        stderr,
        omitted,
        stdout_total,
        stderr_total,
    })
}

/// Reads at most `maximum` bytes from the end of one captured file.
fn read_tail(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    if maximum == 0 {
        return Ok(Vec::new());
    }
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(err(error)),
    };
    let length = file.metadata().map_err(err)?.len();
    let start = length.saturating_sub(maximum as u64);
    file.seek(SeekFrom::Start(start)).map_err(err)?;
    let mut bytes = Vec::new();
    file.take(length - start)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    // A window that begins mid-line is trimmed to its first complete line, so a
    // tail reads as text instead of starting with a torn character; the reported
    // omission grows by exactly what was trimmed.
    if start > 0
        && let Some(index) = bytes.iter().position(|byte| *byte == b'\n')
    {
        bytes.drain(..=index);
    }
    Ok(bytes)
}

fn read(path: &Path, cursor: u64, maximum: usize) -> Result<Vec<u8>, String> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(err(e)),
    };
    file.seek(SeekFrom::Start(cursor)).map_err(err)?;
    let mut bytes = Vec::new();
    file.take(maximum as u64)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    Ok(bytes)
}
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_captured_before_exit_cannot_restore_running_or_undo_collection() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let (finish, resume) = std::sync::mpsc::channel();
        let owner = "chat.race";
        let id = registry
            .start_child_scoped(owner, "invocation.race", None, "child.race", token.clone(), move |handle, _| {
                resume.recv().map_err(err)?;
                handle.progress("final output", json!({"status":"running"}));
                Ok(json!({"status":"completed"}))
            })
            .unwrap();
        // Capture a read, then force settlement before the read builds its reply.
        let snapshot = {
            let state = registry.state.lock().unwrap();
            owned(&state, owner, &id).unwrap().record.snapshot.clone()
        };
        assert!(snapshot.running);
        let captured = ProcessOutput {
            snapshot,
            stdout: Vec::new(),
            stderr: Vec::new(),
            next: ProcessOutputCursor::default(),
            more: false,
        };
        finish.send(()).unwrap();
        registry.await_terminal(owner, &id).unwrap();
        let late = registry
            .output_result(owner, &id, &registry.root.join(&id), captured, 0, false)
            .unwrap();
        assert_eq!(late["status"], "completed");
        assert_eq!(late["moreOutput"], true);

        let final_read = registry
            .output(owner, &id, None, 65536, Duration::ZERO, &token)
            .unwrap();
        assert_eq!(final_read["stdout"], "final output\n");
        assert_eq!(final_read["running"], false);
        assert_eq!(final_read["moreOutput"], false);
        registry.acknowledge(owner, &final_read).unwrap();
        registry.acknowledge(owner, &late).unwrap();
        assert!(registry.completion_notice(owner).unwrap().is_none());
        let jobs = registry.list_scoped(owner, None).unwrap();
        assert_eq!(jobs["jobs"][0]["status"], "completed");
        assert_eq!(jobs["jobs"][0]["cursor"], final_read["cursor"]);
    }

    #[test]
    fn a_child_runner_is_a_job_and_reports_live_then_terminal_state() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let id = registry
            .start_child_scoped(
                "chat.one",
                "invocation.one",
                Some("agent.one"),
                "child.child.one",
                CancellationToken::default(),
                |handle, _| {
                    handle.progress("working", json!({"childId":"child.child.one","status":"running"}));
                    thread::sleep(Duration::from_millis(40));
                    Ok(json!({"childId":"child.child.one","status":"completed"}))
                },
            )
            .unwrap();
        assert!(id.starts_with("job."));
        let info = registry.child_job("chat.one", &id).unwrap().unwrap();
        assert_eq!(info.child_id, "child.child.one");
        let live = registry
            .output("chat.one", &id, None, 65536, Duration::ZERO, &CancellationToken::default())
            .unwrap();
        assert_eq!(live["kind"], "subagent");
        assert_eq!(live["running"], true);
        let settled = registry
            .output(
                "chat.one",
                &id,
                None,
                65536,
                Duration::from_secs(2),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(settled["running"], false);
        assert_eq!(settled["status"], "completed");
        assert_eq!(settled["child"]["status"], "completed");
        assert!(settled["stdout"].as_str().unwrap().contains("working"));
        // A second control path must not find it running.
        assert!(!registry.child_job("chat.one", &id).unwrap().unwrap().running);
    }

    #[test]
    fn a_child_job_is_kept_or_stopped_and_not_owned_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let id = registry
            .start_child_scoped(
                "chat.two",
                "invocation.two",
                None,
                "child.child.two",
                CancellationToken::default(),
                |_, token| {
                    while !token.is_cancelled() {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(json!({"childId":"child.child.two","status":"cancelled"}))
                },
            )
            .unwrap();
        let kept = registry
            .control_scoped(
                "chat.two",
                None,
                "job_keep",
                &json!({"jobId":id,"reason":"long research"}),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(kept["kept"], true);
        assert!(
            registry
                .completion_notice("chat.two")
                .unwrap()
                .is_none(),
            "a kept running job no longer blocks completion"
        );
        assert!(registry.child_job("chat.other", &id).is_err());
        let stopped = registry
            .control_scoped(
                "chat.two",
                None,
                "job_stop",
                &json!({"jobId":id}),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(stopped["running"], false);
    }

    #[test]
    fn a_restart_marks_a_running_child_job_interrupted_without_replay() {
        let root = tempfile::tempdir().unwrap();
        {
            let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
            let _ = registry
                .start_child_scoped(
                    "chat.three",
                    "invocation.three",
                    None,
                    "child.child.three",
                    CancellationToken::default(),
                    |_, _| {
                        thread::sleep(Duration::from_millis(5_000));
                        Ok(json!({"status":"completed"}))
                    },
                )
                .unwrap();
            // The registry is dropped while the child is still running.
        }
        let reopened = JobRegistry::open(root.path().join("jobs")).unwrap();
        let listed = reopened.list_scoped("chat.three", None).unwrap();
        assert_eq!(listed["jobs"][0]["kind"], "subagent");
        assert_eq!(listed["jobs"][0]["running"], false);
        assert_eq!(listed["jobs"][0]["status"], "interrupted");
        assert!(
            listed["jobs"][0]["error"]
                .as_str()
                .unwrap()
                .contains("interrupted")
        );
    }

    /// A finished job can be resolved without reading it: the completion notice
    /// must be satisfiable without draining a log into the model context.
    #[test]
    fn a_released_job_leaves_the_notice_without_being_read() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let id = registry
            .start_child_scoped(
                "chat.release",
                "invocation.release",
                None,
                "child.release",
                CancellationToken::default(),
                |handle, _| {
                    handle.progress("first line", json!({"status":"running"}));
                    Ok(json!({"childId":"child.release","status":"completed"}))
                },
            )
            .unwrap();
        registry.await_terminal("chat.release", &id).unwrap();
        assert!(registry.completion_notice("chat.release").unwrap().is_some());

        let released = registry
            .control_scoped("chat.release", None, "job_release", &json!({"jobId": id}), &token)
            .unwrap();
        assert_eq!(released["released"], true);
        assert_eq!(released["alreadyReleased"], false);
        assert!(registry.completion_notice("chat.release").unwrap().is_none());

        // Releasing again is a no-op, and the record still admits it was never read.
        let again = registry
            .control_scoped(
                "chat.release",
                None,
                "job_release",
                &json!({"jobId": id, "reason":"duplicate"}),
                &token,
            )
            .unwrap();
        assert_eq!(again["alreadyReleased"], true);
        let listed = registry.list_scoped("chat.release", None).unwrap();
        assert_eq!(listed["jobs"][0]["released"], true);
        assert_eq!(listed["jobs"][0]["collected"], false);
        assert!(
            listed["jobs"][0]["stdoutBytes"].as_u64().unwrap() > 0,
            "{listed}"
        );
    }

    #[test]
    fn release_can_discard_the_captured_output_and_keeps_its_counts() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let id = registry
            .start_child_scoped(
                "chat.discard",
                "invocation.discard",
                None,
                "child.discard",
                CancellationToken::default(),
                |handle, _| {
                    for index in 0..20 {
                        handle.progress(&format!("line {index}"), json!({"status":"running"}));
                    }
                    Ok(json!({"childId":"child.discard","status":"completed"}))
                },
            )
            .unwrap();
        registry.await_terminal("chat.discard", &id).unwrap();
        let directory = registry.root.join(&id);
        let captured = std::fs::metadata(directory.join("stdout.log")).unwrap().len();
        assert!(captured > 0);

        let released = registry
            .control_scoped(
                "chat.discard",
                None,
                "job_release",
                &json!({"jobId": id, "discardOutput": true}),
                &token,
            )
            .unwrap();
        assert_eq!(released["discardedOutput"]["stdoutBytes"], json!(captured));
        assert!(!directory.join("stdout.log").exists());
        assert!(!directory.join("stderr.log").exists());

        // The record still explains the job; only the bytes are gone.
        let listed = registry.list_scoped("chat.discard", None).unwrap();
        assert_eq!(listed["jobs"][0]["status"], "completed");
        assert_eq!(listed["jobs"][0]["stdoutBytes"], json!(0));
        assert_eq!(
            listed["jobs"][0]["discardedOutput"]["stdoutBytes"],
            json!(captured)
        );
        let read = registry
            .output("chat.discard", &id, None, 4096, Duration::ZERO, &token)
            .unwrap();
        assert_eq!(read["stdout"], "");
        assert_eq!(read["stdoutBytes"], json!(0));
        assert_eq!(read["moreOutput"], json!(false));
    }

    #[test]
    fn release_refuses_a_running_job_and_keep_answers_with_the_release_path() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let id = registry
            .start_child_scoped(
                "chat.busy",
                "invocation.busy",
                None,
                "child.busy",
                token.clone(),
                |_, token| {
                    while !token.is_cancelled() {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(json!({"childId":"child.busy","status":"cancelled"}))
                },
            )
            .unwrap();
        let refused = registry
            .control_scoped(
                "chat.busy",
                None,
                "job_release",
                &json!({"jobId": id}),
                &CancellationToken::default(),
            )
            .unwrap_err();
        assert!(refused.contains("still running"), "{refused}");

        token.cancel();
        registry.await_terminal("chat.busy", &id).unwrap();
        // Keeping a finished job is a legitimate question with a useful answer,
        // not a failed tool event.
        let kept = registry
            .control_scoped(
                "chat.busy",
                None,
                "job_keep",
                &json!({"jobId": id, "reason":"already done"}),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(kept["kept"], json!(false));
        assert_eq!(kept["alreadyFinished"], json!(true));
        assert!(
            kept["hint"].as_str().unwrap().contains("job_release"),
            "{kept}"
        );
    }

    #[test]
    fn keep_after_restart_answers_instead_of_failing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("jobs");
        let registry = JobRegistry::open(path.clone()).unwrap();
        let id = registry
            .start_child_scoped(
                "chat.restart",
                "invocation.restart",
                None,
                "child.restart",
                CancellationToken::default(),
                |_, _| Ok(json!({"childId":"child.restart","status":"completed"})),
            )
            .unwrap();
        registry.await_terminal("chat.restart", &id).unwrap();
        drop(registry);

        // The runner is gone after a restart; the finished record still answers.
        let reopened = JobRegistry::open(path).unwrap();
        let kept = reopened
            .control_scoped(
                "chat.restart",
                None,
                "job_keep",
                &json!({"jobId": id, "reason":"done"}),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(kept["alreadyFinished"], json!(true));
    }

    #[test]
    fn a_tail_read_returns_the_end_and_reports_what_it_skipped() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let id = registry
            .start_child_scoped(
                "chat.tail",
                "invocation.tail",
                None,
                "child.tail",
                CancellationToken::default(),
                |handle, _| {
                    for index in 0..200 {
                        handle.progress(&format!("line {index}"), json!({"status":"running"}));
                    }
                    Ok(json!({"childId":"child.tail","status":"completed"}))
                },
            )
            .unwrap();
        registry.await_terminal("chat.tail", &id).unwrap();

        let tail = registry
            .tail("chat.tail", &id, 32, Duration::ZERO, &token)
            .unwrap();
        let total = tail["stdoutBytes"].as_u64().unwrap();
        assert!(total > 32, "{tail}");
        let text = tail["stdout"].as_str().unwrap();
        // A tail window is trimmed to its first complete line.
        assert!(text.ends_with("line 199\n"), "{text}");
        assert!(text.len() <= 32, "{text}");
        assert_eq!(tail["mode"], json!("tail"));
        assert_eq!(tail["omittedBytes"], json!(total - text.len() as u64));
        assert_eq!(tail["remainingBytes"], json!(0));
        assert_eq!(tail["moreOutput"], json!(false));
        assert_eq!(tail["cursor"]["stdout"], json!(total));
    }

    #[test]
    fn a_read_reports_the_captured_totals_and_what_remains() {
        let root = tempfile::tempdir().unwrap();
        let registry = JobRegistry::open(root.path().join("jobs")).unwrap();
        let token = CancellationToken::default();
        let id = registry
            .start_child_scoped(
                "chat.totals",
                "invocation.totals",
                None,
                "child.totals",
                CancellationToken::default(),
                |handle, _| {
                    for index in 0..20 {
                        handle.progress(&format!("line {index}"), json!({"status":"running"}));
                    }
                    Ok(json!({"childId":"child.totals","status":"completed"}))
                },
            )
            .unwrap();
        registry.await_terminal("chat.totals", &id).unwrap();
        let read = registry
            .output("chat.totals", &id, None, 8, Duration::ZERO, &token)
            .unwrap();
        let total = read["stdoutBytes"].as_u64().unwrap();
        assert!(total > 8, "{read}");
        assert_eq!(read["stderrBytes"], json!(0));
        assert_eq!(read["remainingBytes"], json!(total - 8));
        assert_eq!(read["moreOutput"], json!(true));
        assert_eq!(read["mode"], json!("read"));
        assert_eq!(read["omittedBytes"], json!(0));
    }
}

#[cfg(test)]
mod live_child_tests {
    use super::*;

    /// The counters a settlement or cancellation reports must describe the work
    /// the child really did. The recorded incident reported zero turns and zero
    /// tokens for children that had already run six to eight model turns.
    #[test]
    fn a_live_child_reports_the_turns_and_tokens_it_really_spent() {
        let directory = std::env::temp_dir().join(format!(
            "aworkit-child-live-{}-{}",
            std::process::id(),
            "counters"
        ));
        std::fs::create_dir_all(&directory).expect("temporary child directory");
        let handle = ChildJobHandle {
            cancellation: CancellationToken::default(),
            directory,
            state: Mutex::new(ChildJobState {
                running: true,
                started: Some(Instant::now()),
                ..Default::default()
            }),
        };
        assert_eq!(handle.live().model_turns, 0);
        handle.record_turn(7);
        handle.record_usage(200, 40);
        let live = handle.live();
        assert_eq!(live.model_turns, 7);
        assert_eq!(live.last_sequence, 7);
        assert_eq!(live.input_tokens, 200);
        assert_eq!(live.output_tokens, 40);
        assert!(live.running);
    }

    #[test]
    fn a_child_that_stopped_reporting_has_an_age_to_show() {
        let directory = std::env::temp_dir().join(format!(
            "aworkit-child-live-{}-{}",
            std::process::id(),
            "stall"
        ));
        std::fs::create_dir_all(&directory).expect("temporary child directory");
        let handle = ChildJobHandle {
            cancellation: CancellationToken::default(),
            directory,
            state: Mutex::new(ChildJobState {
                running: true,
                started: Some(Instant::now() - Duration::from_secs(600)),
                ..Default::default()
            }),
        };
        // The age is measured from the last committed step or, before the first
        // one, from the moment the child started.
        assert!(handle.live().age_seconds >= 600);
        assert_eq!(handle.live().last_sequence, 0);
    }
}
