//! Durable job identities and cursors, with bounded in-memory native runners.
//!
//! A job is owned by one Chat. Its runner is either an OS process session
//! (shell/Python) or an in-process delegated child agent run. Both kinds share
//! the same persisted identity, ownership scope, running/terminal snapshot,
//! captured output cursor, keep/stop/input control surface and completion
//! barrier, so the model observes and controls them through the same tools.
use aworkit_capability_host::{
    CancellationToken, ProcessOutputCursor, ProcessSession, ProcessSnapshot, ProcessSpecV1,
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
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("stdout.log"))
        {
            let _ = writeln!(file, "{line}");
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
            match runner {
                Runner::Process(session) => {
                    if entry.cancellation.is_cancelled() {
                        session.stop();
                    }
                    let snapshot = session.snapshot().map_err(err)?;
                    let settled = entry.record.snapshot.running && !snapshot.running;
                    entry.record.snapshot = snapshot;
                    if settled {
                        save(connection, &entry.record)?;
                    }
                    if !entry.record.snapshot.running {
                        // Release the stdin sender and native session once the
                        // immutable files and terminal facts are sufficient.
                        entry.runner = None;
                    }
                }
                Runner::Child(handle) => {
                    if entry.cancellation.is_cancelled() {
                        handle.cancel();
                    }
                    let (running, error, child, elapsed_ms, stdout_bytes, stderr_bytes) = {
                        let live = handle.state.lock().map_err(err)?;
                        (
                            live.running,
                            live.error.clone(),
                            live.child.clone(),
                            live.started
                                .map_or(0, |start| start.elapsed().as_millis() as u64),
                            std::fs::metadata(handle.directory.join("stdout.log"))
                                .map_or(0, |m| m.len()),
                            std::fs::metadata(handle.directory.join("stderr.log"))
                                .map_or(0, |m| m.len()),
                        )
                    };
                    let settled = entry.record.snapshot.running && !running;
                    entry.record.snapshot.running = running;
                    entry.record.snapshot.elapsed_ms = elapsed_ms;
                    entry.record.snapshot.stdout_bytes = stdout_bytes;
                    entry.record.snapshot.stderr_bytes = stderr_bytes;
                    if settled {
                        entry.record.snapshot.error = error;
                        entry.record.child = child;
                        save(connection, &entry.record)?;
                    }
                    if !running {
                        entry.runner = None;
                    }
                }
            }
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
        self.refresh()?;
        let (runner, record) = {
            let state = self.state.lock().map_err(err)?;
            let entry = owned(&state, owner, id)?;
            (entry.runner.clone(), entry.record.clone())
        };
        let cursor = cursor.unwrap_or(record.cursor);
        let directory = self.root.join(id);
        let stdout_path = directory.join("stdout.log");
        let stderr_path = directory.join("stderr.log");
        let stdout_size = std::fs::metadata(&stdout_path).map_or(0, |m| m.len());
        let stderr_size = std::fs::metadata(&stderr_path).map_or(0, |m| m.len());
        if cursor.stdout > stdout_size || cursor.stderr > stderr_size {
            return Err("output cursor exceeds captured output".into());
        }
        let (snapshot, stdout, stderr, next, more) = if let Some(Runner::Process(session)) = runner {
            let out = session
                .output(cursor, maximum, wait, cancellation)
                .map_err(err)?;
            (out.snapshot, out.stdout, out.stderr, out.next, out.more)
        } else {
            // A delegated child writes progress to the same captured logs, so a
            // soft wait and a cursor read behave exactly like a process job.
            if record.snapshot.running && !wait.is_zero() {
                let deadline = Instant::now() + wait;
                while Instant::now() < deadline && !cancellation.is_cancelled() {
                    self.refresh()?;
                    let running = {
                        let state = self.state.lock().map_err(err)?;
                        state
                            .entries
                            .get(id)
                            .is_some_and(|entry| entry.record.snapshot.running)
                    };
                    if !running {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                self.refresh()?;
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
                stdout_bytes: stdout_size,
                stderr_bytes: stderr_size,
                ..record.snapshot.clone()
            };
            let more = next.stdout < snapshot.stdout_bytes || next.stderr < snapshot.stderr_bytes;
            (snapshot, stdout, stderr, next, more)
        };
        let mut state = self.state.lock().map_err(err)?;
        let entry = state.entries.get_mut(id).ok_or("job unavailable")?;
        entry.record.snapshot = snapshot.clone();
        let record = entry.record.clone();
        let live = match &entry.runner {
            Some(Runner::Child(handle)) => handle.progress_value(),
            _ => None,
        };
        drop(state);
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
            "error":snapshot.error,
            "inputError":snapshot.input_error,
            "kept":record.kept,
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
            let cursor = args
                .get("cursor")
                .map(|c| serde_json::from_value(c.clone()))
                .transpose()
                .map_err(err)?;
            return self.output(
                owner,
                id,
                cursor,
                args["maximumBytes"].as_u64().unwrap_or(65536) as usize,
                Duration::from_millis(args["waitMs"].as_u64().unwrap_or(0)),
                cancellation,
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
                        return Err("job has already exited; collect its output".into());
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
                            return Err("job has already exited; collect its output".into());
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
                Err("job is no longer attached to a live process".into())
            }
        }
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
        owned(&state, owner, id)?;
        let entry = state.entries.get_mut(id).ok_or("job unavailable")?;
        entry.record.cursor = cursor;
        entry.record.collected = value["running"] == false && value["moreOutput"] == false;
        let record = entry.record.clone();
        save(&state.connection, &record)
    }

    pub fn list_scoped(&self, owner: &str, scope: Option<&str>) -> Result<Value, String> {
        let state = self.state.lock().map_err(err)?;
        Ok(
            json!({"jobs":state.entries.values().filter(|e| e.record.owner == owner && scope.is_none_or(|s| e.record.scope.as_deref() == Some(s))).map(|e| json!({"jobId":e.record.id,"kind":e.record.kind,"running":e.record.snapshot.running,"status":job_status(&e.record, &e.record.snapshot),"childId":e.record.child.as_ref().and_then(|child| child["childId"].as_str()),"exitCode":e.record.snapshot.exit_code,"error":e.record.snapshot.error,"collected":e.record.collected,"kept":e.record.kept,"cursor":e.record.cursor})).collect::<Vec<_>>()}),
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

    pub fn completion_notice_scoped(
        &self,
        owner: &str,
        scope: Option<&str>,
    ) -> Result<Option<String>, String> {
        self.refresh()?;
        let state = self.state.lock().map_err(err)?;
        let unresolved = state
            .entries
            .values()
            .filter(|e| {
                e.record.owner == owner
                    && scope.is_none_or(|scope| e.record.scope.as_deref() == Some(scope))
                    && if e.record.snapshot.running {
                        e.record.kept.is_none()
                    } else {
                        !e.record.collected
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
        Ok((!unresolved.is_empty()).then(|| format!("Before finishing this response, resolve these jobs: {}. Use job_output to read/wait, job_input to steer a running subagent or send stdin to a process, job_stop to cancel, or job_keep with an explicit reason if leaving background work running is intended. Do not start a replacement for an existing job. A soft wait is not a failure.", unresolved.join("; "))))
    }
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

/// Retains at most 128 captured jobs. Only acknowledged terminal records
/// are evicted; running or uncollected work can never disappear here.
fn admit_new_job(root: &Path, state: &mut State, owner: &str) -> Result<(), String> {
    while state.entries.len() >= 128 {
        let old = state
            .entries
            .iter()
            .find(|(_, e)| !e.record.snapshot.running && e.record.collected)
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
mod child_tests {
    use super::*;

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
}
