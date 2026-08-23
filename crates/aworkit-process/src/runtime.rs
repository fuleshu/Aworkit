//! Sanitized native process-tree spawning and cleanup.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use aworkit_protocol::ProcessGeneration;
use command_group::{CommandGroup, GroupChild};
#[cfg(unix)]
use command_group::{Signal, UnixChildExt};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{identity::ExecutableIdentityV1, time::MonotonicDeadline};

const MAX_ARGUMENTS: usize = 4096;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 1024;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessContainmentV1 {
    WindowsJobObject,
    PosixProcessGroup,
    Unsupported,
}

/// Fresh, truthful process/IPC capabilities for the compiled platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessCapabilityReportV1 {
    pub containment: ProcessContainmentV1,
    pub authenticated_local_ipc: bool,
    pub peer_process_identity: bool,
    pub peer_user_identity: bool,
    pub exact_executable_identity: bool,
    pub complete_tree_cleanup: bool,
    pub monotonic_deadlines: bool,
    pub detached_helper_survival: bool,
    pub verification_only_launch: bool,
}

impl NativeProcessCapabilityReportV1 {
    #[must_use]
    pub const fn current() -> Self {
        let supported_platform = cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        ));
        Self {
            containment: ProcessContainmentV1::current(),
            authenticated_local_ipc: supported_platform,
            peer_process_identity: cfg!(any(target_os = "windows", target_os = "linux")),
            peer_user_identity: supported_platform,
            exact_executable_identity: supported_platform,
            complete_tree_cleanup: supported_platform,
            monotonic_deadlines: true,
            detached_helper_survival: supported_platform,
            verification_only_launch: supported_platform,
        }
    }

    #[must_use]
    pub const fn supports_generation_supervision(&self) -> bool {
        !matches!(self.containment, ProcessContainmentV1::Unsupported)
            && self.authenticated_local_ipc
            && (self.peer_process_identity || self.peer_user_identity)
            && self.exact_executable_identity
            && self.complete_tree_cleanup
            && self.monotonic_deadlines
            && self.detached_helper_survival
            && self.verification_only_launch
    }
}

impl ProcessContainmentV1 {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::WindowsJobObject
        } else if cfg!(unix) {
            Self::PosixProcessGroup
        } else {
            Self::Unsupported
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SanitizedProcessSpecV1 {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub process_generation: ProcessGeneration,
    pub role: String,
    pub verification_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessTreeHandleV1 {
    pub root_process_id: u32,
    pub process_generation: ProcessGeneration,
    pub executable: ExecutableIdentityV1,
    pub containment: ProcessContainmentV1,
    pub containment_identity_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessTreeCleanupEvidenceV1 {
    pub process_generation: ProcessGeneration,
    pub cooperative_requested: bool,
    pub forced_termination_used: bool,
    pub descendants_observed: Vec<u32>,
    pub tree_empty: bool,
    pub orphan_risk: bool,
    pub completed_in: Duration,
}

struct ProcessTreeRecord {
    handle: NativeProcessTreeHandleV1,
    child: GroupChild,
    cooperative_requested: bool,
    forced_termination_used: bool,
    descendants: BTreeSet<u32>,
}

#[derive(Default)]
struct RegistryState {
    processes: BTreeMap<u64, ProcessTreeRecord>,
}

/// Generation-keyed owner of exact process-group or Job handles.
#[derive(Default)]
pub struct NativeProcessRegistry {
    state: Mutex<RegistryState>,
}

impl NativeProcessRegistry {
    pub fn spawn_tree(
        &self,
        spec: &SanitizedProcessSpecV1,
    ) -> Result<NativeProcessTreeHandleV1, NativeProcessError> {
        validate_spec(spec)?;
        let containment = ProcessContainmentV1::current();
        if containment == ProcessContainmentV1::Unsupported {
            return Err(NativeProcessError::ContainmentUnavailable);
        }
        let mut state = self.state.lock().expect("native process registry lock");
        if state.processes.contains_key(&spec.process_generation.0) {
            return Err(NativeProcessError::GenerationAlreadyLive);
        }
        let executable = ExecutableIdentityV1::open(&spec.executable)?;
        let working_directory = canonical_directory(&spec.working_directory)?;
        let mut command = std::process::Command::new(&executable.canonical_path);
        command
            .args(&spec.arguments)
            .current_dir(working_directory)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.group_spawn()?;
        if !ExecutableIdentityV1::open(&executable.canonical_path)
            .is_ok_and(|observed| observed == executable)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NativeProcessError::Identity(
                crate::identity::IdentityError::ExecutableChanged,
            ));
        }
        let root_process_id = child.id();
        let containment_identity_hash = format!(
            "sha256:{:x}",
            Sha256::digest(format!(
                "{}:{}:{}:{}",
                root_process_id, spec.process_generation.0, executable.content_hash, spec.role
            ))
        );
        let handle = NativeProcessTreeHandleV1 {
            root_process_id,
            process_generation: spec.process_generation,
            executable,
            containment,
            containment_identity_hash,
        };
        state.processes.insert(
            spec.process_generation.0,
            ProcessTreeRecord {
                handle: handle.clone(),
                child,
                cooperative_requested: false,
                forced_termination_used: false,
                descendants: BTreeSet::from([root_process_id]),
            },
        );
        Ok(handle)
    }

    pub fn request_cooperative_shutdown(
        &self,
        generation: ProcessGeneration,
    ) -> Result<(), NativeProcessError> {
        let mut state = self.state.lock().expect("native process registry lock");
        let record = state
            .processes
            .get_mut(&generation.0)
            .ok_or(NativeProcessError::UnknownGeneration)?;
        record.cooperative_requested = true;
        record
            .descendants
            .extend(enumerate_descendants(record.handle.root_process_id));
        #[cfg(unix)]
        record.child.signal(Signal::SIGTERM)?;
        Ok(())
    }

    pub fn await_exit(
        &self,
        generation: ProcessGeneration,
        timeout: Duration,
    ) -> Result<Option<ExitStatus>, NativeProcessError> {
        let deadline =
            MonotonicDeadline::after(timeout).map_err(|_| NativeProcessError::Deadline)?;
        loop {
            let status = {
                let mut state = self.state.lock().expect("native process registry lock");
                let record = state
                    .processes
                    .get_mut(&generation.0)
                    .ok_or(NativeProcessError::UnknownGeneration)?;
                record
                    .descendants
                    .extend(enumerate_descendants(record.handle.root_process_id));
                record.child.try_wait()?
            };
            if status.is_some() {
                return Ok(status);
            }
            if deadline.expired() {
                return Ok(None);
            }
            thread::sleep(POLL_INTERVAL.min(deadline.remaining()));
        }
    }

    pub fn force_cleanup(
        &self,
        generation: ProcessGeneration,
        timeout: Duration,
    ) -> Result<ProcessTreeCleanupEvidenceV1, NativeProcessError> {
        let started = Instant::now();
        {
            let mut state = self.state.lock().expect("native process registry lock");
            let record = state
                .processes
                .get_mut(&generation.0)
                .ok_or(NativeProcessError::UnknownGeneration)?;
            record
                .descendants
                .extend(enumerate_descendants(record.handle.root_process_id));
            if record.child.try_wait()?.is_none() {
                record.child.kill()?;
                record.forced_termination_used = true;
            }
        }
        let status = self.await_exit(generation, timeout)?;
        self.cleanup_evidence(generation, status.is_some(), started.elapsed())
    }

    pub fn prove_empty(
        &self,
        generation: ProcessGeneration,
    ) -> Result<ProcessTreeCleanupEvidenceV1, NativeProcessError> {
        let empty = {
            let mut state = self.state.lock().expect("native process registry lock");
            let record = state
                .processes
                .get_mut(&generation.0)
                .ok_or(NativeProcessError::UnknownGeneration)?;
            record
                .descendants
                .extend(enumerate_descendants(record.handle.root_process_id));
            record.child.try_wait()?.is_some()
        };
        self.cleanup_evidence(generation, empty, Duration::ZERO)
    }

    fn cleanup_evidence(
        &self,
        generation: ProcessGeneration,
        root_exited: bool,
        completed_in: Duration,
    ) -> Result<ProcessTreeCleanupEvidenceV1, NativeProcessError> {
        let state = self.state.lock().expect("native process registry lock");
        let record = state
            .processes
            .get(&generation.0)
            .ok_or(NativeProcessError::UnknownGeneration)?;
        let living = enumerate_descendants(record.handle.root_process_id);
        let tree_empty = root_exited && living.is_empty();
        Ok(ProcessTreeCleanupEvidenceV1 {
            process_generation: generation,
            cooperative_requested: record.cooperative_requested,
            forced_termination_used: record.forced_termination_used,
            descendants_observed: record.descendants.iter().copied().collect(),
            tree_empty,
            orphan_risk: !tree_empty,
            completed_in,
        })
    }

    /// Starts the stable helper in a separate Job/process group that is never
    /// inserted into the application-generation registry.
    pub fn spawn_detached_helper(
        spec: &SanitizedProcessSpecV1,
    ) -> Result<DetachedHelperProcess, NativeProcessError> {
        validate_spec(spec)?;
        let executable = ExecutableIdentityV1::open(&spec.executable)?;
        let mut command = std::process::Command::new(&executable.canonical_path);
        command
            .args(&spec.arguments)
            .current_dir(canonical_directory(&spec.working_directory)?)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.group_spawn()?;
        if !ExecutableIdentityV1::open(&executable.canonical_path)
            .is_ok_and(|observed| observed == executable)
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(NativeProcessError::Identity(
                crate::identity::IdentityError::ExecutableChanged,
            ));
        }
        Ok(DetachedHelperProcess { child, executable })
    }
}

impl Drop for NativeProcessRegistry {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for record in state.processes.values_mut() {
            if record.child.try_wait().ok().flatten().is_none() {
                let _ = record.child.kill();
            }
            let _ = record.child.wait();
        }
        state.processes.clear();
    }
}

pub struct DetachedHelperProcess {
    child: GroupChild,
    pub executable: ExecutableIdentityV1,
}

impl DetachedHelperProcess {
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, NativeProcessError> {
        Ok(self.child.try_wait()?)
    }

    pub fn terminate(mut self) -> Result<ExitStatus, NativeProcessError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        Ok(self.child.wait()?)
    }
}

fn validate_spec(spec: &SanitizedProcessSpecV1) -> Result<(), NativeProcessError> {
    if spec.role.is_empty()
        || spec.role.len() > 128
        || spec.arguments.len() > MAX_ARGUMENTS
        || spec.arguments.iter().any(|value| value.contains('\0'))
        || spec.arguments.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
    {
        return Err(NativeProcessError::InvalidArguments);
    }
    if spec.environment.len() > MAX_ENVIRONMENT_ENTRIES
        || spec
            .environment
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .sum::<usize>()
            > MAX_ENVIRONMENT_BYTES
        || spec.environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains(['=', '\0'])
                || value.contains('\0')
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        return Err(NativeProcessError::InvalidEnvironment);
    }
    if !spec.executable.is_absolute() || !spec.working_directory.is_absolute() {
        return Err(NativeProcessError::AmbientPathDenied);
    }
    if spec.verification_only
        && !spec
            .arguments
            .iter()
            .any(|argument| argument == "--bootstrap-verification-only")
    {
        return Err(NativeProcessError::VerificationModeMissing);
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, NativeProcessError> {
    let path = std::fs::canonicalize(path)?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(NativeProcessError::WorkingDirectoryUnavailable)
    }
}

fn enumerate_descendants(root: u32) -> BTreeSet<u32> {
    let system = sysinfo::System::new_all();
    let mut parents = BTreeMap::<u32, Vec<u32>>::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            parents
                .entry(parent.as_u32())
                .or_default()
                .push(pid.as_u32());
        }
    }
    let mut result = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for child in parents.get(&parent).into_iter().flatten() {
            if result.insert(*child) {
                frontier.push(*child);
            }
        }
    }
    result
}

#[derive(Debug, Error)]
pub enum NativeProcessError {
    #[error("process argument vector is malformed or exceeds its bound")]
    InvalidArguments,
    #[error("process environment is malformed or exceeds its bound")]
    InvalidEnvironment,
    #[error("ambient or PATH-based process lookup is denied")]
    AmbientPathDenied,
    #[error("working directory is unavailable")]
    WorkingDirectoryUnavailable,
    #[error("verification-only launch is missing its fixed mode argument")]
    VerificationModeMissing,
    #[error("process containment is unavailable on this platform")]
    ContainmentUnavailable,
    #[error("this process generation already has a live handle")]
    GenerationAlreadyLive,
    #[error("process generation is unknown")]
    UnknownGeneration,
    #[error("monotonic process deadline is invalid")]
    Deadline,
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_spec(generation: u64, command: &str) -> SanitizedProcessSpecV1 {
        SanitizedProcessSpecV1 {
            executable: std::fs::canonicalize("/bin/sh").expect("shell executable"),
            arguments: vec!["-c".to_owned(), command.to_owned()],
            working_directory: std::env::current_dir().expect("working directory"),
            environment: BTreeMap::new(),
            process_generation: ProcessGeneration(generation),
            role: "test".to_owned(),
            verification_only: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_group_cleanup_is_generation_fenced_and_bounded() {
        let registry = NativeProcessRegistry::default();
        let handle = registry
            .spawn_tree(&shell_spec(1, "sleep 30 & wait"))
            .expect("spawn group");
        assert_eq!(handle.process_generation, ProcessGeneration(1));
        registry
            .request_cooperative_shutdown(ProcessGeneration(1))
            .expect("cooperative shutdown");
        if registry
            .await_exit(ProcessGeneration(1), Duration::from_millis(100))
            .expect("bounded wait")
            .is_none()
        {
            let evidence = registry
                .force_cleanup(ProcessGeneration(1), Duration::from_secs(2))
                .expect("forced cleanup");
            assert!(evidence.tree_empty, "{evidence:?}");
        }
        assert!(
            registry
                .prove_empty(ProcessGeneration(1))
                .expect("proof")
                .tree_empty
        );
    }

    #[cfg(unix)]
    #[test]
    fn detached_helper_is_not_in_the_application_generation_group() {
        let registry = NativeProcessRegistry::default();
        registry
            .spawn_tree(&shell_spec(2, "sleep 30"))
            .expect("application group");
        let mut helper = NativeProcessRegistry::spawn_detached_helper(&shell_spec(3, "sleep 30"))
            .expect("detached helper");
        registry
            .force_cleanup(ProcessGeneration(2), Duration::from_secs(2))
            .expect("application cleanup");
        assert!(helper.try_wait().expect("helper health").is_none());
        helper.terminate().expect("helper cleanup");
    }

    #[test]
    fn process_capability_report_distinguishes_supported_and_degraded() {
        assert!(NativeProcessCapabilityReportV1::current().supports_generation_supervision());
        let degraded = NativeProcessCapabilityReportV1 {
            containment: ProcessContainmentV1::Unsupported,
            ..NativeProcessCapabilityReportV1::current()
        };
        assert!(!degraded.supports_generation_supervision());
    }
}
