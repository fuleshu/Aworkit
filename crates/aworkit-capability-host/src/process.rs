//! Bounded, environment-scrubbed process-group execution.

use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use aworkit_process::identity::ExecutableIdentityV1;
use thiserror::Error;

const MAX_OUTPUT: usize = 256 * 1024;
const MAX_ARGUMENTS: usize = 4096;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 1024;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
const COOPERATIVE_GRACE: Duration = Duration::from_millis(100);

/// Compatibility request retained for the early built-in adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRequest {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub timeout: Duration,
}

/// Explicit command environment, plus host PATH and the Windows OS baseline.
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
        let directory = tempfile::tempdir()?;
        if cancellation.is_cancelled() {
            return Err(ProcessError::CancelledBeforeLaunch);
        }
        let session = crate::ProcessSession::start(request, directory.path(), false)?;
        let started = Instant::now();
        let mut termination = ProcessTermination::Exited;
        loop {
            let snapshot = session.snapshot()?;
            if !snapshot.running {
                break;
            }
            if cancellation.is_cancelled() {
                termination = ProcessTermination::Cancelled;
                session.stop();
            } else if started.elapsed() >= request.timeout {
                termination = ProcessTermination::TimedOut;
                session.stop();
            }
            thread::sleep(Duration::from_millis(5));
        }
        let output = session.output(
            crate::ProcessOutputCursor::default(),
            request.maximum_output_bytes,
            Duration::ZERO,
            cancellation,
        )?;
        if let Some(error) = output.snapshot.error {
            return Err(ProcessError::Supervision(error));
        }
        Ok(ControlledProcessResult {
            status: output.snapshot.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            termination,
            process_group_id: output.snapshot.process_id,
            output_truncated: output.more,
            tree_cleanup_attempted: termination != ProcessTermination::Exited,
        })
    }
}

pub(crate) fn prepare_command(
    request: &ProcessSpecV1,
) -> Result<(Command, ExecutableIdentityV1), ProcessError> {
    validate_request(request)?;
    let executable_path = std::fs::canonicalize(&request.program)
        .map_err(|_| ProcessError::ExecutableIdentityMismatch)?;
    let executable = ExecutableIdentityV1::open(&executable_path)
        .map_err(|_| ProcessError::ExecutableIdentityMismatch)?;
    let mut command = Command::new(&executable.canonical_path);
    command.env_clear();
    // Host commands and their descendants must be able to find installed tools.
    // Keep executable discovery without copying credentials or unrelated app state.
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    // Winsock name resolution needs SystemRoot even for an absolute executable,
    // and command text that names a machine directory needs the rest of the
    // machine-scoped Windows baseline: an undefined `%NAME%` stays verbatim in
    // cmd.exe, so `%SystemDrive%` would otherwise resolve to a *relative* path
    // and create a literal '%SystemDrive%' folder in the working directory.
    // Keep the OS baseline without inheriting credentials, user environment or
    // app variables.
    #[cfg(windows)]
    crate::shell::apply_machine_environment(&mut command);
    crate::shell::command_arguments(&mut command, &executable.canonical_path, &request.arguments);
    command.envs(&request.environment);
    if let Some(path) = &request.working_directory {
        command.current_dir(std::fs::canonicalize(path)?);
    }

    Ok((command, executable))
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
    if !request.program.is_absolute() {
        return Err(ProcessError::ExecutableIdentityMismatch);
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

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("process supervision failed: {0}")]
    Supervision(String),
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
    #[error("process executable is not an exact stable absolute identity")]
    ExecutableIdentityMismatch,
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
