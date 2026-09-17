//! Durable identities and cursors, with bounded in-memory native sessions.
use aworkit_capability_host::{
    CancellationToken, ProcessOutputCursor, ProcessSession, ProcessSnapshot, ProcessSpecV1,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: String,
    owner: String,
    invocation: String,
    command: Vec<String>,
    snapshot: ProcessSnapshot,
    cursor: ProcessOutputCursor,
    collected: bool,
    kept: Option<String>,
    interrupted: bool,
}
struct Entry {
    record: Record,
    session: Option<Arc<ProcessSession>>,
    cancellation: CancellationToken,
}
struct State {
    connection: Connection,
    entries: BTreeMap<String, Entry>,
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
                    record.snapshot.error = Some("Aworkit restarted; the previous process session was interrupted and will not be replayed. Cleanup cannot be reconstructed from a PID.".into());
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
                        session: None,
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
            if let Some(session) = &entry.session {
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
                    entry.session = None;
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
        if let Ok(state) = self.state.lock() {
            for entry in state
                .entries
                .values()
                .filter(|e| e.record.owner == owner && e.record.kept.is_none())
            {
                if let Some(session) = &entry.session {
                    session.stop();
                }
            }
        }
    }

    pub fn stop_all(&self, owner: &str) {
        if let Ok(state) = self.state.lock() {
            for entry in state.entries.values().filter(|e| e.record.owner == owner) {
                if let Some(session) = &entry.session {
                    session.stop();
                }
            }
        }
    }

    pub fn start(
        &self,
        owner: &str,
        invocation: &str,
        spec: &ProcessSpecV1,
        interactive: bool,
        cancellation: CancellationToken,
    ) -> Result<String, String> {
        self.refresh()?;
        let id = format!(
            "job.{:x}",
            Sha256::digest(format!("{owner}\0{invocation}").as_bytes())
        );
        let mut state = self.state.lock().map_err(err)?;
        if state.entries.contains_key(&id) {
            return Ok(id);
        }
        // Retain at most 128 captured jobs. Only acknowledged terminal records
        // are evicted; running or uncollected work can never disappear here.
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
            let path = self.root.join(&old);
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
                "running job capacity reached (8 per Chat, 32 total); stop or finish a job first"
                    .into(),
            );
        }
        if cancellation.is_cancelled() {
            return Err("job launch cancelled".into());
        }
        let mut record = Record {
            id: id.clone(),
            owner: owner.into(),
            invocation: invocation.into(),
            command: spec.arguments.clone(),
            snapshot: ProcessSnapshot {
                running: true,
                ..Default::default()
            },
            cursor: Default::default(),
            collected: false,
            kept: None,
            interrupted: false,
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
                session,
                cancellation,
            },
        );
        persisted?;
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
        let (session, record) = {
            let state = self.state.lock().map_err(err)?;
            let entry = owned(&state, owner, id)?;
            (entry.session.clone(), entry.record.clone())
        };
        let cursor = cursor.unwrap_or(record.cursor);
        let stdout_size =
            std::fs::metadata(self.root.join(id).join("stdout.log")).map_or(0, |m| m.len());
        let stderr_size =
            std::fs::metadata(self.root.join(id).join("stderr.log")).map_or(0, |m| m.len());
        if cursor.stdout > stdout_size || cursor.stderr > stderr_size {
            return Err("output cursor exceeds captured output".into());
        }
        let (snapshot, stdout, stderr, next, more) = if let Some(session) = session {
            let out = session
                .output(cursor, maximum, wait, cancellation)
                .map_err(err)?;
            (out.snapshot, out.stdout, out.stderr, out.next, out.more)
        } else {
            let stdout = read(
                &self.root.join(id).join("stdout.log"),
                cursor.stdout,
                maximum,
            )?;
            let stderr = read(
                &self.root.join(id).join("stderr.log"),
                cursor.stderr,
                maximum.saturating_sub(stdout.len()),
            )?;
            let next = ProcessOutputCursor {
                stdout: cursor.stdout + stdout.len() as u64,
                stderr: cursor.stderr + stderr.len() as u64,
            };
            let more = next.stdout < record.snapshot.stdout_bytes
                || next.stderr < record.snapshot.stderr_bytes;
            (record.snapshot.clone(), stdout, stderr, next, more)
        };
        let mut state = self.state.lock().map_err(err)?;
        let entry = state.entries.get_mut(id).ok_or("job unavailable")?;
        entry.record.snapshot = snapshot.clone();
        let record = entry.record.clone();
        Ok(
            json!({"jobId":id,"running":snapshot.running,"status":if record.interrupted {"interrupted"} else if snapshot.running {"running"} else if snapshot.stopped {"stopped"} else if snapshot.error.is_some() {"failed"} else {"exited"},"exitCode":snapshot.exit_code,"rootExited":snapshot.root_exited,"treeEmpty":snapshot.tree_empty,"elapsedMs":snapshot.elapsed_ms,"stdout":String::from_utf8_lossy(&stdout),"stderr":String::from_utf8_lossy(&stderr),"cursor":next,"moreOutput":more,"error":snapshot.error,"inputError":snapshot.input_error,"kept":record.kept,"outputDirectory":self.root.join(id)}),
        )
    }

    pub fn control(
        &self,
        owner: &str,
        operation: &str,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        self.refresh()?;
        if operation == "job_list" {
            return self.list(owner);
        }
        let id = args["jobId"].as_str().ok_or("jobId missing")?;
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
        let session = {
            let state = self.state.lock().map_err(err)?;
            owned(&state, owner, id)?.session.clone()
        };
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
                let mut state = self.state.lock().map_err(err)?;
                let entry = state.entries.get_mut(id).ok_or("job unavailable")?;
                entry.record.kept = Some(reason.to_owned());
                let record = entry.record.clone();
                save(&state.connection, &record)?;
                Ok(
                    json!({"jobId":id,"kept":true,"reason":reason,"lifetime":"until stopped or Aworkit exits"}),
                )
            }
            _ => Err("unknown job operation".into()),
        }
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

    fn list(&self, owner: &str) -> Result<Value, String> {
        let state = self.state.lock().map_err(err)?;
        Ok(
            json!({"jobs":state.entries.values().filter(|e| e.record.owner == owner).map(|e| json!({"jobId":e.record.id,"running":e.record.snapshot.running,"exitCode":e.record.snapshot.exit_code,"error":e.record.snapshot.error,"collected":e.record.collected,"kept":e.record.kept,"cursor":e.record.cursor})).collect::<Vec<_>>()}),
        )
    }

    /// A runtime gate, independent of model promises to clean up later.
    pub fn completion_notice(&self, owner: &str) -> Result<Option<String>, String> {
        self.refresh()?;
        let state = self.state.lock().map_err(err)?;
        let unresolved = state
            .entries
            .values()
            .filter(|e| {
                e.record.owner == owner
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
        Ok((!unresolved.is_empty()).then(|| format!("Before finishing this response, resolve these process jobs: {}. Use job_output to read/wait, job_input to send stdin, job_stop to stop the process tree, or job_keep with an explicit reason if leaving a service running is intended. Do not start a replacement for an existing job. A soft wait is not a failure.", unresolved.join("; "))))
    }
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
