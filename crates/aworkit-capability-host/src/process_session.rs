//! Supervised process sessions. Soft waits never own or terminate the process.
//! Output is spooled to files, so inherited pipe handles cannot block collection.
use crate::{CancellationToken, ProcessError, ProcessSpecV1};
use aworkit_process::identity::ExecutableIdentityV1;
use aworkit_process_platform::ProcessTree;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(20);
const SPOOL_LIMIT: u64 = 64 * 1024 * 1024;

/// Persistable lifecycle facts; root exit alone does not mean the tree exited.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub process_id: u32,
    pub running: bool,
    pub root_exited: bool,
    pub tree_empty: bool,
    pub exit_code: Option<i32>,
    pub stopped: bool,
    pub elapsed_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub error: Option<String>,
    pub input_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessOutputCursor {
    pub stdout: u64,
    pub stderr: u64,
}

pub struct ProcessOutput {
    pub snapshot: ProcessSnapshot,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub next: ProcessOutputCursor,
    pub more: bool,
}

/// Owned handle; dropping it requests cleanup without waiting for blocked I/O.
pub struct ProcessSession {
    state: Arc<Mutex<ProcessSnapshot>>,
    stop: CancellationToken,
    input: Mutex<Option<SyncSender<Vec<u8>>>>,
    directory: PathBuf,
}

impl ProcessSession {
    /// Starts inside a newly created private job directory. No shell or inherited environment is added here.
    pub fn start(
        spec: &ProcessSpecV1,
        directory: &Path,
        interactive: bool,
    ) -> Result<Self, ProcessError> {
        let (mut command, identity) = crate::process::prepare_command(spec)?;
        std::fs::create_dir_all(directory)?;
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("stdout.log"))?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("stderr.log"))?;
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        command.stdin(if interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut tree = ProcessTree::spawn(&mut command)?;
        if !ExecutableIdentityV1::open(&identity.canonical_path)
            .is_ok_and(|current| current == identity)
        {
            let _ = tree.terminate();
            return Err(ProcessError::ExecutableIdentityMismatch);
        }
        let state = Arc::new(Mutex::new(ProcessSnapshot {
            process_id: tree.child().id(),
            running: true,
            ..Default::default()
        }));
        let stop = CancellationToken::default();
        let input = tree.child().stdin.take().map(|mut stdin| {
            let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(4);
            let state = Arc::clone(&state);
            thread::spawn(move || {
                for bytes in receiver {
                    if let Err(error) = stdin.write_all(&bytes).and_then(|()| stdin.flush()) {
                        if let Ok(mut state) = state.lock() {
                            state.input_error = Some(error.to_string());
                        }
                        break;
                    }
                }
            });
            sender
        });
        let monitor_state = Arc::clone(&state);
        let monitor_stop = stop.clone();
        let monitor_directory = directory.to_owned();
        thread::Builder::new()
            .name("aworkit-process-monitor".into())
            .spawn(move || {
                monitor(tree, monitor_state, monitor_stop, &monitor_directory);
            })?;
        Ok(Self {
            state,
            stop,
            input: Mutex::new(input),
            directory: directory.to_owned(),
        })
    }

    pub fn snapshot(&self) -> Result<ProcessSnapshot, ProcessError> {
        self.state
            .lock()
            .map(|s| s.clone())
            .map_err(|_| ProcessError::StateUnavailable)
    }

    /// Returns new bounded output immediately, or waits for output/exit up to the soft deadline.
    pub fn output(
        &self,
        cursor: ProcessOutputCursor,
        maximum: usize,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let started = Instant::now();
        let maximum = maximum.clamp(1, 256 * 1024);
        let snapshot = loop {
            let snapshot = self.snapshot()?;
            if !snapshot.running
                || snapshot.stdout_bytes > cursor.stdout
                || snapshot.stderr_bytes > cursor.stderr
                || started.elapsed() >= wait.min(Duration::from_secs(60))
                || cancellation.is_cancelled()
            {
                break snapshot;
            }
            thread::sleep(POLL);
        };
        let stdout = read_output(&self.directory.join("stdout.log"), cursor.stdout, maximum)?;
        let stderr = read_output(
            &self.directory.join("stderr.log"),
            cursor.stderr,
            maximum.saturating_sub(stdout.len()),
        )?;
        let next = ProcessOutputCursor {
            stdout: cursor.stdout + stdout.len() as u64,
            stderr: cursor.stderr + stderr.len() as u64,
        };
        let more = next.stdout < snapshot.stdout_bytes || next.stderr < snapshot.stderr_bytes;
        Ok(ProcessOutput {
            snapshot,
            stdout,
            stderr,
            next,
            more,
        })
    }

    /// Queues bounded stdin without blocking the control path. Closing stdin is ordered after queued input.
    pub fn input(&self, bytes: Vec<u8>, close: bool) -> Result<(), ProcessError> {
        if bytes.len() > 16 * 1024 {
            return Err(ProcessError::ArgumentTooLarge);
        }
        if !self.snapshot()?.running {
            return Err(std::io::Error::other("job has exited").into());
        }
        let mut input = self
            .input
            .lock()
            .map_err(|_| ProcessError::StateUnavailable)?;
        let sender = input
            .as_ref()
            .ok_or_else(|| std::io::Error::other("stdin is closed or unavailable"))?;
        if !bytes.is_empty() {
            sender
                .try_send(bytes)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        if close {
            input.take();
        }
        Ok(())
    }

    pub fn stop(&self) {
        self.stop.cancel();
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
    }
}

impl Drop for ProcessSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_output(path: &Path, offset: u64, maximum: usize) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut result = Vec::new();
    file.take(maximum as u64).read_to_end(&mut result)?;
    Ok(result)
}

fn monitor(
    mut tree: ProcessTree,
    state: Arc<Mutex<ProcessSnapshot>>,
    stop: CancellationToken,
    directory: &Path,
) {
    let started = Instant::now();
    let mut terminating = None;
    loop {
        let Ok(mut snapshot) = state.lock() else {
            let _ = tree.terminate();
            return;
        };
        snapshot.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        snapshot.stdout_bytes =
            std::fs::metadata(directory.join("stdout.log")).map_or(0, |m| m.len());
        snapshot.stderr_bytes =
            std::fs::metadata(directory.join("stderr.log")).map_or(0, |m| m.len());
        match tree.child().try_wait() {
            Ok(Some(status)) => {
                snapshot.root_exited = true;
                snapshot.exit_code = status.code();
            }
            Ok(None) => {}
            Err(error) => snapshot.error = Some(error.to_string()),
        }
        match tree.is_running() {
            Ok(false) => {
                snapshot.stdout_bytes =
                    std::fs::metadata(directory.join("stdout.log")).map_or(0, |m| m.len());
                snapshot.stderr_bytes =
                    std::fs::metadata(directory.join("stderr.log")).map_or(0, |m| m.len());
                snapshot.running = false;
                snapshot.tree_empty = true;
                return;
            }
            Ok(true) => {}
            Err(error) => snapshot.error = Some(error.to_string()),
        }
        if snapshot.stdout_bytes.saturating_add(snapshot.stderr_bytes) >= SPOOL_LIMIT {
            snapshot.error = Some("job exceeded the 64 MiB output resource limit".into());
        }
        if terminating.is_none() && (stop.is_cancelled() || snapshot.error.is_some()) {
            snapshot.stopped = stop.is_cancelled();
            if let Err(error) = tree.terminate() {
                snapshot.error = Some(format!("process-tree cleanup failed: {error}"));
            }
            terminating = Some(Instant::now());
        }
        if terminating.is_some_and(|time| time.elapsed() >= Duration::from_secs(2)) {
            snapshot.running = false;
            snapshot.error =
                Some("process-tree cleanup could not be verified within two seconds".into());
            return;
        }
        drop(snapshot);
        thread::sleep(POLL);
    }
}
