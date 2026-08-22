//! Bounded, environment-scrubbed process-group execution.

use std::{
    collections::{BTreeMap, VecDeque},
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use command_group::{CommandGroup, GroupChild};
#[cfg(unix)]
use command_group::{Signal, UnixChildExt};
use thiserror::Error;

const MAX_OUTPUT: usize = 256 * 1024;
const MAX_ARGUMENTS: usize = 4096;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 1024;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

/// Compatibility request retained for the early built-in adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRequest {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub timeout: Duration,
}

/// Complete hermetic command specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpecV1 {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub timeout: Duration,
    pub maximum_output_bytes: usize,
    pub cancellation_grace: Duration,
}

impl From<&ProcessRequest> for ProcessSpecV1 {
    fn from(value: &ProcessRequest) -> Self {
        Self {
            program: value.program.clone(),
            arguments: value.arguments.clone(),
            working_directory: value.working_directory.clone(),
            environment: BTreeMap::new(),
            timeout: value.timeout,
            maximum_output_bytes: MAX_OUTPUT,
            cancellation_grace: COOPERATIVE_GRACE,
        }
    }
}

/// Thread-safe cancellation control kept on a reserved control path.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessTermination {
    Exited,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// Exact lifecycle facts needed for conservative side-effect classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlledProcessResult {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub termination: ProcessTermination,
    pub process_group_id: u32,
    pub output_truncated: bool,
    pub tree_cleanup_attempted: bool,
}

pub struct ProcessRunner;

/// Replaceable platform process boundary. Native Windows/macOS/Linux adapters
/// can implement this without leaking OS process objects into tool code.
pub trait PlatformProcessPort: Send + Sync {
    fn health(&self) -> Result<PlatformProcessHealthV1, ProcessError>;

    fn execute(
        &self,
        request: &ProcessSpecV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ProcessError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformProcessHealthV1 {
    pub adapter: String,
    pub available: bool,
    pub process_tree_cleanup: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProcessPort;

impl PlatformProcessPort for NativeProcessPort {
    fn health(&self) -> Result<PlatformProcessHealthV1, ProcessError> {
        Ok(PlatformProcessHealthV1 {
            adapter: "native-command-group".to_owned(),
            available: true,
            process_tree_cleanup: true,
        })
    }

    fn execute(
        &self,
        request: &ProcessSpecV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ProcessError> {
        ProcessRunner::run_controlled(request, cancellation)
    }
}

/// Scripted, thread-safe platform conformance adapter used to verify lifecycle
/// logic without relying on a particular operating system.
#[derive(Clone, Default)]
pub struct HermeticProcessPort {
    state: Arc<Mutex<HermeticProcessState>>,
}

#[derive(Default)]
struct HermeticProcessState {
    scripted: VecDeque<HermeticProcessStep>,
    observed: Vec<ProcessSpecV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HermeticProcessStep {
    Result(ControlledProcessResult),
    LaunchFailure,
}

impl HermeticProcessPort {
    pub fn push(&self, step: HermeticProcessStep) -> Result<(), ProcessError> {
        self.state
            .lock()
            .map_err(|_| ProcessError::StateUnavailable)?
            .scripted
            .push_back(step);
        Ok(())
    }

    pub fn observed(&self) -> Result<Vec<ProcessSpecV1>, ProcessError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ProcessError::StateUnavailable)?
            .observed
            .clone())
    }
}

impl PlatformProcessPort for HermeticProcessPort {
    fn health(&self) -> Result<PlatformProcessHealthV1, ProcessError> {
        drop(
            self.state
                .lock()
                .map_err(|_| ProcessError::StateUnavailable)?,
        );
        Ok(PlatformProcessHealthV1 {
            adapter: "hermetic-script".to_owned(),
            available: true,
            process_tree_cleanup: true,
        })
    }

    fn execute(
        &self,
        request: &ProcessSpecV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ProcessError> {
        if cancellation.is_cancelled() {
            return Err(ProcessError::CancelledBeforeLaunch);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| ProcessError::StateUnavailable)?;
        state.observed.push(request.clone());
        match state.scripted.pop_front() {
            Some(HermeticProcessStep::Result(result)) => Ok(result),
            Some(HermeticProcessStep::LaunchFailure) => Err(ProcessError::HermeticLaunchFailure),
            None => Err(ProcessError::HermeticScriptExhausted),
        }
    }
}

impl ProcessRunner {
    pub fn run(request: &ProcessRequest) -> Result<ProcessResult, ProcessError> {
        let result =
            Self::run_controlled(&ProcessSpecV1::from(request), &CancellationToken::default())?;
        if result.output_truncated {
            return Err(ProcessError::OutputTooLarge);
        }
        Ok(ProcessResult {
            status: result.status,
            stdout: result.stdout,
            stderr: result.stderr,
            timed_out: result.termination == ProcessTermination::TimedOut,
        })
    }

    /// Executes one argv-only command in an independently killable process group.
    pub fn run_controlled(
        request: &ProcessSpecV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ProcessError> {
        validate_request(request)?;
        if cancellation.is_cancelled() {
            return Err(ProcessError::CancelledBeforeLaunch);
        }
        let mut command = Command::new(&request.program);
        command
            .args(&request.arguments)
            .env_clear()
            .envs(&request.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = &request.working_directory {
            command.current_dir(path);
        }
        let mut child = command.group_spawn()?;
        let process_group_id = child.id();
        let stdout = child
            .inner()
            .stdout
            .take()
            .ok_or(ProcessError::MissingPipe)?;
        let stderr = child
            .inner()
            .stderr
            .take()
            .ok_or(ProcessError::MissingPipe)?;
        let stdout_reader = spawn_reader(stdout, request.maximum_output_bytes);
        let stderr_reader = spawn_reader(stderr, request.maximum_output_bytes);
        let started = Instant::now();
        let (status, termination, cleanup) = loop {
            if let Some(status) = child.try_wait()? {
                break (status.code(), ProcessTermination::Exited, false);
            }
            if cancellation.is_cancelled() {
                let status = terminate_group(&mut child, request.cancellation_grace)?;
                break (status, ProcessTermination::Cancelled, true);
            }
            if started.elapsed() >= request.timeout {
                let status = terminate_group(&mut child, request.cancellation_grace)?;
                break (status, ProcessTermination::TimedOut, true);
            }
            thread::sleep(POLL_INTERVAL);
        };
        let mut stdout = stdout_reader
            .join()
            .map_err(|_| ProcessError::ReaderPanicked)??;
        let mut stderr = stderr_reader
            .join()
            .map_err(|_| ProcessError::ReaderPanicked)??;
        let total = stdout.bytes.len().saturating_add(stderr.bytes.len());
        if total > request.maximum_output_bytes {
            let stderr_limit = request
                .maximum_output_bytes
                .saturating_sub(stdout.bytes.len());
            stderr.bytes.truncate(stderr_limit);
            stderr.truncated = true;
            if stdout.bytes.len() > request.maximum_output_bytes {
                stdout.bytes.truncate(request.maximum_output_bytes);
                stdout.truncated = true;
                stderr.bytes.clear();
            }
        }
        let output_truncated = stdout.truncated || stderr.truncated;
        Ok(ControlledProcessResult {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            termination,
            process_group_id,
            output_truncated,
            tree_cleanup_attempted: cleanup,
        })
    }
}

fn validate_request(request: &ProcessSpecV1) -> Result<(), ProcessError> {
    if request.timeout.is_zero() {
        return Err(ProcessError::DeadlineElapsed);
    }
    if request.maximum_output_bytes == 0 || request.maximum_output_bytes > MAX_OUTPUT {
        return Err(ProcessError::InvalidOutputLimit);
    }
    if request.arguments.len() > MAX_ARGUMENTS
        || request.arguments.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
        || request.arguments.iter().any(|value| value.contains('\0'))
    {
        return Err(ProcessError::ArgumentTooLarge);
    }
    if request.environment.iter().any(|(key, value)| {
        key.is_empty()
            || key.contains(['=', '\0'])
            || value.contains('\0')
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }) {
        return Err(ProcessError::InvalidEnvironment);
    }
    if request.environment.len() > MAX_ENVIRONMENT_ENTRIES
        || request
            .environment
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .sum::<usize>()
            > MAX_ENVIRONMENT_BYTES
    {
        return Err(ProcessError::InvalidEnvironment);
    }
    if request
        .working_directory
        .as_ref()
        .is_some_and(|path| !path.is_dir())
    {
        return Err(ProcessError::InvalidWorkingDirectory);
    }
    Ok(())
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_reader(
    mut source: impl Read + Send + 'static,
    maximum: usize,
) -> thread::JoinHandle<Result<CapturedOutput, std::io::Error>> {
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(maximum.min(16 * 1024));
        let mut buffer = [0_u8; 8192];
        let mut truncated = false;
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let remaining = maximum.saturating_sub(bytes.len());
            let accepted = remaining.min(count);
            bytes.extend_from_slice(&buffer[..accepted]);
            truncated |= accepted != count;
        }
        Ok(CapturedOutput { bytes, truncated })
    })
}

fn terminate_group(child: &mut GroupChild, grace: Duration) -> Result<Option<i32>, ProcessError> {
    #[cfg(unix)]
    {
        let _ = child.signal(Signal::SIGTERM);
        let started = Instant::now();
        while started.elapsed() < grace {
            if let Some(status) = child.try_wait()? {
                return Ok(status.code());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    Ok(child.wait()?.code())
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("argument vector exceeds its bound")]
    ArgumentTooLarge,
    #[error("child environment is malformed")]
    InvalidEnvironment,
    #[error("working directory is unavailable")]
    InvalidWorkingDirectory,
    #[error("output bound is invalid")]
    InvalidOutputLimit,
    #[error("process output exceeds its bound")]
    OutputTooLarge,
    #[error("deadline elapsed before launch")]
    DeadlineElapsed,
    #[error("process was cancelled before launch")]
    CancelledBeforeLaunch,
    #[error("child pipe was not created")]
    MissingPipe,
    #[error("output reader thread failed")]
    ReaderPanicked,
    #[error("process adapter state is unavailable")]
    StateUnavailable,
    #[error("hermetic process adapter injected a launch failure")]
    HermeticLaunchFailure,
    #[error("hermetic process adapter script is exhausted")]
    HermeticScriptExhausted,
}
