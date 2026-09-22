//! One-shot Codex delegation over the Codex App Server protocol.
//!
//! One delegation spawns one app-server process, creates exactly one ephemeral
//! thread, runs exactly one turn, and settles with the child's final answer or
//! a safe failure diagnostic. There is no continuation, no resume and no
//! pooling: native Codex configuration and login stay authoritative, and the
//! configured permission mode is the only thing Aworkit overrides about how the
//! child may act.
//!
//! The child runs unattended. Approval, permission, MCP-elicitation and
//! user-input requests are answered or declined by fixed policy without a
//! human, and any other server request fails the run instead of being guessed
//! at. Codex commentary, reasoning, tool traffic, stderr and workspace diffs
//! never cross this boundary.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use aworkit_protocol::StableId;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    CancellationToken,
    codex_app_server::{
        CodexAppServerEnvironmentV1, ManagedGroupChild, ReaderMessage, drain_discarded,
        read_json_lines,
    },
    external_agent::{
        ExternalAgentBackendV1, OneShotDelegationV1, SubagentBackendCapabilitiesV1,
        SubagentOutcomeV1, SubagentStopReasonV1,
    },
};

/// Default bound for the initialize, thread and turn submissions.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// Default bound for one protocol message.
pub const DEFAULT_MAXIMUM_MESSAGE_BYTES: usize = 1_024 * 1_024;
/// Default bound for the whole run's message count.
pub const DEFAULT_MAXIMUM_MESSAGES: usize = 8_192;
/// How long one protocol read may block before the run rechecks its bounds.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Largest accepted argv for one Codex app-server process.
const MAXIMUM_ARGUMENTS: usize = 512;
/// Largest accepted environment overlay for one Codex app-server process.
const MAXIMUM_ENVIRONMENT_ENTRIES: usize = 256;

/// Non-interactive permission mode fixed for every thread a backend starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexPermissionModeV1 {
    /// Never ask for approval; the native sandbox still applies.
    Never,
    /// Route permission requests through Codex automatic review.
    ApproveForMe,
    /// Skip approval and sandbox enforcement. Must be selected explicitly.
    DangerouslyBypassApprovalsAndSandbox,
}

impl CodexPermissionModeV1 {
    /// The exact `thread/start` fields this mode maps to.
    fn thread_start_params(self) -> Value {
        match self {
            Self::Never => json!({"approvalPolicy": "never"}),
            Self::ApproveForMe => json!({
                "approvalPolicy": "on-request",
                "approvalsReviewer": "auto_review",
                "sandbox": "workspace-write",
            }),
            Self::DangerouslyBypassApprovalsAndSandbox => json!({
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
            }),
        }
    }

    /// Stable, non-secret mode name used in unattended diagnostics.
    fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::ApproveForMe => "approve-for-me",
            Self::DangerouslyBypassApprovalsAndSandbox => {
                "dangerously-bypass-approvals-and-sandbox"
            }
        }
    }
}

/// Runtime bounds for one one-shot Codex delegation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexOneShotLimitsV1 {
    /// Bound for initialize, `thread/start` and `turn/start` submissions.
    pub handshake_timeout: Duration,
    /// Largest accepted protocol message.
    pub maximum_message_bytes: usize,
    /// Largest accepted protocol message count for the whole run.
    pub maximum_messages: usize,
    /// How long one read blocks before the run rechecks its bounds.
    pub poll_interval: Duration,
}

impl Default for CodexOneShotLimitsV1 {
    fn default() -> Self {
        Self {
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            maximum_message_bytes: DEFAULT_MAXIMUM_MESSAGE_BYTES,
            maximum_messages: DEFAULT_MAXIMUM_MESSAGES,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Exact argv-only configuration for one Codex one-shot backend.
pub struct CodexOneShotConfigV1 {
    /// Registry name this backend answers to.
    pub name: String,
    /// Resolved Codex executable that implements `app-server`.
    pub executable: PathBuf,
    /// Argument vector, which must begin with the `app-server` subcommand.
    pub arguments: Vec<String>,
    /// Working directory of the child process. Codex threads default to it.
    pub working_directory: Option<PathBuf>,
    /// Whether the child inherits the ambient environment. Codex normally
    /// needs its existing configuration and login state.
    pub inherit_environment: bool,
    /// Explicit environment overlay for this transient process only.
    pub environment: Vec<CodexAppServerEnvironmentV1>,
    /// Non-interactive permission mode fixed for every thread.
    pub permission_mode: CodexPermissionModeV1,
    /// Runtime bounds.
    pub limits: CodexOneShotLimitsV1,
}

impl CodexOneShotConfigV1 {
    fn validate(&self) -> Result<(), CodexOneShotErrorV1> {
        if StableId::parse(self.name.clone()).is_err() {
            return Err(CodexOneShotErrorV1::InvalidBackendName);
        }
        if !self.executable.is_absolute() {
            return Err(CodexOneShotErrorV1::InvalidExecutable);
        }
        if self.arguments.first().map(String::as_str) != Some("app-server") {
            return Err(CodexOneShotErrorV1::MissingAppServerSubcommand);
        }
        if self.arguments.len() > MAXIMUM_ARGUMENTS
            || self
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
            || argument_bytes(&self.arguments) > 64 * 1_024
        {
            return Err(CodexOneShotErrorV1::InvalidArguments);
        }
        if self.environment.len() > MAXIMUM_ENVIRONMENT_ENTRIES {
            return Err(CodexOneShotErrorV1::InvalidEnvironment);
        }
        let limits = self.limits;
        if limits.handshake_timeout.is_zero()
            || limits.handshake_timeout > Duration::from_secs(600)
            || limits.maximum_message_bytes == 0
            || limits.maximum_message_bytes > 4 * 1_024 * 1_024
            || limits.maximum_messages == 0
            || limits.maximum_messages > 1_000_000
            || limits.poll_interval.is_zero()
            || limits.poll_interval > Duration::from_secs(5)
        {
            return Err(CodexOneShotErrorV1::InvalidLimits);
        }
        Ok(())
    }
}

fn argument_bytes(arguments: &[String]) -> usize {
    arguments.iter().map(String::len).sum()
}

/// One protocol failure observed on the Codex App Server wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexWireErrorV1 {
    /// The transport failed.
    Transport,
    /// The peer closed the protocol stream or exited.
    Exited,
    /// No message arrived inside the requested wait.
    TimedOut,
    /// A message was not a valid protocol object.
    Protocol,
    /// A message exceeded the accepted bound.
    MessageTooLarge,
    /// The run exceeded its accepted message count.
    MessageLimit,
}

/// The bounded JSON-RPC transport one one-shot run speaks over.
///
/// Implementations are per-run and own their own process; the seam exists so
/// the protocol state machine can be exercised without a child process.
pub trait CodexProtocolWireV1: Send {
    /// Sends one JSON-RPC message.
    fn send(&mut self, message: &Value) -> Result<(), CodexWireErrorV1>;
    /// Receives the next message, waiting at most `timeout`.
    fn receive(&mut self, timeout: Duration) -> Result<Value, CodexWireErrorV1>;
}

/// The stdio transport: one supervised app-server process, one reader thread.
pub struct StdioCodexWireV1 {
    child: Option<ManagedGroupChild>,
    writer: Option<std::process::ChildStdin>,
    receiver: Receiver<ReaderMessage>,
    readers: Vec<JoinHandle<()>>,
    limits: CodexOneShotLimitsV1,
    observed_messages: usize,
}

impl StdioCodexWireV1 {
    /// Spawns the configured app-server with a supervised process group.
    pub fn spawn(config: &CodexOneShotConfigV1) -> Result<Self, CodexOneShotErrorV1> {
        config.validate()?;
        if !config.executable.is_file() {
            return Err(CodexOneShotErrorV1::InvalidExecutable);
        }
        let mut command = Command::new(&config.executable);
        command
            .args(&config.arguments)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if !config.inherit_environment {
            command.env_clear();
        }
        for entry in &config.environment {
            command.env(entry.name(), entry.value());
        }
        if let Some(working_directory) = &config.working_directory {
            command.current_dir(working_directory);
        }
        let mut child = ManagedGroupChild::spawn(&mut command).map_err(map_probe_error)?;
        let writer = child.stdin().map_err(map_probe_error)?;
        let stdout = child.stdout().map_err(map_probe_error)?;
        let stderr = child.stderr().map_err(map_probe_error)?;
        let (sender, receiver) = mpsc::sync_channel(16);
        let maximum_message_bytes = config.limits.maximum_message_bytes;
        let stdout_reader = thread::spawn(move || {
            read_json_lines(stdout, maximum_message_bytes, &sender);
        });
        let stderr_reader = thread::spawn(move || drain_discarded(stderr));
        Ok(Self {
            child: Some(child),
            writer: Some(writer),
            receiver,
            readers: vec![stdout_reader, stderr_reader],
            limits: config.limits,
            observed_messages: 0,
        })
    }
}

impl CodexProtocolWireV1 for StdioCodexWireV1 {
    fn send(&mut self, message: &Value) -> Result<(), CodexWireErrorV1> {
        let writer = self.writer.as_mut().ok_or(CodexWireErrorV1::Transport)?;
        write_json_line(writer, message, self.limits.maximum_message_bytes)
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value, CodexWireErrorV1> {
        let message = self
            .receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => CodexWireErrorV1::TimedOut,
                mpsc::RecvTimeoutError::Disconnected => CodexWireErrorV1::Exited,
            })?;
        self.observed_messages = self.observed_messages.saturating_add(1);
        if self.observed_messages > self.limits.maximum_messages {
            return Err(CodexWireErrorV1::MessageLimit);
        }
        match message {
            ReaderMessage::Line(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| CodexWireErrorV1::Protocol)
            }
            ReaderMessage::TooLarge => Err(CodexWireErrorV1::MessageTooLarge),
            ReaderMessage::Transport => Err(CodexWireErrorV1::Transport),
            ReaderMessage::Eof => Err(CodexWireErrorV1::Exited),
        }
    }
}

impl Drop for StdioCodexWireV1 {
    fn drop(&mut self) {
        // Closing stdin first lets a healthy app-server exit on its own; the
        // supervised group is then terminated as a complete tree so no Codex
        // descendant survives the delegation.
        drop(self.writer.take());
        drop(self.child.take());
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn write_json_line(
    writer: &mut impl Write,
    message: &Value,
    maximum_message_bytes: usize,
) -> Result<(), CodexWireErrorV1> {
    let mut bytes = serde_json::to_vec(message).map_err(|_| CodexWireErrorV1::Protocol)?;
    if bytes.len().saturating_add(1) > maximum_message_bytes {
        return Err(CodexWireErrorV1::MessageTooLarge);
    }
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| CodexWireErrorV1::Transport)
}

fn map_probe_error(
    error: crate::codex_app_server::CodexAppServerProbeError,
) -> CodexOneShotErrorV1 {
    match error {
        crate::codex_app_server::CodexAppServerProbeError::Launch
        | crate::codex_app_server::CodexAppServerProbeError::InvalidPath => {
            CodexOneShotErrorV1::Launch
        }
        _ => CodexOneShotErrorV1::Transport,
    }
}

/// A run failure already settled into its outward outcome.
type RunResult<T> = Result<T, SubagentOutcomeV1>;

/// Observable state of one in-flight run.
struct RunState {
    thread_id: String,
    turn_id: Option<String>,
    last_final_answer: Option<String>,
    last_unphased_answer: Option<String>,
    diagnostic: Option<String>,
    terminal_turn: Option<Value>,
    early_notifications: Vec<(String, Value)>,
    interruption_sent: bool,
}

impl RunState {
    fn new() -> Self {
        Self {
            thread_id: String::new(),
            turn_id: None,
            last_final_answer: None,
            last_unphased_answer: None,
            diagnostic: None,
            terminal_turn: None,
            early_notifications: Vec::new(),
            interruption_sent: false,
        }
    }

    /// The best final answer observed so far, preserving exact bytes.
    fn selected_answer(&self) -> Option<&str> {
        self.last_final_answer
            .as_deref()
            .or(self.last_unphased_answer.as_deref())
    }

    fn record_diagnostic(&mut self, diagnostic: String) {
        self.diagnostic = Some(diagnostic);
    }
}

/// Runs one unattended one-shot Codex delegation to completion.
pub fn run_codex_one_shot(
    wire: &mut dyn CodexProtocolWireV1,
    config: &CodexOneShotConfigV1,
    request: &OneShotDelegationV1,
    cancellation: &CancellationToken,
) -> SubagentOutcomeV1 {
    match drive(wire, config, request, cancellation) {
        Ok(outcome) => outcome,
        Err(outcome) => outcome,
    }
}

fn drive(
    wire: &mut dyn CodexProtocolWireV1,
    config: &CodexOneShotConfigV1,
    request: &OneShotDelegationV1,
    cancellation: &CancellationToken,
) -> RunResult<SubagentOutcomeV1> {
    if let Err(error) = config.validate() {
        return Err(SubagentOutcomeV1::failed(
            SubagentStopReasonV1::ProductError,
            format!("Codex delegation configuration is invalid: {error}"),
        ));
    }
    let limits = config.limits;
    let deadline = Instant::now() + request.deadline;

    if cancellation.is_cancelled() {
        return Err(SubagentOutcomeV1::aborted());
    }

    // initialize → initialized
    send_request(
        wire,
        1,
        "initialize",
        &json!({
            "clientInfo": {
                "name": "aworkit",
                "title": "Aworkit",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {"experimentalApi": false, "requestAttestation": false},
        }),
        "initialize",
    )?;
    let mut state = RunState::new();
    let initialize = await_response(
        wire,
        &mut state,
        config,
        1,
        deadline,
        cancellation,
        limits,
        "initialize",
    )?;
    if initialize.get("platformOs").is_none() && initialize.get("userAgent").is_none() {
        return Err(protocol_stop(
            "initialize",
            "the app-server initialize response was invalid",
        ));
    }
    send(
        wire,
        &json!({"method": "initialized", "params": {}}),
        "initialize",
    )?;

    // thread/start
    let mut thread_params = json!({
        "cwd": path_text(&request.working_directory)?,
        "ephemeral": true,
    });
    if let Some(model) = request.options.model.as_deref() {
        thread_params["model"] = json!(model);
    }
    merge_object(
        &mut thread_params,
        config.permission_mode.thread_start_params(),
    );
    send_request(wire, 2, "thread/start", &thread_params, "thread-start")?;
    let thread = await_response(
        wire,
        &mut state,
        config,
        2,
        deadline,
        cancellation,
        limits,
        "thread-start",
    )?;
    let thread = thread.get("thread").ok_or_else(|| {
        protocol_stop(
            "thread-start",
            "the app-server thread/start response was invalid",
        )
    })?;
    if thread.get("ephemeral") != Some(&Value::Bool(true)) {
        return Err(protocol_stop(
            "thread-start",
            "the app-server did not create an ephemeral thread",
        ));
    }
    let thread_id = thread
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 512)
        .ok_or_else(|| {
            protocol_stop("thread-start", "the app-server thread identity was invalid")
        })?;
    state.thread_id = thread_id.to_owned();

    // turn/start
    let mut turn_params = json!({
        "threadId": state.thread_id,
        "input": [{"type": "text", "text": request.task, "text_elements": []}],
    });
    if let Some(effort) = request.options.reasoning_effort.as_deref() {
        turn_params["effort"] = json!(effort);
    }
    send_request(wire, 3, "turn/start", &turn_params, "turn-start")?;
    let turn = await_response(
        wire,
        &mut state,
        config,
        3,
        deadline,
        cancellation,
        limits,
        "turn-start",
    )?;
    let turn_id = turn
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 512)
        .ok_or_else(|| protocol_stop("turn-start", "the app-server turn identity was invalid"))?;
    commit_turn_id(&mut state, turn_id);

    // Await the authoritative terminal notification for this thread and turn.
    while state.terminal_turn.is_none() {
        if cancellation.is_cancelled() {
            interrupt(wire, &mut state);
            return Err(SubagentOutcomeV1::aborted());
        }
        if Instant::now() >= deadline {
            interrupt(wire, &mut state);
            return Err(SubagentOutcomeV1::failed(
                SubagentStopReasonV1::Limit,
                failure_diagnostic("turn", "limit", None),
            ));
        }
        let wait = limits
            .poll_interval
            .min(deadline.saturating_duration_since(Instant::now()));
        match wire.receive(wait) {
            Ok(message) => handle_message(wire, &mut state, &message, config)?,
            Err(CodexWireErrorV1::TimedOut) => {}
            Err(error) => return Err(wire_stop("turn", error)),
        }
    }

    let terminal = state.terminal_turn.clone().unwrap_or(Value::Null);
    settle_turn(&state, &terminal)
}

fn settle_turn(state: &RunState, terminal: &Value) -> RunResult<SubagentOutcomeV1> {
    match terminal.get("status").and_then(Value::as_str) {
        Some("completed") => {
            let answer = state.selected_answer().unwrap_or("").to_owned();
            if answer.trim().is_empty() {
                return Err(SubagentOutcomeV1::failed(
                    SubagentStopReasonV1::InvalidResult,
                    failure_diagnostic("turn", "invalid-result", None),
                ));
            }
            let mut outcome = SubagentOutcomeV1::completed(answer).map_err(|_| {
                SubagentOutcomeV1::failed(
                    SubagentStopReasonV1::InvalidResult,
                    failure_diagnostic("turn", "invalid-result", None),
                )
            })?;
            if let Some(diagnostic) = &state.diagnostic {
                outcome.diagnostic = Some(diagnostic.clone());
            }
            Ok(outcome)
        }
        Some("interrupted") => Err(SubagentOutcomeV1::failed(
            SubagentStopReasonV1::Aborted,
            "Codex interrupted the delegated turn",
        )),
        Some("failed") => {
            let (reason, category, http_status) = classify_turn_failure(terminal);
            Err(SubagentOutcomeV1::failed(
                reason,
                failure_diagnostic("turn", category, http_status),
            ))
        }
        _ => Err(protocol_stop(
            "turn",
            "the app-server returned an invalid terminal turn status",
        )),
    }
}

fn classify_turn_failure(turn: &Value) -> (SubagentStopReasonV1, &'static str, Option<u64>) {
    let info = turn
        .get("error")
        .and_then(|error| error.get("codexErrorInfo"));
    match info {
        Some(Value::String(info)) => match info.as_str() {
            "contextWindowExceeded" | "sessionBudgetExceeded" | "usageLimitExceeded" => {
                (SubagentStopReasonV1::Limit, "limit", None)
            }
            "serverOverloaded" | "internalServerError" => {
                (SubagentStopReasonV1::Service, "service", None)
            }
            "cyberPolicy" | "misalignmentPolicyViolation" | "unauthorized" | "sandboxError" => {
                (SubagentStopReasonV1::AccessPolicy, "access-policy", None)
            }
            "badRequest" | "threadRollbackFailed" | "other" => {
                (SubagentStopReasonV1::ProductError, "product-error", None)
            }
            _ => (SubagentStopReasonV1::Unknown, "unknown", None),
        },
        Some(Value::Object(info)) => {
            let Some((category, detail)) = info.iter().next() else {
                return (SubagentStopReasonV1::Unknown, "unknown", None);
            };
            if info.len() != 1 {
                return (SubagentStopReasonV1::Unknown, "unknown", None);
            }
            match category.as_str() {
                "httpConnectionFailed"
                | "responseStreamConnectionFailed"
                | "responseStreamDisconnected"
                | "responseTooManyFailedAttempts" => (
                    SubagentStopReasonV1::Transport,
                    "transport",
                    detail
                        .get("httpStatusCode")
                        .and_then(Value::as_u64)
                        .filter(|status| *status <= 65_535),
                ),
                "activeTurnNotSteerable" => {
                    (SubagentStopReasonV1::ProductError, "product-error", None)
                }
                _ => (SubagentStopReasonV1::Unknown, "unknown", None),
            }
        }
        _ => (SubagentStopReasonV1::Unknown, "unknown", None),
    }
}

/// Handles one protocol message that is not a response to this run's requests.
fn handle_message(
    wire: &mut dyn CodexProtocolWireV1,
    state: &mut RunState,
    message: &Value,
    config: &CodexOneShotConfigV1,
) -> Result<(), SubagentOutcomeV1> {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        if let Some(id) = message.get("id") {
            return answer_server_request(wire, state, id, method, &params, config);
        }
        return observe_notification(state, method, &params);
    }
    // A response is correlated by the request that awaited it, so anything
    // reaching here has already been claimed or is not this run's business.
    Ok(())
}

fn observe_notification(
    state: &mut RunState,
    method: &str,
    params: &Value,
) -> Result<(), SubagentOutcomeV1> {
    if method == "item/completed" {
        if params.get("threadId").and_then(Value::as_str) != Some(state.thread_id.as_str()) {
            return Ok(());
        }
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str) else {
            return Ok(());
        };
        if state.turn_id.as_deref() != Some(turn_id) {
            if state.turn_id.is_none() {
                state
                    .early_notifications
                    .push((method.to_owned(), params.clone()));
            }
            return Ok(());
        }
        return record_item(state, params);
    }
    if method == "turn/completed" {
        if params.get("threadId").and_then(Value::as_str) != Some(state.thread_id.as_str()) {
            return Ok(());
        }
        let turn = params.get("turn").cloned().unwrap_or(Value::Null);
        let Some(turn_id) = turn.get("id").and_then(Value::as_str) else {
            return Err(protocol_stop(
                "turn",
                "the app-server turn/completed notification was invalid",
            ));
        };
        if state.turn_id.is_none() {
            state
                .early_notifications
                .push((method.to_owned(), params.clone()));
            return Ok(());
        }
        if state.turn_id.as_deref() != Some(turn_id) {
            return Ok(());
        }
        state.terminal_turn = Some(turn);
        return Ok(());
    }
    // Every other notification is observational detail this boundary does not
    // carry, including Codex reasoning and tool traffic.
    Ok(())
}

fn record_item(state: &mut RunState, params: &Value) -> Result<(), SubagentOutcomeV1> {
    let Some(item) = params.get("item") else {
        return Err(protocol_stop(
            "turn",
            "the app-server item/completed notification was invalid",
        ));
    };
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let Some(text) = item.get("text").and_then(Value::as_str) else {
                return Err(protocol_stop(
                    "turn",
                    "the app-server returned an invalid agent message",
                ));
            };
            match item.get("phase") {
                Some(Value::String(phase)) if phase == "final_answer" => {
                    state.last_final_answer = Some(text.to_owned());
                }
                Some(Value::Null) | None => {
                    state.last_unphased_answer = Some(text.to_owned());
                }
                Some(Value::String(phase)) if phase == "commentary" => {}
                _ => {
                    return Err(protocol_stop(
                        "turn",
                        "the app-server returned an unknown agent message phase",
                    ));
                }
            }
            Ok(())
        }
        Some("commandExecution")
            if item.get("status").and_then(Value::as_str) == Some("declined") =>
        {
            state.record_diagnostic(
                "Codex declined the command under the selected permission mode".to_owned(),
            );
            Ok(())
        }
        Some("fileChange") if item.get("status").and_then(Value::as_str) == Some("declined") => {
            state.record_diagnostic(
                "Codex declined the file change under the selected permission mode".to_owned(),
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Answers one server request with the fixed unattended policy.
fn answer_server_request(
    wire: &mut dyn CodexProtocolWireV1,
    state: &mut RunState,
    id: &Value,
    method: &str,
    params: &Value,
    config: &CodexOneShotConfigV1,
) -> Result<(), SubagentOutcomeV1> {
    let mode = config.permission_mode.diagnostic_name();
    let (result, request, decision) = match method {
        "item/commandExecution/requestApproval" => {
            let decision = unattended_decision(params)?;
            (
                json!({"decision": decision}),
                "command approval",
                if decision == "cancel" {
                    "cancelled"
                } else {
                    "declined"
                },
            )
        }
        "item/fileChange/requestApproval" => {
            let decision = unattended_decision(params)?;
            (
                json!({"decision": decision}),
                "file approval",
                if decision == "cancel" {
                    "cancelled"
                } else {
                    "declined"
                },
            )
        }
        "item/permissions/requestApproval" => (
            json!({"permissions": {}, "scope": "turn"}),
            "permission grant",
            "denied",
        ),
        "item/tool/requestUserInput" => (json!({"answers": {}}), "user input", "empty response"),
        "mcpServer/elicitation/request" => (
            json!({"action": "decline", "content": null, "_meta": null}),
            "MCP elicitation",
            "declined",
        ),
        // An unrecognized request is never guessed at: the run fails with a
        // safe fact naming the protocol method and nothing else.
        other => {
            return Err(SubagentOutcomeV1::failed(
                SubagentStopReasonV1::Unknown,
                format!(
                    "Product subagent failure (product: Codex; stage: turn; category: unknown; request: {})",
                    bounded_method(other)
                ),
            ));
        }
    };
    state.record_diagnostic(format!(
        "Codex requested {request}; the unattended {mode} policy answered {decision}"
    ));
    send(wire, &json!({"id": id, "result": result}), "turn")
}

/// The unattended decision for one approval request, or a protocol failure
/// when the app-server offers no decision this policy may take.
fn unattended_decision(params: &Value) -> Result<&'static str, SubagentOutcomeV1> {
    match params.get("availableDecisions") {
        None | Some(Value::Null) => Ok("decline"),
        Some(Value::Array(available)) => {
            let offered = |decision: &str| {
                available
                    .iter()
                    .any(|value| value.as_str() == Some(decision))
            };
            if offered("cancel") {
                Ok("cancel")
            } else if offered("decline") {
                Ok("decline")
            } else {
                Err(protocol_stop(
                    "turn",
                    "the app-server offered no unattended approval decision",
                ))
            }
        }
        Some(_) => Err(protocol_stop(
            "turn",
            "the app-server sent an invalid approval request",
        )),
    }
}

/// A method name is a protocol identifier; it is bounded and never echoed with payload.
fn bounded_method(method: &str) -> String {
    method
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '_' | '-')
        })
        .take(128)
        .collect()
}

fn commit_turn_id(state: &mut RunState, turn_id: &str) {
    state.turn_id = Some(turn_id.to_owned());
    let buffered = std::mem::take(&mut state.early_notifications);
    for (method, params) in buffered {
        let _ = observe_notification(state, &method, &params);
    }
}

fn interrupt(wire: &mut dyn CodexProtocolWireV1, state: &mut RunState) {
    if state.interruption_sent {
        return;
    }
    let Some(turn_id) = state.turn_id.clone() else {
        return;
    };
    state.interruption_sent = true;
    let _ = wire.send(&json!({
        "method": "turn/interrupt",
        "id": 4,
        "params": {"threadId": state.thread_id, "turnId": turn_id},
    }));
}

fn send_request(
    wire: &mut dyn CodexProtocolWireV1,
    id: u64,
    method: &str,
    params: &Value,
    stage: &'static str,
) -> RunResult<()> {
    send(
        wire,
        &json!({"method": method, "id": id, "params": params}),
        stage,
    )
}

fn send(wire: &mut dyn CodexProtocolWireV1, message: &Value, stage: &'static str) -> RunResult<()> {
    wire.send(message).map_err(|error| wire_stop(stage, error))
}

fn await_response(
    wire: &mut dyn CodexProtocolWireV1,
    state: &mut RunState,
    config: &CodexOneShotConfigV1,
    id: u64,
    deadline: Instant,
    cancellation: &CancellationToken,
    limits: CodexOneShotLimitsV1,
    stage: &'static str,
) -> RunResult<Value> {
    let handshake_deadline = (Instant::now() + limits.handshake_timeout).min(deadline);
    loop {
        if cancellation.is_cancelled() {
            return Err(SubagentOutcomeV1::aborted());
        }
        let now = Instant::now();
        if now >= handshake_deadline {
            return Err(SubagentOutcomeV1::failed(
                SubagentStopReasonV1::Limit,
                failure_diagnostic(stage, "limit", None),
            ));
        }
        let wait = limits
            .poll_interval
            .min(handshake_deadline.saturating_duration_since(now));
        match wire.receive(wait) {
            Ok(message) => {
                if let Some(response_id) = message.get("id").and_then(Value::as_u64) {
                    if message.get("method").is_none() && response_id == id {
                        if message.get("error").is_some() {
                            return Err(SubagentOutcomeV1::failed(
                                SubagentStopReasonV1::ProductError,
                                failure_diagnostic(stage, "product-error", None),
                            ));
                        }
                        return message.get("result").cloned().ok_or_else(|| {
                            protocol_stop(stage, "the app-server response was invalid")
                        });
                    }
                }
                handle_message(wire, state, &message, config)?;
            }
            Err(CodexWireErrorV1::TimedOut) => {}
            Err(error) => return Err(wire_stop(stage, error)),
        }
    }
}

fn path_text(path: &Path) -> RunResult<&str> {
    path.to_str()
        .filter(|value| !value.is_empty() && !value.contains('\0') && value.len() <= 16 * 1_024)
        .ok_or_else(|| {
            SubagentOutcomeV1::failed(
                SubagentStopReasonV1::ProductError,
                "the delegated working directory is not a usable path",
            )
        })
}

fn merge_object(target: &mut Value, source: Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn protocol_stop(stage: &'static str, diagnostic: &str) -> SubagentOutcomeV1 {
    SubagentOutcomeV1::failed(
        SubagentStopReasonV1::ProductError,
        format!(
            "{diagnostic} ({})",
            failure_diagnostic(stage, "invalid-result", None)
        ),
    )
}

fn failure_diagnostic(stage: &str, category: &str, http_status: Option<u64>) -> String {
    let http = http_status.map_or_else(String::new, |status| format!("; HTTP status: {status}"));
    format!("Product subagent failure (product: Codex; stage: {stage}; category: {category}{http})")
}

fn wire_stop(stage: &'static str, error: CodexWireErrorV1) -> SubagentOutcomeV1 {
    let (reason, category) = match error {
        CodexWireErrorV1::Exited => (SubagentStopReasonV1::Process, "process"),
        CodexWireErrorV1::Transport => (SubagentStopReasonV1::Transport, "transport"),
        CodexWireErrorV1::Protocol => (SubagentStopReasonV1::ProductError, "invalid-result"),
        CodexWireErrorV1::MessageTooLarge | CodexWireErrorV1::MessageLimit => {
            (SubagentStopReasonV1::Limit, "limit")
        }
        CodexWireErrorV1::TimedOut => (SubagentStopReasonV1::Transport, "transport"),
    };
    SubagentOutcomeV1::failed(reason, failure_diagnostic(stage, category, None))
}

/// The one-shot Codex backend registered under a configured target name.
pub struct CodexOneShotBackendV1 {
    name: String,
    config: CodexOneShotConfigV1,
}

impl CodexOneShotBackendV1 {
    /// Validates and prepares one Codex backend. No process starts here.
    pub fn new(config: CodexOneShotConfigV1) -> Result<Self, CodexOneShotErrorV1> {
        config.validate()?;
        Ok(Self {
            name: config.name.clone(),
            config,
        })
    }
}

impl ExternalAgentBackendV1 for CodexOneShotBackendV1 {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SubagentBackendCapabilitiesV1 {
        SubagentBackendCapabilitiesV1 {
            agent_options: true,
            ..SubagentBackendCapabilitiesV1::external_agent()
        }
    }

    fn run(
        &self,
        request: &OneShotDelegationV1,
        cancellation: &CancellationToken,
    ) -> SubagentOutcomeV1 {
        let mut wire = match StdioCodexWireV1::spawn(&self.config) {
            Ok(wire) => wire,
            Err(error) => {
                return SubagentOutcomeV1::failed(
                    SubagentStopReasonV1::Process,
                    format!("Codex could not start: {error}"),
                );
            }
        };
        run_codex_one_shot(&mut wire, &self.config, request, cancellation)
    }
}

/// Why one Codex one-shot delegation could not be prepared.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CodexOneShotErrorV1 {
    /// The backend name is not a valid stable identifier.
    #[error("Codex backend name is not a valid stable identifier")]
    InvalidBackendName,
    /// The configured executable is not an absolute path to a file.
    #[error("Codex executable must be an absolute path to a regular file")]
    InvalidExecutable,
    /// The argument vector does not start the app-server subcommand.
    #[error("Codex arguments must begin with the explicit 'app-server' subcommand")]
    MissingAppServerSubcommand,
    /// The argument vector is invalid or too large.
    #[error("Codex argument vector is invalid or too large")]
    InvalidArguments,
    /// The environment overlay is invalid or too large.
    #[error("Codex environment overlay is invalid or too large")]
    InvalidEnvironment,
    /// The runtime bounds are invalid.
    #[error("Codex runtime bounds are invalid")]
    InvalidLimits,
    /// The child process could not be started.
    #[error("the Codex app-server process could not be started")]
    Launch,
    /// The transport failed.
    #[error("the Codex app-server transport failed")]
    Transport,
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

    use aworkit_protocol::StableId;
    use serde_json::{Value, json};

    use super::*;

    /// A scripted app-server peer: messages are served in order and every send
    /// is recorded for assertions.
    struct ScriptedWireV1 {
        scripted: VecDeque<Value>,
        sent: Vec<Value>,
        cancel_after: Option<(usize, CancellationToken)>,
        served: usize,
        /// A live peer blocks instead of exiting once the script runs out.
        quiet_when_exhausted: bool,
    }

    impl ScriptedWireV1 {
        fn new(scripted: Vec<Value>) -> Self {
            Self {
                scripted: scripted.into(),
                sent: Vec::new(),
                cancel_after: None,
                served: 0,
                quiet_when_exhausted: false,
            }
        }

        fn cancelling_after(scripted: Vec<Value>, served: usize, token: CancellationToken) -> Self {
            Self {
                cancel_after: Some((served, token)),
                ..Self::new(scripted)
            }
        }

        fn response(id: u64, result: Value) -> Value {
            json!({"id": id, "result": result})
        }

        fn notification(method: &str, params: Value) -> Value {
            json!({"method": method, "params": params})
        }

        fn server_request(id: u64, method: &str, params: Value) -> Value {
            json!({"id": id, "method": method, "params": params})
        }

        fn sent_method(&self, method: &str) -> Option<&Value> {
            self.sent
                .iter()
                .find(|message| message.get("method").and_then(Value::as_str) == Some(method))
        }
    }

    impl CodexProtocolWireV1 for ScriptedWireV1 {
        fn send(&mut self, message: &Value) -> Result<(), CodexWireErrorV1> {
            self.sent.push(message.clone());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Value, CodexWireErrorV1> {
            let Some(message) = self.scripted.pop_front() else {
                return Err(if self.quiet_when_exhausted {
                    CodexWireErrorV1::TimedOut
                } else {
                    CodexWireErrorV1::Exited
                });
            };
            self.served += 1;
            if let Some((after, token)) = &self.cancel_after {
                if self.served >= *after {
                    token.cancel();
                }
            }
            Ok(message)
        }
    }

    fn config(mode: CodexPermissionModeV1) -> CodexOneShotConfigV1 {
        CodexOneShotConfigV1 {
            name: "codex".to_owned(),
            executable: PathBuf::from("/usr/bin/codex"),
            arguments: vec!["app-server".to_owned(), "--stdio".to_owned()],
            working_directory: Some(PathBuf::from("/tmp")),
            inherit_environment: true,
            environment: Vec::new(),
            permission_mode: mode,
            limits: CodexOneShotLimitsV1 {
                poll_interval: Duration::from_millis(1),
                handshake_timeout: Duration::from_secs(5),
                ..CodexOneShotLimitsV1::default()
            },
        }
    }

    fn request() -> OneShotDelegationV1 {
        OneShotDelegationV1 {
            run_id: StableId::parse("run.codex").expect("stable id"),
            task: "Summarize the delegation seam".to_owned(),
            working_directory: PathBuf::from("/tmp"),
            deadline: Duration::from_secs(30),
            options: Default::default(),
        }
    }

    /// The fixed handshake prefix every scripted run starts with.
    fn handshake(thread_id: &str, turn_id: &str, ephemeral: bool) -> Vec<Value> {
        vec![
            ScriptedWireV1::response(
                1,
                json!({"userAgent": "codex/0.146.0", "platformOs": "linux"}),
            ),
            ScriptedWireV1::response(
                2,
                json!({"thread": {"id": thread_id, "ephemeral": ephemeral}}),
            ),
            ScriptedWireV1::response(3, json!({"turn": {"id": turn_id}})),
        ]
    }

    fn agent_message(turn_id: &str, text: &str, phase: Value) -> Value {
        ScriptedWireV1::notification(
            "item/completed",
            json!({
                "threadId": "thread.1",
                "turnId": turn_id,
                "item": {"type": "agentMessage", "text": text, "phase": phase},
            }),
        )
    }

    fn turn_completed(turn_id: &str, status: &str) -> Value {
        ScriptedWireV1::notification(
            "turn/completed",
            json!({"threadId": "thread.1", "turn": {"id": turn_id, "status": status}}),
        )
    }

    fn run(scripted: Vec<Value>, mode: CodexPermissionModeV1) -> (SubagentOutcomeV1, Vec<Value>) {
        let mut wire = ScriptedWireV1::new(scripted);
        let outcome = run_codex_one_shot(
            &mut wire,
            &config(mode),
            &request(),
            &CancellationToken::default(),
        );
        (outcome, wire.sent)
    }

    #[test]
    fn a_completed_turn_returns_the_final_answer() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "Planning", json!("commentary")));
        scripted.push(agent_message(
            "turn.1",
            "The seam is bounded",
            json!("final_answer"),
        ));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, sent) = run(scripted, CodexPermissionModeV1::Never);

        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        assert_eq!(outcome.answer.as_deref(), Some("The seam is bounded"));
        assert!(outcome.diagnostic.is_none());

        let initialize = sent.first().expect("initialize was sent");
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            sent.iter()
                .filter(|message| message["method"] == "initialized")
                .count(),
            1
        );
        let thread_start = sent
            .iter()
            .find(|message| message["method"] == "thread/start")
            .expect("thread/start was sent");
        assert_eq!(thread_start["params"]["cwd"], "/tmp");
        assert_eq!(thread_start["params"]["ephemeral"], true);
        assert_eq!(thread_start["params"]["approvalPolicy"], "never");
        let turn_start = sent
            .iter()
            .find(|message| message["method"] == "turn/start")
            .expect("turn/start was sent");
        assert_eq!(turn_start["params"]["threadId"], "thread.1");
        assert_eq!(
            turn_start["params"]["input"],
            json!([{"type": "text", "text": "Summarize the delegation seam", "text_elements": []}])
        );
    }

    #[test]
    fn an_unphased_answer_is_the_compatibility_fallback() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "Older product", Value::Null));
        scripted.push(agent_message("turn.1", "Newer unphased", Value::Null));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.answer.as_deref(), Some("Newer unphased"));
    }

    #[test]
    fn a_model_override_and_effort_reach_the_protocol() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "Done", json!("final_answer")));
        scripted.push(turn_completed("turn.1", "completed"));
        let mut request = request();
        request.options.model = Some("gpt-5.6-sol".to_owned());
        request.options.reasoning_effort = Some("high".to_owned());
        let mut wire = ScriptedWireV1::new(scripted);
        let outcome = run_codex_one_shot(
            &mut wire,
            &config(CodexPermissionModeV1::Never),
            &request,
            &CancellationToken::default(),
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        assert_eq!(
            wire.sent_method("thread/start").expect("thread/start")["params"]["model"],
            "gpt-5.6-sol"
        );
        assert_eq!(
            wire.sent_method("turn/start").expect("turn/start")["params"]["effort"],
            "high"
        );
    }

    #[test]
    fn a_declared_final_answer_wins_over_a_later_unphased_message() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "Final", json!("final_answer")));
        scripted.push(agent_message("turn.1", "Trailing note", Value::Null));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.answer.as_deref(), Some("Final"));
    }

    #[test]
    fn a_completed_turn_without_an_answer_is_an_invalid_result() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "   ", json!("final_answer")));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::InvalidResult);
        assert!(outcome.answer.is_none());
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("category: invalid-result"))
        );
    }

    #[test]
    fn a_failed_turn_reports_its_classified_category() {
        for (info, reason, category) in [
            (
                json!("contextWindowExceeded"),
                SubagentStopReasonV1::Limit,
                "limit",
            ),
            (
                json!("usageLimitExceeded"),
                SubagentStopReasonV1::Limit,
                "limit",
            ),
            (
                json!("internalServerError"),
                SubagentStopReasonV1::Service,
                "service",
            ),
            (
                json!("sandboxError"),
                SubagentStopReasonV1::AccessPolicy,
                "access-policy",
            ),
            (
                json!("badRequest"),
                SubagentStopReasonV1::ProductError,
                "product-error",
            ),
            (
                json!("somethingNew"),
                SubagentStopReasonV1::Unknown,
                "unknown",
            ),
        ] {
            let mut scripted = handshake("thread.1", "turn.1", true);
            scripted.push(ScriptedWireV1::notification(
                "turn/completed",
                json!({
                    "threadId": "thread.1",
                    "turn": {
                        "id": "turn.1",
                        "status": "failed",
                        "error": {"codexErrorInfo": info},
                    },
                }),
            ));
            let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
            assert_eq!(outcome.stop_reason, reason);
            assert!(
                outcome
                    .diagnostic
                    .as_deref()
                    .is_some_and(|diagnostic| diagnostic.contains(category)),
                "expected {category} in {:?}",
                outcome.diagnostic
            );
        }
    }

    #[test]
    fn a_transport_failure_keeps_its_http_status() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(ScriptedWireV1::notification(
            "turn/completed",
            json!({
                "threadId": "thread.1",
                "turn": {
                    "id": "turn.1",
                    "status": "failed",
                    "error": {
                        "codexErrorInfo": {"responseStreamDisconnected": {"httpStatusCode": 503}},
                    },
                },
            }),
        ));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Transport);
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("HTTP status: 503"))
        );
    }

    #[test]
    fn an_interrupted_turn_is_aborted() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(turn_completed("turn.1", "interrupted"));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
    }

    #[test]
    fn an_exited_process_is_reported_as_a_process_failure() {
        let (outcome, _) = run(
            handshake("thread.1", "turn.1", true),
            CodexPermissionModeV1::Never,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Process);
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("category: process"))
        );
    }

    #[test]
    fn approval_requests_are_answered_without_a_human() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(ScriptedWireV1::server_request(
            91,
            "item/commandExecution/requestApproval",
            json!({
                "threadId": "thread.1",
                "turnId": "turn.1",
                "availableDecisions": ["accept", "decline", "cancel"],
            }),
        ));
        scripted.push(ScriptedWireV1::server_request(
            92,
            "item/fileChange/requestApproval",
            json!({"threadId": "thread.1", "turnId": "turn.1"}),
        ));
        scripted.push(ScriptedWireV1::server_request(
            93,
            "item/permissions/requestApproval",
            json!({"threadId": "thread.1", "turnId": "turn.1"}),
        ));
        scripted.push(ScriptedWireV1::server_request(
            94,
            "item/tool/requestUserInput",
            json!({"threadId": "thread.1", "turnId": "turn.1", "questions": []}),
        ));
        scripted.push(ScriptedWireV1::server_request(
            95,
            "mcpServer/elicitation/request",
            json!({"threadId": "thread.1", "turnId": null}),
        ));
        scripted.push(agent_message(
            "turn.1",
            "Done under policy",
            json!("final_answer"),
        ));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, sent) = run(scripted, CodexPermissionModeV1::Never);

        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        let answer = |id: u64| {
            sent.iter()
                .find(|message| message.get("id").and_then(Value::as_u64) == Some(id))
                .expect("server request was answered")
        };
        assert_eq!(answer(91)["result"], json!({"decision": "cancel"}));
        assert_eq!(answer(92)["result"], json!({"decision": "decline"}));
        assert_eq!(
            answer(93)["result"],
            json!({"permissions": {}, "scope": "turn"})
        );
        assert_eq!(answer(94)["result"], json!({"answers": {}}));
        assert_eq!(
            answer(95)["result"],
            json!({"action": "decline", "content": null, "_meta": null})
        );
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("MCP elicitation")),
            "expected the latest unattended fact in {:?}",
            outcome.diagnostic
        );
    }

    #[test]
    fn an_unknown_server_request_fails_the_run_without_leaking_payload() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(ScriptedWireV1::server_request(
            96,
            "item/somethingNew/requestApproval",
            json!({"secret": "do-not-echo"}),
        ));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Unknown);
        let diagnostic = outcome.diagnostic.expect("diagnostic");
        assert!(diagnostic.contains("item/somethingNew/requestApproval"));
        assert!(!diagnostic.contains("do-not-echo"));
    }

    #[test]
    fn another_threads_and_turns_messages_are_ignored() {
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(ScriptedWireV1::notification(
            "item/completed",
            json!({
                "threadId": "thread.other",
                "turnId": "turn.1",
                "item": {"type": "agentMessage", "text": "Foreign", "phase": "final_answer"},
            }),
        ));
        scripted.push(ScriptedWireV1::notification(
            "item/completed",
            json!({
                "threadId": "thread.1",
                "turnId": "turn.other",
                "item": {"type": "agentMessage", "text": "Other turn", "phase": "final_answer"},
            }),
        ));
        scripted.push(agent_message("turn.1", "Mine", json!("final_answer")));
        scripted.push(turn_completed("turn.1", "completed"));
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.answer.as_deref(), Some("Mine"));
    }

    #[test]
    fn a_non_ephemeral_thread_is_refused() {
        let scripted = handshake("thread.1", "turn.1", false);
        let (outcome, _) = run(scripted, CodexPermissionModeV1::Never);
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::ProductError);
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("ephemeral"))
        );
    }

    #[test]
    fn permission_modes_map_to_the_exact_thread_fields() {
        for (mode, expected) in [
            (
                CodexPermissionModeV1::Never,
                json!({"approvalPolicy": "never"}),
            ),
            (
                CodexPermissionModeV1::ApproveForMe,
                json!({
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "auto_review",
                    "sandbox": "workspace-write",
                }),
            ),
            (
                CodexPermissionModeV1::DangerouslyBypassApprovalsAndSandbox,
                json!({"approvalPolicy": "never", "sandbox": "danger-full-access"}),
            ),
        ] {
            let mut scripted = handshake("thread.1", "turn.1", true);
            scripted.push(agent_message("turn.1", "Done", json!("final_answer")));
            scripted.push(turn_completed("turn.1", "completed"));
            let (outcome, sent) = run(scripted, mode);
            assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
            let params = &sent
                .iter()
                .find(|message| message["method"] == "thread/start")
                .expect("thread/start was sent")["params"];
            for (key, value) in expected.as_object().expect("object") {
                assert_eq!(&params[key], value, "mode {mode:?} field {key}");
            }
        }
    }

    #[test]
    fn a_cancelled_run_interrupts_the_turn_and_settles_aborted() {
        let token = CancellationToken::default();
        let mut scripted = handshake("thread.1", "turn.1", true);
        scripted.push(agent_message("turn.1", "partial", json!("commentary")));
        let mut wire = ScriptedWireV1::cancelling_after(scripted, 4, token.clone());
        let outcome = run_codex_one_shot(
            &mut wire,
            &config(CodexPermissionModeV1::Never),
            &request(),
            &token,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
        let interrupt = wire
            .sent_method("turn/interrupt")
            .expect("turn/interrupt was sent");
        assert_eq!(interrupt["params"]["threadId"], "thread.1");
        assert_eq!(interrupt["params"]["turnId"], "turn.1");
    }

    #[test]
    fn an_expired_deadline_settles_as_a_limit() {
        let mut wire = ScriptedWireV1::new(handshake("thread.1", "turn.1", true));
        wire.quiet_when_exhausted = true;
        let mut request = request();
        request.deadline = Duration::from_millis(5);
        let outcome = run_codex_one_shot(
            &mut wire,
            &config(CodexPermissionModeV1::Never),
            &request,
            &CancellationToken::default(),
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Limit);
    }

    #[test]
    fn a_pre_cancelled_run_never_starts_a_thread() {
        let token = CancellationToken::default();
        token.cancel();
        let mut wire = ScriptedWireV1::new(handshake("thread.1", "turn.1", true));
        let outcome = run_codex_one_shot(
            &mut wire,
            &config(CodexPermissionModeV1::Never),
            &request(),
            &token,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
        assert!(wire.sent.is_empty());
    }

    #[test]
    fn configuration_validation_refuses_hostile_input() {
        let mut invalid = config(CodexPermissionModeV1::Never);
        invalid.arguments = vec!["--stdio".to_owned()];
        assert_eq!(
            CodexOneShotBackendV1::new(invalid).err(),
            Some(CodexOneShotErrorV1::MissingAppServerSubcommand)
        );

        let mut relative = config(CodexPermissionModeV1::Never);
        relative.executable = PathBuf::from("codex");
        assert_eq!(
            CodexOneShotBackendV1::new(relative).err(),
            Some(CodexOneShotErrorV1::InvalidExecutable)
        );

        let mut unlimited = config(CodexPermissionModeV1::Never);
        unlimited.limits.maximum_messages = 0;
        assert_eq!(
            CodexOneShotBackendV1::new(unlimited).err(),
            Some(CodexOneShotErrorV1::InvalidLimits)
        );

        let backend = CodexOneShotBackendV1::new(config(CodexPermissionModeV1::Never))
            .expect("valid configuration");
        assert_eq!(backend.name(), "codex");
        assert_eq!(
            backend.capabilities(),
            SubagentBackendCapabilitiesV1 {
                agent_options: true,
                ..SubagentBackendCapabilitiesV1::external_agent()
            }
        );
        assert!(!backend.inherits_parent_context());
    }

    #[test]
    fn a_startup_failure_is_reported_as_a_process_outcome() {
        // A backend whose executable cannot be launched still settles with a
        // safe diagnostic instead of panicking or hanging.
        let mut missing = config(CodexPermissionModeV1::Never);
        missing.executable = PathBuf::from("/nonexistent/codex");
        let backend = CodexOneShotBackendV1::new(missing).expect("configuration is valid");
        let outcome = backend.run(&request(), &CancellationToken::default());
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Process);
        assert!(outcome.answer.is_none());
    }

    #[test]
    fn the_registry_accepts_a_codex_backend() {
        let mut registry = crate::external_agent::ExternalAgentBackendRegistryV1::new();
        registry
            .register(Arc::new(
                CodexOneShotBackendV1::new(config(CodexPermissionModeV1::Never))
                    .expect("valid configuration"),
            ))
            .expect("registers");
        assert_eq!(registry.names(), vec!["codex"]);
    }
}
