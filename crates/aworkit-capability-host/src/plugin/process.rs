//! Supervised native subprocess transport for the framed plugin protocol.

use std::{
    io::{Read, Write},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use aworkit_process::identity::ExecutableIdentityV1;
use command_group::{CommandGroup, GroupChild};
#[cfg(unix)]
use command_group::{Signal, UnixChildExt};
use thiserror::Error;

use crate::{CancellationToken, ProcessSpecV1};

use super::{PluginFrameCodecV1, PluginFrameError, PluginProtocolFrameV1};

const MAXIMUM_ARGUMENTS: usize = 4096;
const MAXIMUM_ARGUMENT_BYTES: usize = 256 * 1024;
const MAXIMUM_ENVIRONMENT_ENTRIES: usize = 1024;
const MAXIMUM_ENVIRONMENT_BYTES: usize = 256 * 1024;
const MAXIMUM_DIAGNOSTIC_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginProcessLimitsV1 {
    pub maximum_queued_frames: usize,
    pub maximum_stderr_bytes: usize,
}

impl Default for PluginProcessLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_queued_frames: 32,
            maximum_stderr_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginProcessDiagnosticsV1 {
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginProcessExitV1 {
    pub exit_code: Option<i32>,
    pub forced: bool,
    pub tree_cleanup_attempted: bool,
}

enum ReaderMessageV1 {
    Frame(PluginProtocolFrameV1),
    Closed(Option<String>),
}

#[derive(Default)]
struct DiagnosticStateV1 {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Persistent plugin session with bounded stdout frames, bounded diagnostics,
/// reserved direct control writes, and descendant-tree cleanup.
pub struct NativePluginProcessV1 {
    child: Option<GroupChild>,
    stdin: Option<ChildStdin>,
    receiver: mpsc::Receiver<ReaderMessageV1>,
    stdout_reader: Option<thread::JoinHandle<()>>,
    diagnostics: Arc<Mutex<DiagnosticStateV1>>,
    stderr_reader: Option<thread::JoinHandle<()>>,
    codec: PluginFrameCodecV1,
    termination_grace: Duration,
}

impl NativePluginProcessV1 {
    /// Launches an argv-only process group. The specification is the same
    /// environment-scrubbed contract used by built-in process execution.
    pub fn spawn(
        spec: &ProcessSpecV1,
        codec: PluginFrameCodecV1,
        limits: PluginProcessLimitsV1,
    ) -> Result<Self, PluginProcessError> {
        validate_spec(spec, limits)?;
        let executable_path = std::fs::canonicalize(&spec.program)
            .map_err(|_| PluginProcessError::ExecutableIdentityMismatch)?;
        let executable = ExecutableIdentityV1::open(&executable_path)
            .map_err(|_| PluginProcessError::ExecutableIdentityMismatch)?;
        let mut command = Command::new(&executable.canonical_path);
        command
            .args(&spec.arguments)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &spec.working_directory {
            command.current_dir(std::fs::canonicalize(directory)?);
        }
        let mut child = command.group_spawn()?;
        if !ExecutableIdentityV1::open(&executable.canonical_path)
            .is_ok_and(|observed| observed == executable)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PluginProcessError::ExecutableIdentityMismatch);
        }
        let stdin = child
            .inner()
            .stdin
            .take()
            .ok_or(PluginProcessError::MissingPipe)?;
        let stdout = child
            .inner()
            .stdout
            .take()
            .ok_or(PluginProcessError::MissingPipe)?;
        let stderr = child
            .inner()
            .stderr
            .take()
            .ok_or(PluginProcessError::MissingPipe)?;

        let (sender, receiver) = mpsc::sync_channel(limits.maximum_queued_frames);
        let stdout_reader = thread::spawn(move || read_stdout(stdout, codec, &sender));
        let diagnostics = Arc::new(Mutex::new(DiagnosticStateV1::default()));
        let diagnostic_writer = Arc::clone(&diagnostics);
        let stderr_reader = thread::spawn(move || {
            read_stderr(stderr, limits.maximum_stderr_bytes, &diagnostic_writer);
        });

        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            receiver,
            stdout_reader: Some(stdout_reader),
            diagnostics,
            stderr_reader: Some(stderr_reader),
            codec,
            termination_grace: spec.cancellation_grace,
        })
    }

    /// Sends one already validated protocol frame. Cancellation and shutdown
    /// use this same direct write path and do not compete with event queues.
    pub fn send(&mut self, frame: &PluginProtocolFrameV1) -> Result<(), PluginProcessError> {
        self.ensure_running()?;
        let encoded = self.codec.encode(frame)?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(PluginProcessError::AlreadyStopped)?;
        stdin.write_all(&encoded)?;
        stdin.flush()?;
        Ok(())
    }

    pub fn receive(&self, timeout: Duration) -> Result<PluginProtocolFrameV1, PluginProcessError> {
        if timeout.is_zero() {
            return Err(PluginProcessError::ReceiveTimeout);
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(ReaderMessageV1::Frame(frame)) => Ok(frame),
            Ok(ReaderMessageV1::Closed(None)) => Err(PluginProcessError::ProtocolStreamClosed),
            Ok(ReaderMessageV1::Closed(Some(reason))) => {
                Err(PluginProcessError::ProtocolStreamFailed(reason))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(PluginProcessError::ReceiveTimeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(PluginProcessError::ProtocolStreamClosed)
            }
        }
    }

    pub fn diagnostics(&self) -> Result<PluginProcessDiagnosticsV1, PluginProcessError> {
        let value = self
            .diagnostics
            .lock()
            .map_err(|_| PluginProcessError::StateUnavailable)?;
        Ok(PluginProcessDiagnosticsV1 {
            stderr: value.bytes.clone(),
            truncated: value.truncated,
        })
    }

    pub fn try_wait(&mut self) -> Result<Option<PluginProcessExitV1>, PluginProcessError> {
        let Some(child) = self.child.as_mut() else {
            return Err(PluginProcessError::AlreadyStopped);
        };
        let Some(status) = child.try_wait()? else {
            return Ok(None);
        };
        self.stdin.take();
        Ok(Some(PluginProcessExitV1 {
            exit_code: status.code(),
            forced: false,
            tree_cleanup_attempted: false,
        }))
    }

    /// Waits for a clean protocol-requested shutdown and force-cleans the
    /// descendant group when the deadline expires.
    pub fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> Result<PluginProcessExitV1, PluginProcessError> {
        if timeout.is_zero() {
            return self.terminate_tree();
        }
        let started = Instant::now();
        loop {
            if let Some(exit) = self.try_wait()? {
                self.join_readers();
                return Ok(exit);
            }
            if started.elapsed() >= timeout {
                return self.terminate_tree();
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn terminate_if_cancelled(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<PluginProcessExitV1>, PluginProcessError> {
        if cancellation.is_cancelled() {
            self.terminate_tree().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn terminate_tree(&mut self) -> Result<PluginProcessExitV1, PluginProcessError> {
        self.stdin.take();
        let Some(mut child) = self.child.take() else {
            return Err(PluginProcessError::AlreadyStopped);
        };
        #[cfg(not(unix))]
        let _ = self.termination_grace; // Windows has no graceful signal phase.
        #[cfg(unix)]
        {
            let _ = child.signal(Signal::SIGTERM);
            let started = Instant::now();
            while started.elapsed() < self.termination_grace {
                if let Some(status) = child.try_wait()? {
                    self.join_readers();
                    return Ok(PluginProcessExitV1 {
                        exit_code: status.code(),
                        forced: false,
                        tree_cleanup_attempted: true,
                    });
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        let status = child.wait()?;
        self.join_readers();
        Ok(PluginProcessExitV1 {
            exit_code: status.code(),
            forced: true,
            tree_cleanup_attempted: true,
        })
    }

    fn ensure_running(&mut self) -> Result<(), PluginProcessError> {
        if self.try_wait()?.is_some() {
            Err(PluginProcessError::AlreadyStopped)
        } else {
            Ok(())
        }
    }

    fn join_readers(&mut self) {
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for NativePluginProcessV1 {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.terminate_tree();
        } else {
            self.join_readers();
        }
    }
}

fn read_stdout(
    mut stdout: impl Read,
    codec: PluginFrameCodecV1,
    sender: &mpsc::SyncSender<ReaderMessageV1>,
) {
    let mut decoder = codec.decoder();
    let mut chunk = [0_u8; 8192];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => {
                let result = decoder.finish().err().map(|error| error.to_string());
                let _ = sender.try_send(ReaderMessageV1::Closed(result));
                return;
            }
            Ok(count) => match decoder.push(&chunk[..count]) {
                Ok(frames) => {
                    for frame in frames {
                        if sender.try_send(ReaderMessageV1::Frame(frame)).is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.try_send(ReaderMessageV1::Closed(Some(error.to_string())));
                    return;
                }
            },
            Err(error) => {
                let _ = sender.try_send(ReaderMessageV1::Closed(Some(error.to_string())));
                return;
            }
        }
    }
}

fn read_stderr(mut stderr: impl Read, maximum: usize, diagnostics: &Arc<Mutex<DiagnosticStateV1>>) {
    let mut chunk = [0_u8; 4096];
    loop {
        let Ok(count) = stderr.read(&mut chunk) else {
            return;
        };
        if count == 0 {
            return;
        }
        let Ok(mut value) = diagnostics.lock() else {
            return;
        };
        let remaining = maximum.saturating_sub(value.bytes.len());
        let accepted = remaining.min(count);
        value.bytes.extend_from_slice(&chunk[..accepted]);
        value.truncated |= accepted != count;
    }
}

fn validate_spec(
    spec: &ProcessSpecV1,
    limits: PluginProcessLimitsV1,
) -> Result<(), PluginProcessError> {
    if spec.program.as_os_str().is_empty()
        || !spec.program.is_absolute()
        || spec.timeout.is_zero()
        || spec.cancellation_grace.is_zero()
        || limits.maximum_queued_frames == 0
        || limits.maximum_stderr_bytes == 0
        || limits.maximum_stderr_bytes > MAXIMUM_DIAGNOSTIC_BYTES
    {
        return Err(PluginProcessError::InvalidSpecification);
    }
    if spec.arguments.len() > MAXIMUM_ARGUMENTS
        || spec.arguments.iter().map(String::len).sum::<usize>() > MAXIMUM_ARGUMENT_BYTES
        || spec.arguments.iter().any(|value| value.contains('\0'))
    {
        return Err(PluginProcessError::InvalidSpecification);
    }
    if spec.environment.len() > MAXIMUM_ENVIRONMENT_ENTRIES
        || spec
            .environment
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .sum::<usize>()
            > MAXIMUM_ENVIRONMENT_BYTES
        || spec.environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains(['=', '\0'])
                || value.contains('\0')
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        return Err(PluginProcessError::InvalidSpecification);
    }
    if spec
        .working_directory
        .as_ref()
        .is_some_and(|directory| !directory.is_dir())
    {
        return Err(PluginProcessError::InvalidSpecification);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum PluginProcessError {
    #[error("plugin process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("plugin process frame failed validation: {0}")]
    Frame(#[from] PluginFrameError),
    #[error("plugin process specification or bounds are invalid")]
    InvalidSpecification,
    #[error("plugin executable is not an exact stable absolute identity")]
    ExecutableIdentityMismatch,
    #[error("plugin process did not expose all protocol pipes")]
    MissingPipe,
    #[error("plugin process protocol receive deadline elapsed")]
    ReceiveTimeout,
    #[error("plugin process protocol stream closed without a terminal frame")]
    ProtocolStreamClosed,
    #[error("plugin process protocol stream failed: {0}")]
    ProtocolStreamFailed(String),
    #[error("plugin process lifecycle state is unavailable")]
    StateUnavailable,
    #[error("plugin process has already stopped")]
    AlreadyStopped,
}
