//! Generation-fenced worker process supervision and core-only IPC admission.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use aworkit_protocol::{
    MAX_FRAME_BYTES, ProcessGeneration, StableId, WorkerControlEnvelopeV1, WorkerControlKindV1,
    WorkerFrozenRunSnapshotV1, WorkerHandshakeV1, WorkerOutputEnvelopeV1, WorkerOutputKindV1,
    decode_frame, encode_frame,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::FrozenRunSnapshot;

/// A worker command that is safe to deliver only after its semantic commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerControl {
    Resume,
    Pause,
    Cancel,
    Input { input_id: StableId },
    ApprovalGranted { approval_id: StableId },
}

/// Authenticated handshake facts emitted by a freshly launched worker generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerHandshake {
    pub chat_id: StableId,
    pub generation: ProcessGeneration,
    pub snapshot_hash: String,
}

#[derive(Clone, Debug)]
struct WorkerRecord {
    generation: ProcessGeneration,
    snapshot_hash: String,
    healthy: bool,
    restarts: u32,
}

/// Tracks worker generations; platform spawning remains behind its narrow process adapter.
#[derive(Default)]
pub struct WorkerSupervisor {
    workers: BTreeMap<String, WorkerRecord>,
    max_restarts: u32,
}

impl WorkerSupervisor {
    #[must_use]
    pub fn with_restart_budget(max_restarts: u32) -> Self {
        Self {
            workers: BTreeMap::new(),
            max_restarts,
        }
    }

    /// Allocates a new core-owned generation for the immutable snapshot.
    pub fn start(
        &mut self,
        snapshot: &FrozenRunSnapshot,
    ) -> Result<WorkerHandshake, WorkerSupervisorError> {
        let generation = match self.workers.get(snapshot.chat_id.as_str()) {
            Some(record) => ProcessGeneration(
                record
                    .generation
                    .0
                    .checked_add(1)
                    .ok_or(WorkerSupervisorError::GenerationExhausted)?,
            ),
            None => ProcessGeneration(1),
        };
        let restarts = self
            .workers
            .get(snapshot.chat_id.as_str())
            .map_or(0, |record| record.restarts);
        if restarts > self.max_restarts {
            return Err(WorkerSupervisorError::RestartBudgetExhausted);
        }
        self.workers.insert(
            snapshot.chat_id.as_str().to_owned(),
            WorkerRecord {
                generation,
                snapshot_hash: snapshot.snapshot_hash.clone(),
                healthy: false,
                restarts,
            },
        );
        Ok(WorkerHandshake {
            chat_id: snapshot.chat_id.clone(),
            generation,
            snapshot_hash: snapshot.snapshot_hash.clone(),
        })
    }

    /// Admits the worker's handshake only when it proves the exact frozen identity.
    pub fn acknowledge_handshake(
        &mut self,
        handshake: &WorkerHandshake,
    ) -> Result<(), WorkerSupervisorError> {
        let record = self
            .workers
            .get_mut(handshake.chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != handshake.generation
            || record.snapshot_hash != handshake.snapshot_hash
        {
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        record.healthy = true;
        Ok(())
    }

    /// Validates that a committed control cannot reach a stale/unhealthy worker.
    pub fn deliver(
        &self,
        chat_id: &StableId,
        generation: ProcessGeneration,
        _control: &WorkerControl,
    ) -> Result<(), WorkerSupervisorError> {
        let record = self
            .workers
            .get(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != generation || !record.healthy {
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        Ok(())
    }

    /// Marks a dead worker and consumes one bounded replacement opportunity.
    pub fn crashed(
        &mut self,
        chat_id: &StableId,
        generation: ProcessGeneration,
    ) -> Result<(), WorkerSupervisorError> {
        let record = self
            .workers
            .get_mut(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        if record.generation != generation {
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        record.healthy = false;
        record.restarts = record
            .restarts
            .checked_add(1)
            .ok_or(WorkerSupervisorError::RestartBudgetExhausted)?;
        if record.restarts > self.max_restarts {
            return Err(WorkerSupervisorError::RestartBudgetExhausted);
        }
        Ok(())
    }
}

/// A supervisor rejects stale generations instead of risking cross-Run effects.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum WorkerSupervisorError {
    #[error("unknown worker Chat")]
    UnknownWorker,
    #[error("stale or unauthenticated worker generation")]
    StaleHandshake,
    #[error("worker generation counter is exhausted")]
    GenerationExhausted,
    #[error("worker restart budget is exhausted")]
    RestartBudgetExhausted,
    #[error("worker executable could not be started: {0}")]
    Spawn(String),
    #[error("worker framed transport failed: {0}")]
    Transport(String),
    #[error("worker handshake timed out")]
    HandshakeTimeout,
    #[error("worker output timed out")]
    OutputTimeout,
    #[error("worker heartbeat exceeded its health deadline")]
    HeartbeatTimeout,
    #[error("worker emitted an unexpected output during handshake")]
    UnexpectedHandshakeOutput,
    #[error("worker executable identity does not match the launched binary")]
    ExecutableIdentityMismatch,
    #[error("worker for this Chat is already alive")]
    AlreadyRunning,
    #[error("worker shutdown acknowledgement did not arrive before the deadline")]
    ShutdownTimeout,
    #[error("worker process exited unexpectedly")]
    ProcessExited,
    #[error("worker start control is malformed")]
    InvalidStartControl,
}

struct LiveWorkerV1 {
    child: Child,
    input: ChildStdin,
    outputs: Receiver<Result<WorkerOutputEnvelopeV1, String>>,
    pump: Option<JoinHandle<()>>,
    chat_id: StableId,
    run_id: StableId,
    generation: ProcessGeneration,
    snapshot_hash: String,
    last_heartbeat: Option<u64>,
    last_heartbeat_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default)]
struct RestartStateV1 {
    last_generation: u64,
    crashes: u32,
}

/// Owns actual worker child processes, framed stdio, generation fencing, and
/// bounded cleanup. Workers have no process-spawn capability, so the direct
/// child is the complete runtime process tree by contract.
pub struct ProcessWorkerSupervisorV1 {
    executable: PathBuf,
    workers: BTreeMap<String, LiveWorkerV1>,
    restart_state: BTreeMap<String, RestartStateV1>,
    maximum_restarts: u32,
}

impl ProcessWorkerSupervisorV1 {
    pub fn new(
        executable: impl Into<PathBuf>,
        maximum_restarts: u32,
    ) -> Result<Self, WorkerSupervisorError> {
        let executable = std::fs::canonicalize(executable.into())
            .map_err(|error| WorkerSupervisorError::Spawn(error.to_string()))?;
        if !executable.is_file() {
            return Err(WorkerSupervisorError::Spawn(
                "worker executable is not a file".to_owned(),
            ));
        }
        Ok(Self {
            executable,
            workers: BTreeMap::new(),
            restart_state: BTreeMap::new(),
            maximum_restarts,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn spawn_start(
        &mut self,
        snapshot: WorkerFrozenRunSnapshotV1,
        committed_cursor: u64,
        timeout: Duration,
    ) -> Result<WorkerHandshakeV1, WorkerSupervisorError> {
        if self.workers.contains_key(snapshot.chat_id.as_str()) {
            return Err(WorkerSupervisorError::AlreadyRunning);
        }
        let generation = self.allocate_generation(&snapshot.chat_id)?;
        let control = WorkerControlEnvelopeV1 {
            message_id: stable_id(&format!(
                "start:{}:{}:{}",
                snapshot.chat_id, snapshot.run_id, generation.0
            ))?,
            chat_id: snapshot.chat_id.clone(),
            run_id: snapshot.run_id.clone(),
            generation,
            snapshot_hash: snapshot.snapshot_hash.clone(),
            committed_cursor,
            control: WorkerControlKindV1::Start(snapshot),
        };
        self.spawn_control(control, timeout)
    }

    pub fn spawn_restore(
        &mut self,
        control: WorkerControlEnvelopeV1,
        timeout: Duration,
    ) -> Result<WorkerHandshakeV1, WorkerSupervisorError> {
        if !matches!(control.control, WorkerControlKindV1::Restore(_))
            || self.workers.contains_key(control.chat_id.as_str())
        {
            return Err(WorkerSupervisorError::InvalidStartControl);
        }
        let expected = self.next_generation(&control.chat_id)?;
        if expected != control.generation {
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        let allocated = self.allocate_generation(&control.chat_id)?;
        debug_assert_eq!(allocated, expected);
        self.spawn_control(control, timeout)
    }

    pub fn send_control(
        &mut self,
        control: &WorkerControlEnvelopeV1,
    ) -> Result<(), WorkerSupervisorError> {
        let worker = self
            .workers
            .get_mut(control.chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        validate_control_identity(worker, control)?;
        write_control(&mut worker.input, control)
    }

    pub fn receive(
        &mut self,
        chat_id: &StableId,
        timeout: Duration,
    ) -> Result<WorkerOutputEnvelopeV1, WorkerSupervisorError> {
        let worker = self
            .workers
            .get_mut(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        let output = receive_output(worker, timeout)?;
        validate_output_identity(worker, &output)?;
        if let WorkerOutputKindV1::Heartbeat(heartbeat) = &output.output {
            if worker
                .last_heartbeat
                .is_some_and(|sequence| heartbeat.sequence <= sequence)
            {
                return Err(WorkerSupervisorError::StaleHandshake);
            }
            worker.last_heartbeat = Some(heartbeat.sequence);
            worker.last_heartbeat_at = Some(Instant::now());
        }
        Ok(output)
    }

    pub fn check_health(
        &mut self,
        chat_id: &StableId,
    ) -> Result<ProcessGeneration, WorkerSupervisorError> {
        let worker = self
            .workers
            .get_mut(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        match worker
            .child
            .try_wait()
            .map_err(|error| WorkerSupervisorError::Transport(error.to_string()))?
        {
            None => Ok(worker.generation),
            Some(_) => Err(WorkerSupervisorError::ProcessExited),
        }
    }

    pub fn check_health_within(
        &mut self,
        chat_id: &StableId,
        maximum_silence: Duration,
    ) -> Result<ProcessGeneration, WorkerSupervisorError> {
        let generation = self.check_health(chat_id)?;
        let worker = self
            .workers
            .get(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        if worker
            .last_heartbeat_at
            .is_none_or(|observed| observed.elapsed() > maximum_silence)
        {
            return Err(WorkerSupervisorError::HeartbeatTimeout);
        }
        Ok(generation)
    }

    pub fn shutdown(
        &mut self,
        chat_id: &StableId,
        committed_cursor: u64,
        timeout: Duration,
    ) -> Result<(), WorkerSupervisorError> {
        let mut worker = self
            .workers
            .remove(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        let control_id = stable_id(&format!(
            "shutdown:{}:{}",
            worker.chat_id, worker.generation.0
        ))?;
        let control = WorkerControlEnvelopeV1 {
            message_id: stable_id(&format!("shutdown.message:{control_id}"))?,
            chat_id: worker.chat_id.clone(),
            run_id: worker.run_id.clone(),
            generation: worker.generation,
            snapshot_hash: worker.snapshot_hash.clone(),
            committed_cursor,
            control: WorkerControlKindV1::Shutdown {
                control_id: control_id.clone(),
            },
        };
        write_control(&mut worker.input, &control)?;
        let deadline = Instant::now() + timeout;
        let mut acknowledged = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receive_output(&mut worker, remaining) {
                Ok(output) => {
                    validate_output_identity(&worker, &output)?;
                    if matches!(
                        output.output,
                        WorkerOutputKindV1::ShutdownAck { control_id: ref received }
                            if received == &control_id
                    ) {
                        acknowledged = true;
                        break;
                    }
                }
                Err(WorkerSupervisorError::OutputTimeout) => break,
                Err(error) => {
                    terminate_worker(&mut worker);
                    return Err(error);
                }
            }
        }
        if !acknowledged {
            terminate_worker(&mut worker);
            return Err(WorkerSupervisorError::ShutdownTimeout);
        }
        wait_or_kill(&mut worker, deadline);
        Ok(())
    }

    pub fn mark_crashed(
        &mut self,
        chat_id: &StableId,
        generation: ProcessGeneration,
    ) -> Result<(), WorkerSupervisorError> {
        let mut worker = self
            .workers
            .remove(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        if worker.generation != generation {
            self.workers.insert(chat_id.as_str().to_owned(), worker);
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        terminate_worker(&mut worker);
        let state = self
            .restart_state
            .get_mut(chat_id.as_str())
            .ok_or(WorkerSupervisorError::UnknownWorker)?;
        state.crashes = state
            .crashes
            .checked_add(1)
            .ok_or(WorkerSupervisorError::RestartBudgetExhausted)?;
        if state.crashes > self.maximum_restarts {
            return Err(WorkerSupervisorError::RestartBudgetExhausted);
        }
        Ok(())
    }

    fn allocate_generation(
        &mut self,
        chat_id: &StableId,
    ) -> Result<ProcessGeneration, WorkerSupervisorError> {
        let generation = self.next_generation(chat_id)?;
        let state = self
            .restart_state
            .entry(chat_id.as_str().to_owned())
            .or_default();
        state.last_generation = generation.0;
        Ok(generation)
    }

    fn next_generation(
        &self,
        chat_id: &StableId,
    ) -> Result<ProcessGeneration, WorkerSupervisorError> {
        let state = self
            .restart_state
            .get(chat_id.as_str())
            .copied()
            .unwrap_or_default();
        if state.crashes > self.maximum_restarts {
            return Err(WorkerSupervisorError::RestartBudgetExhausted);
        }
        state
            .last_generation
            .checked_add(1)
            .map(ProcessGeneration)
            .ok_or(WorkerSupervisorError::GenerationExhausted)
    }

    fn spawn_control(
        &mut self,
        control: WorkerControlEnvelopeV1,
        timeout: Duration,
    ) -> Result<WorkerHandshakeV1, WorkerSupervisorError> {
        let snapshot_hash = control.snapshot_hash.clone();
        let chat_id = control.chat_id.clone();
        let run_id = control.run_id.clone();
        let generation = control.generation;
        let mut command = Command::new(&self.executable);
        command
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| WorkerSupervisorError::Spawn(error.to_string()))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| WorkerSupervisorError::Spawn("worker stdin was not piped".to_owned()))?;
        let output = child.stdout.take().ok_or_else(|| {
            WorkerSupervisorError::Spawn("worker stdout was not piped".to_owned())
        })?;
        let (sender, receiver) = mpsc::sync_channel(1_024);
        let pump = std::thread::Builder::new()
            .name(format!("aworkit-worker-output-{}", generation.0))
            .spawn(move || pump_outputs(output, sender))
            .map_err(|error| WorkerSupervisorError::Spawn(error.to_string()))?;
        let mut worker = LiveWorkerV1 {
            child,
            input,
            outputs: receiver,
            pump: Some(pump),
            chat_id: chat_id.clone(),
            run_id: run_id.clone(),
            generation,
            snapshot_hash: snapshot_hash.clone(),
            last_heartbeat: None,
            last_heartbeat_at: None,
        };
        if let Err(error) = write_control(&mut worker.input, &control) {
            terminate_worker(&mut worker);
            return Err(error);
        }
        let output = match receive_output(&mut worker, timeout) {
            Ok(output) => output,
            Err(error) => {
                terminate_worker(&mut worker);
                return Err(match error {
                    WorkerSupervisorError::OutputTimeout => WorkerSupervisorError::HandshakeTimeout,
                    other => other,
                });
            }
        };
        let WorkerOutputKindV1::Handshake(handshake) = output.output else {
            terminate_worker(&mut worker);
            return Err(WorkerSupervisorError::UnexpectedHandshakeOutput);
        };
        let executable_identity = match std::fs::canonicalize(&handshake.executable_identity) {
            Ok(identity) => identity,
            Err(_) => {
                terminate_worker(&mut worker);
                return Err(WorkerSupervisorError::ExecutableIdentityMismatch);
            }
        };
        if output.generation != generation
            || handshake.protocol_version != 1
            || handshake.chat_id != chat_id
            || handshake.run_id != run_id
            || handshake.generation != generation
            || handshake.snapshot_hash != snapshot_hash
            || executable_identity != self.executable
        {
            terminate_worker(&mut worker);
            return Err(WorkerSupervisorError::StaleHandshake);
        }
        self.workers.insert(chat_id.to_string(), worker);
        Ok(handshake)
    }
}

impl Drop for ProcessWorkerSupervisorV1 {
    fn drop(&mut self) {
        for worker in self.workers.values_mut() {
            terminate_worker(worker);
        }
        self.workers.clear();
    }
}

fn validate_control_identity(
    worker: &LiveWorkerV1,
    control: &WorkerControlEnvelopeV1,
) -> Result<(), WorkerSupervisorError> {
    if worker.chat_id != control.chat_id
        || worker.run_id != control.run_id
        || worker.generation != control.generation
        || worker.snapshot_hash != control.snapshot_hash
    {
        Err(WorkerSupervisorError::StaleHandshake)
    } else {
        Ok(())
    }
}

fn validate_output_identity(
    worker: &LiveWorkerV1,
    output: &WorkerOutputEnvelopeV1,
) -> Result<(), WorkerSupervisorError> {
    if output.generation != worker.generation {
        return Err(WorkerSupervisorError::StaleHandshake);
    }
    if let WorkerOutputKindV1::Proposal(proposal) = &output.output
        && (proposal.chat_id != worker.chat_id
            || proposal.run_id != worker.run_id
            || proposal.generation != worker.generation
            || proposal.snapshot_hash != worker.snapshot_hash)
    {
        return Err(WorkerSupervisorError::StaleHandshake);
    }
    Ok(())
}

fn write_control(
    input: &mut ChildStdin,
    control: &WorkerControlEnvelopeV1,
) -> Result<(), WorkerSupervisorError> {
    let frame = encode_frame(control)
        .map_err(|error| WorkerSupervisorError::Transport(error.to_string()))?;
    input
        .write_all(&frame)
        .and_then(|()| input.flush())
        .map_err(|error| WorkerSupervisorError::Transport(error.to_string()))
}

fn receive_output(
    worker: &mut LiveWorkerV1,
    timeout: Duration,
) -> Result<WorkerOutputEnvelopeV1, WorkerSupervisorError> {
    match worker.outputs.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(WorkerSupervisorError::Transport(error)),
        Err(RecvTimeoutError::Timeout) => Err(WorkerSupervisorError::OutputTimeout),
        Err(RecvTimeoutError::Disconnected) => Err(WorkerSupervisorError::ProcessExited),
    }
}

fn pump_outputs(
    mut output: impl Read,
    sender: mpsc::SyncSender<Result<WorkerOutputEnvelopeV1, String>>,
) {
    loop {
        match read_output_frame(&mut output) {
            Ok(Some(output)) => {
                if sender.send(Ok(output)).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

fn read_output_frame<R: Read>(input: &mut R) -> Result<Option<WorkerOutputEnvelopeV1>, String> {
    let mut prefix = [0_u8; 4];
    let mut read = 0;
    while read < prefix.len() {
        let count = input
            .read(&mut prefix[read..])
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return if read == 0 {
                Ok(None)
            } else {
                Err("truncated worker frame prefix".to_owned())
            };
        }
        read += count;
    }
    let body_len = u32::from_be_bytes(prefix) as usize;
    if body_len > MAX_FRAME_BYTES {
        return Err("worker frame exceeded one MiB".to_owned());
    }
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + body_len, 0);
    input
        .read_exact(&mut frame[4..])
        .map_err(|error| error.to_string())?;
    decode_frame(&frame).map_err(|error| error.to_string())
}

fn wait_or_kill(worker: &mut LiveWorkerV1, deadline: Instant) {
    while Instant::now() < deadline {
        if worker.child.try_wait().ok().flatten().is_some() {
            join_pump(worker);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    terminate_worker(worker);
}

fn terminate_worker(worker: &mut LiveWorkerV1) {
    let _ = worker.child.kill();
    let _ = worker.child.wait();
    join_pump(worker);
}

fn join_pump(worker: &mut LiveWorkerV1) {
    if let Some(pump) = worker.pump.take() {
        let _ = pump.join();
    }
}

fn stable_id(material: &str) -> Result<StableId, WorkerSupervisorError> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    StableId::parse(format!("supervisor.{}", &digest[..48]))
        .map_err(|_| WorkerSupervisorError::InvalidStartControl)
}
