//! One-shot Claude Code delegation over the product's non-interactive CLI.
//!
//! One delegation spawns one `claude --print` process, submits the task on its
//! standard input, and settles with the child's final answer or a safe failure
//! diagnostic. The task never appears in the argument vector, so it cannot leak
//! into a process listing; the prompt is the only thing Aworkit writes.
//!
//! Claude Code has no ACP or server mode, so this backend drives the CLI's
//! documented one-shot surface: `--print --output-format stream-json` with the
//! profile's permission mode, an optional model override and an optional
//! reasoning effort. Native Claude settings and authentication stay
//! authoritative, and there is no host-PATH fallback beyond the configured
//! executable.
//!
//! The child runs unattended. The permission mode decides what it may do, and
//! any denial it reports is surfaced as a bounded fact rather than silently
//! swallowed. Assistant reasoning, tool traffic and stderr never cross this
//! boundary: only the final answer and fixed failure facts do.

use std::{
    io::Write,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use aworkit_protocol::StableId;
use serde_json::Value;
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

/// Default bound for one stream-json event. Assistant messages can carry large
/// tool results, so this is far above one console line.
const DEFAULT_MAXIMUM_MESSAGE_BYTES: usize = 8 * 1_024 * 1_024;
/// Default bound for the whole run's event count.
const DEFAULT_MAXIMUM_MESSAGES: usize = 100_000;
/// How long one read blocks before the run rechecks its bounds.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Reasoning-effort values this backend accepts. Claude Code rejects the rest.
pub const ACCEPTED_REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Largest accepted extra argument vector.
const MAXIMUM_ARGUMENTS: usize = 128;
/// Largest accepted environment overlay.
const MAXIMUM_ENVIRONMENT_ENTRIES: usize = 256;
/// How much of a product error message is inspected for classification. Only
/// fixed facts are ever returned to the parent.
const MAXIMUM_INSPECTED_ERROR_CHARS: usize = 512;

/// Non-interactive permission policy fixed for every run of one backend.
///
/// The values match Claude Code's own `--permission-mode` choices so a reader
/// can compare them directly with the product documentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudePermissionModeV1 {
    /// Deny anything not already authorized instead of prompting.
    DontAsk,
    /// Accept file edits; remaining prompts are denied.
    AcceptEdits,
    /// Let the product's classifier allow or deny requests.
    Auto,
    /// Plan only; execution approval is denied and the plan is the answer.
    Plan,
    /// Explicitly bypass permission checks. Must be selected on purpose.
    BypassPermissions,
}

impl ClaudePermissionModeV1 {
    /// The exact `--permission-mode` value.
    fn cli_value(self) -> &'static str {
        match self {
            Self::DontAsk => "dontAsk",
            Self::AcceptEdits => "acceptEdits",
            Self::Auto => "auto",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
        }
    }

    /// Stable, non-secret mode name used in diagnostics.
    fn diagnostic_name(self) -> &'static str {
        self.cli_value()
    }
}

/// Runtime bounds for one one-shot Claude Code delegation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaudeOneShotLimitsV1 {
    /// Largest accepted stream event.
    pub maximum_message_bytes: usize,
    /// Largest accepted stream event count for the whole run.
    pub maximum_messages: usize,
    /// How long one read blocks before the run rechecks its bounds.
    pub poll_interval: Duration,
}

impl Default for ClaudeOneShotLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_message_bytes: DEFAULT_MAXIMUM_MESSAGE_BYTES,
            maximum_messages: DEFAULT_MAXIMUM_MESSAGES,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Exact argv-only configuration for one Claude Code backend.
pub struct ClaudeOneShotConfigV1 {
    /// Registry name this backend answers to.
    pub name: String,
    /// Resolved Claude Code executable.
    pub executable: PathBuf,
    /// Extra fixed arguments appended after the adapter's own flags. The
    /// delegated task is never placed here.
    pub arguments: Vec<String>,
    /// Working directory of the child process.
    pub working_directory: Option<PathBuf>,
    /// Whether the child inherits the ambient environment. Claude Code normally
    /// needs its existing configuration and login state.
    pub inherit_environment: bool,
    /// Explicit environment overlay for this transient process only.
    pub environment: Vec<CodexAppServerEnvironmentV1>,
    /// Non-interactive permission policy fixed for every run.
    pub permission_mode: ClaudePermissionModeV1,
    /// Runtime bounds.
    pub limits: ClaudeOneShotLimitsV1,
}

impl ClaudeOneShotConfigV1 {
    fn validate(&self) -> Result<(), ClaudeOneShotErrorV1> {
        if StableId::parse(self.name.clone()).is_err() {
            return Err(ClaudeOneShotErrorV1::InvalidBackendName);
        }
        if !self.executable.is_absolute() {
            return Err(ClaudeOneShotErrorV1::InvalidExecutable);
        }
        if self.arguments.len() > MAXIMUM_ARGUMENTS
            || self
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
            || self.arguments.iter().map(String::len).sum::<usize>() > 16 * 1_024
        {
            return Err(ClaudeOneShotErrorV1::InvalidArguments);
        }
        if self.environment.len() > MAXIMUM_ENVIRONMENT_ENTRIES {
            return Err(ClaudeOneShotErrorV1::InvalidEnvironment);
        }
        let limits = self.limits;
        if limits.maximum_message_bytes == 0
            || limits.maximum_message_bytes > 64 * 1_024 * 1_024
            || limits.maximum_messages == 0
            || limits.maximum_messages > 10_000_000
            || limits.poll_interval.is_zero()
            || limits.poll_interval > Duration::from_secs(5)
        {
            return Err(ClaudeOneShotErrorV1::InvalidLimits);
        }
        Ok(())
    }
}

/// One protocol failure observed on the Claude Code stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeWireErrorV1 {
    /// The transport failed.
    Transport,
    /// The child closed the stream or exited.
    Exited,
    /// No event arrived inside the requested wait.
    TimedOut,
    /// An event was not a valid protocol object.
    Protocol,
    /// An event exceeded the accepted bound.
    MessageTooLarge,
    /// The run exceeded its accepted event count.
    MessageLimit,
}

/// The bounded event stream one one-shot run reads.
///
/// Implementations are per-run and own their own process; the seam exists so the
/// classification logic can be exercised without a child process.
pub trait ClaudeEventStreamV1: Send {
    /// Submits the delegated task on the child's standard input and closes it.
    fn submit(&mut self, task: &str) -> Result<(), ClaudeWireErrorV1>;
    /// Receives the next stream-json event, waiting at most `timeout`.
    fn receive(&mut self, timeout: Duration) -> Result<Value, ClaudeWireErrorV1>;
}

/// The stdio transport: one supervised CLI process, one reader thread.
pub struct StdioClaudeStreamV1 {
    child: Option<ManagedGroupChild>,
    writer: Option<std::process::ChildStdin>,
    receiver: Receiver<ReaderMessage>,
    readers: Vec<JoinHandle<()>>,
    limits: ClaudeOneShotLimitsV1,
    observed_messages: usize,
}

impl StdioClaudeStreamV1 {
    /// Spawns the configured CLI with a supervised process group.
    pub fn spawn(
        config: &ClaudeOneShotConfigV1,
        request: &OneShotDelegationV1,
    ) -> Result<Self, ClaudeOneShotErrorV1> {
        config.validate()?;
        if !config.executable.is_file() {
            return Err(ClaudeOneShotErrorV1::InvalidExecutable);
        }
        let mut command = Command::new(&config.executable);
        command
            .args(claude_arguments(config, request))
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

impl ClaudeEventStreamV1 for StdioClaudeStreamV1 {
    fn submit(&mut self, task: &str) -> Result<(), ClaudeWireErrorV1> {
        let writer = self.writer.as_mut().ok_or(ClaudeWireErrorV1::Transport)?;
        writer
            .write_all(task.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|_| ClaudeWireErrorV1::Transport)?;
        // Closing stdin tells the CLI the one-shot prompt is complete.
        drop(self.writer.take());
        Ok(())
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value, ClaudeWireErrorV1> {
        let message = self
            .receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => ClaudeWireErrorV1::TimedOut,
                mpsc::RecvTimeoutError::Disconnected => ClaudeWireErrorV1::Exited,
            })?;
        self.observed_messages = self.observed_messages.saturating_add(1);
        if self.observed_messages > self.limits.maximum_messages {
            return Err(ClaudeWireErrorV1::MessageLimit);
        }
        match message {
            ReaderMessage::Line(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| ClaudeWireErrorV1::Protocol)
            }
            ReaderMessage::TooLarge => Err(ClaudeWireErrorV1::MessageTooLarge),
            ReaderMessage::Transport => Err(ClaudeWireErrorV1::Transport),
            ReaderMessage::Eof => Err(ClaudeWireErrorV1::Exited),
        }
    }
}

impl Drop for StdioClaudeStreamV1 {
    fn drop(&mut self) {
        // Closing stdin first lets a healthy CLI exit on its own; the supervised
        // group is then terminated as a complete tree so no Claude descendant,
        // tool subprocess or language server survives the delegation.
        drop(self.writer.take());
        drop(self.child.take());
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

/// The exact argument vector for one delegation. The task is never included.
fn claude_arguments(config: &ClaudeOneShotConfigV1, request: &OneShotDelegationV1) -> Vec<String> {
    let mut arguments = vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--verbose".to_owned(),
        "--permission-mode".to_owned(),
        config.permission_mode.cli_value().to_owned(),
    ];
    arguments.extend(config.arguments.iter().cloned());
    if let Some(model) = request.options.model.as_deref() {
        arguments.push("--model".to_owned());
        arguments.push(model.to_owned());
    }
    if let Some(effort) = request.options.reasoning_effort.as_deref() {
        arguments.push("--effort".to_owned());
        arguments.push(effort.to_owned());
    }
    arguments
}

fn map_probe_error(
    error: crate::codex_app_server::CodexAppServerProbeError,
) -> ClaudeOneShotErrorV1 {
    match error {
        crate::codex_app_server::CodexAppServerProbeError::Launch
        | crate::codex_app_server::CodexAppServerProbeError::InvalidPath => {
            ClaudeOneShotErrorV1::Launch
        }
        _ => ClaudeOneShotErrorV1::Transport,
    }
}

/// A run failure already settled into its outward outcome.
type RunResult<T> = Result<T, SubagentOutcomeV1>;

/// Observable state of one in-flight run.
#[derive(Default)]
struct RunState {
    /// Latest main-thread assistant text, used when the terminal event carries
    /// no result string.
    last_assistant_text: Option<String>,
    /// Bounded unattended facts, for example denied actions.
    diagnostic: Option<String>,
    /// The authoritative terminal event.
    terminal: Option<Value>,
}

/// Runs one unattended one-shot Claude Code delegation to completion.
pub fn run_claude_one_shot(
    stream: &mut dyn ClaudeEventStreamV1,
    config: &ClaudeOneShotConfigV1,
    request: &OneShotDelegationV1,
    cancellation: &CancellationToken,
) -> SubagentOutcomeV1 {
    match drive(stream, config, request, cancellation) {
        Ok(outcome) => outcome,
        Err(outcome) => outcome,
    }
}

fn drive(
    stream: &mut dyn ClaudeEventStreamV1,
    config: &ClaudeOneShotConfigV1,
    request: &OneShotDelegationV1,
    cancellation: &CancellationToken,
) -> RunResult<SubagentOutcomeV1> {
    if let Err(error) = config.validate() {
        return Err(SubagentOutcomeV1::failed(
            SubagentStopReasonV1::ProductError,
            format!("Claude Code delegation configuration is invalid: {error}"),
        ));
    }
    if cancellation.is_cancelled() {
        return Err(SubagentOutcomeV1::aborted());
    }
    let deadline = Instant::now() + request.deadline;
    if let Err(error) = stream.submit(&request.task) {
        return Err(wire_stop("start", error));
    }

    let mut state = RunState::default();
    while state.terminal.is_none() {
        if cancellation.is_cancelled() {
            return Err(SubagentOutcomeV1::aborted());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(SubagentOutcomeV1::failed(
                SubagentStopReasonV1::Limit,
                failure_diagnostic("turn", "limit", None),
            ));
        }
        let wait = config
            .limits
            .poll_interval
            .min(deadline.saturating_duration_since(now));
        match stream.receive(wait) {
            Ok(event) => observe_event(&mut state, &event)?,
            Err(ClaudeWireErrorV1::TimedOut) => {}
            Err(error) => return Err(wire_stop("turn", error)),
        }
    }

    let terminal = state.terminal.clone().unwrap_or(Value::Null);
    settle(&state, &terminal, config)
}

/// Records one stream event. Unknown event types are ignored: the terminal
/// `result` event is authoritative, and the stream schema evolves with the
/// product.
fn observe_event(state: &mut RunState, event: &Value) -> RunResult<()> {
    match event.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            // Only this thread's own assistant text is the delegated answer.
            // Forwarded subagent traffic carries a parent tool-use id.
            let main_thread = event.get("parent_tool_use_id").is_none_or(Value::is_null);
            if main_thread {
                if let Some(text) = assistant_text(event) {
                    state.last_assistant_text = Some(text);
                }
            }
            Ok(())
        }
        Some("result") => {
            state.terminal = Some(event.clone());
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The concatenated text blocks of one assistant message.
fn assistant_text(event: &Value) -> Option<String> {
    let blocks = event.get("message")?.get("content")?.as_array()?;
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(part) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        if part.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(part);
    }
    (!text.trim().is_empty()).then_some(text)
}

/// Settles the run from the authoritative terminal event.
fn settle(
    state: &RunState,
    terminal: &Value,
    config: &ClaudeOneShotConfigV1,
) -> RunResult<SubagentOutcomeV1> {
    let denied = denied_actions(terminal);
    let unattended = denied.map(|count| {
        format!(
            "Claude Code denied {count} action(s) under the unattended {} policy",
            config.permission_mode.diagnostic_name()
        )
    });
    // A terminal event without a boolean outcome cannot be trusted as success.
    let is_error = terminal
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if is_error {
        let (reason, category) = classify_failure(terminal);
        let mut outcome =
            SubagentOutcomeV1::failed(reason, failure_diagnostic("turn", category, None));
        if reason == SubagentStopReasonV1::AccessPolicy && is_authentication_failure(terminal) {
            outcome.diagnostic = Some(bounded(format!(
                "{}; Claude Code is not authenticated in the child environment, sign in with the CLI first",
                failure_diagnostic("turn", category, None)
            )));
        }
        return Err(outcome);
    }
    let answer = terminal
        .get("result")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| state.last_assistant_text.clone());
    let Some(answer) = answer else {
        return Err(SubagentOutcomeV1::failed(
            SubagentStopReasonV1::InvalidResult,
            failure_diagnostic("turn", "invalid-result", None),
        ));
    };
    let mut outcome = SubagentOutcomeV1::completed(answer).map_err(|_| {
        SubagentOutcomeV1::failed(
            SubagentStopReasonV1::InvalidResult,
            failure_diagnostic("turn", "invalid-result", None),
        )
    })?;
    if let Some(unattended) = unattended.or_else(|| state.diagnostic.clone()) {
        outcome.diagnostic = Some(bounded(unattended));
    }
    Ok(outcome)
}

/// The classification of a failed terminal event, using the product's own
/// terminal reason and HTTP status where it reports them.
fn classify_failure(terminal: &Value) -> (SubagentStopReasonV1, &'static str) {
    match terminal.get("terminal_reason").and_then(Value::as_str) {
        Some("aborted_streaming" | "aborted_tools" | "interrupted") => {
            (SubagentStopReasonV1::Aborted, "aborted")
        }
        Some("max_turns" | "budget_exceeded" | "context_window_exceeded") => {
            (SubagentStopReasonV1::Limit, "limit")
        }
        _ => match terminal.get("api_error_status").and_then(Value::as_u64) {
            Some(401 | 403) => (SubagentStopReasonV1::AccessPolicy, "access-policy"),
            Some(429 | 500..=599) => (SubagentStopReasonV1::Service, "service"),
            Some(400..=499) => (SubagentStopReasonV1::ProductError, "product-error"),
            _ if is_authentication_failure(terminal) => {
                (SubagentStopReasonV1::AccessPolicy, "access-policy")
            }
            _ => (SubagentStopReasonV1::ProductError, "product-error"),
        },
    }
}

/// Whether the product reported that the child is not signed in. Only a bounded
/// prefix of its own message is inspected, and only fixed facts are returned.
fn is_authentication_failure(terminal: &Value) -> bool {
    let Some(message) = terminal.get("result").and_then(Value::as_str) else {
        return false;
    };
    let inspected: String = message
        .chars()
        .take(MAXIMUM_INSPECTED_ERROR_CHARS)
        .collect();
    let lowered = inspected.to_lowercase();
    lowered.contains("not logged in")
        || lowered.contains("run /login")
        || lowered.contains("authentication")
        || lowered.contains("unauthorized")
}

/// How many actions the product denied under the selected permission mode.
fn denied_actions(terminal: &Value) -> Option<usize> {
    let denied = terminal.get("permission_denials")?.as_array()?;
    (!denied.is_empty()).then_some(denied.len())
}

fn bounded(value: String) -> String {
    if value.len() <= crate::external_agent::MAXIMUM_DIAGNOSTIC_BYTES {
        return value;
    }
    let mut end = crate::external_agent::MAXIMUM_DIAGNOSTIC_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn failure_diagnostic(stage: &str, category: &str, http_status: Option<u64>) -> String {
    let http = http_status.map_or_else(String::new, |status| format!("; HTTP status: {status}"));
    format!(
        "Product subagent failure (product: Claude Code; stage: {stage}; category: {category}{http})"
    )
}

fn wire_stop(stage: &'static str, error: ClaudeWireErrorV1) -> SubagentOutcomeV1 {
    let (reason, category) = match error {
        ClaudeWireErrorV1::Exited => (SubagentStopReasonV1::Process, "process"),
        ClaudeWireErrorV1::Transport => (SubagentStopReasonV1::Transport, "transport"),
        ClaudeWireErrorV1::Protocol => (SubagentStopReasonV1::ProductError, "invalid-result"),
        ClaudeWireErrorV1::MessageTooLarge | ClaudeWireErrorV1::MessageLimit => {
            (SubagentStopReasonV1::Limit, "limit")
        }
        ClaudeWireErrorV1::TimedOut => (SubagentStopReasonV1::Transport, "transport"),
    };
    SubagentOutcomeV1::failed(reason, failure_diagnostic(stage, category, None))
}

/// The one-shot Claude Code backend registered under a configured target name.
pub struct ClaudeOneShotBackendV1 {
    name: String,
    config: ClaudeOneShotConfigV1,
}

impl ClaudeOneShotBackendV1 {
    /// Validates and prepares one Claude Code backend. No process starts here.
    pub fn new(config: ClaudeOneShotConfigV1) -> Result<Self, ClaudeOneShotErrorV1> {
        config.validate()?;
        Ok(Self {
            name: config.name.clone(),
            config,
        })
    }
}

impl ExternalAgentBackendV1 for ClaudeOneShotBackendV1 {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> SubagentBackendCapabilitiesV1 {
        SubagentBackendCapabilitiesV1 {
            agent_options: true,
            ..SubagentBackendCapabilitiesV1::external_agent()
        }
    }

    fn reasoning_efforts(&self) -> Option<&'static [&'static str]> {
        Some(ACCEPTED_REASONING_EFFORTS)
    }

    fn run(
        &self,
        request: &OneShotDelegationV1,
        cancellation: &CancellationToken,
    ) -> SubagentOutcomeV1 {
        let mut stream = match StdioClaudeStreamV1::spawn(&self.config, request) {
            Ok(stream) => stream,
            Err(error) => {
                return SubagentOutcomeV1::failed(
                    SubagentStopReasonV1::Process,
                    format!("Claude Code could not start: {error}"),
                );
            }
        };
        run_claude_one_shot(&mut stream, &self.config, request, cancellation)
    }
}

/// Why one Claude Code delegation could not be prepared.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ClaudeOneShotErrorV1 {
    /// The backend name is not a valid stable identifier.
    #[error("Claude Code backend name is not a valid stable identifier")]
    InvalidBackendName,
    /// The configured executable is not an absolute path to a file.
    #[error("Claude Code executable must be an absolute path to a regular file")]
    InvalidExecutable,
    /// The extra argument vector is invalid or too large.
    #[error("Claude Code argument vector is invalid or too large")]
    InvalidArguments,
    /// The environment overlay is invalid or too large.
    #[error("Claude Code environment overlay is invalid or too large")]
    InvalidEnvironment,
    /// The runtime bounds are invalid.
    #[error("Claude Code runtime bounds are invalid")]
    InvalidLimits,
    /// The child process could not be started.
    #[error("the Claude Code process could not be started")]
    Launch,
    /// The transport failed.
    #[error("the Claude Code transport failed")]
    Transport,
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::PathBuf, time::Duration};

    use aworkit_protocol::StableId;
    use serde_json::{Value, json};

    use super::*;

    /// A scripted CLI stream: events are served in order and the submitted task
    /// is recorded for assertions.
    struct ScriptedStreamV1 {
        scripted: VecDeque<Value>,
        submitted: Vec<String>,
        cancel_after: Option<(usize, CancellationToken)>,
        served: usize,
        /// A live CLI blocks instead of exiting once the script runs out.
        quiet_when_exhausted: bool,
    }

    impl ScriptedStreamV1 {
        fn new(scripted: Vec<Value>) -> Self {
            Self {
                scripted: scripted.into(),
                submitted: Vec::new(),
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

        fn assistant(text: &str) -> Value {
            json!({
                "type": "assistant",
                "parent_tool_use_id": null,
                "message": {"content": [{"type": "text", "text": text}]},
            })
        }

        fn result(result: &str, is_error: bool) -> Value {
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": is_error,
                "result": result,
                "terminal_reason": if is_error { "api_error" } else { "completed" },
                "permission_denials": [],
            })
        }
    }

    impl ClaudeEventStreamV1 for ScriptedStreamV1 {
        fn submit(&mut self, task: &str) -> Result<(), ClaudeWireErrorV1> {
            self.submitted.push(task.to_owned());
            Ok(())
        }

        fn receive(&mut self, _timeout: Duration) -> Result<Value, ClaudeWireErrorV1> {
            let Some(event) = self.scripted.pop_front() else {
                return Err(if self.quiet_when_exhausted {
                    ClaudeWireErrorV1::TimedOut
                } else {
                    ClaudeWireErrorV1::Exited
                });
            };
            self.served += 1;
            if let Some((after, token)) = &self.cancel_after {
                if self.served >= *after {
                    token.cancel();
                }
            }
            Ok(event)
        }
    }

    fn config(mode: ClaudePermissionModeV1) -> ClaudeOneShotConfigV1 {
        ClaudeOneShotConfigV1 {
            name: "claude-code".to_owned(),
            executable: PathBuf::from("/usr/bin/claude"),
            arguments: Vec::new(),
            working_directory: Some(PathBuf::from("/tmp")),
            inherit_environment: true,
            environment: Vec::new(),
            permission_mode: mode,
            limits: ClaudeOneShotLimitsV1 {
                poll_interval: Duration::from_millis(1),
                ..ClaudeOneShotLimitsV1::default()
            },
        }
    }

    fn request() -> OneShotDelegationV1 {
        OneShotDelegationV1 {
            run_id: StableId::parse("run.claude").expect("stable id"),
            task: "Summarize the delegation seam".to_owned(),
            working_directory: PathBuf::from("/tmp"),
            deadline: Duration::from_secs(30),
            options: Default::default(),
        }
    }

    fn run(scripted: Vec<Value>, mode: ClaudePermissionModeV1) -> (SubagentOutcomeV1, Vec<String>) {
        let mut stream = ScriptedStreamV1::new(scripted);
        let outcome = run_claude_one_shot(
            &mut stream,
            &config(mode),
            &request(),
            &CancellationToken::default(),
        );
        (outcome, stream.submitted)
    }

    #[test]
    fn a_successful_run_returns_the_result_text() {
        let (outcome, submitted) = run(
            vec![
                ScriptedStreamV1::assistant("Let me look"),
                ScriptedStreamV1::result("The seam is bounded", false),
            ],
            ClaudePermissionModeV1::DontAsk,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        assert_eq!(outcome.answer.as_deref(), Some("The seam is bounded"));
        assert_eq!(submitted, vec!["Summarize the delegation seam"]);
    }

    #[test]
    fn the_task_never_appears_in_the_argument_vector() {
        let request = request();
        let arguments = claude_arguments(&config(ClaudePermissionModeV1::DontAsk), &request);
        assert!(arguments.contains(&"--print".to_owned()));
        assert!(arguments.contains(&"stream-json".to_owned()));
        assert!(arguments.contains(&"dontAsk".to_owned()));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.contains("Summarize the delegation seam")),
            "the delegated task must be submitted on stdin only: {arguments:?}"
        );
    }

    #[test]
    fn permission_mode_model_and_effort_reach_the_command_line() {
        for (mode, expected) in [
            (ClaudePermissionModeV1::DontAsk, "dontAsk"),
            (ClaudePermissionModeV1::AcceptEdits, "acceptEdits"),
            (ClaudePermissionModeV1::Auto, "auto"),
            (ClaudePermissionModeV1::Plan, "plan"),
            (
                ClaudePermissionModeV1::BypassPermissions,
                "bypassPermissions",
            ),
        ] {
            let mut request = request();
            request.options.model = Some("sonnet".to_owned());
            request.options.reasoning_effort = Some("high".to_owned());
            let arguments = claude_arguments(&config(mode), &request);
            assert!(
                arguments
                    .windows(2)
                    .any(|pair| pair == ["--permission-mode", expected]),
                "mode {expected} missing from {arguments:?}"
            );
            assert!(
                arguments
                    .windows(2)
                    .any(|pair| pair == ["--model", "sonnet"]),
                "model missing from {arguments:?}"
            );
            assert!(
                arguments
                    .windows(2)
                    .any(|pair| pair == ["--effort", "high"]),
                "effort missing from {arguments:?}"
            );
        }
    }

    #[test]
    fn the_backend_declares_the_efforts_the_cli_accepts() {
        let backend = ClaudeOneShotBackendV1::new(config(ClaudePermissionModeV1::DontAsk))
            .expect("valid configuration");
        assert_eq!(backend.name(), "claude-code");
        assert!(backend.capabilities().agent_options);
        assert_eq!(
            backend.reasoning_efforts(),
            Some(ACCEPTED_REASONING_EFFORTS)
        );
        assert!(!backend.inherits_parent_context());
        assert!(!ACCEPTED_REASONING_EFFORTS.contains(&"none"));
        assert!(!ACCEPTED_REASONING_EFFORTS.contains(&"minimal"));
    }

    #[test]
    fn an_assistant_message_is_the_fallback_answer() {
        let (outcome, _) = run(
            vec![
                ScriptedStreamV1::assistant("First thought"),
                json!({"type": "result", "is_error": false, "result": "   "}),
                ScriptedStreamV1::assistant("Final thought"),
            ],
            ClaudePermissionModeV1::DontAsk,
        );
        // The terminal event ends the run, so the latest assistant text before it wins.
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        assert_eq!(outcome.answer.as_deref(), Some("First thought"));
    }

    #[test]
    fn a_successful_run_without_any_answer_is_an_invalid_result() {
        let (outcome, _) = run(
            vec![json!({"type": "result", "is_error": false, "result": ""})],
            ClaudePermissionModeV1::DontAsk,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::InvalidResult);
        assert!(outcome.answer.is_none());
    }

    #[test]
    fn a_failed_run_is_classified_from_the_product_facts() {
        for (terminal, reason, category) in [
            (
                json!({
                    "type": "result", "is_error": true, "result": "rate limited",
                    "terminal_reason": "api_error", "api_error_status": 429,
                }),
                SubagentStopReasonV1::Service,
                "service",
            ),
            (
                json!({
                    "type": "result", "is_error": true, "result": "server error",
                    "terminal_reason": "api_error", "api_error_status": 503,
                }),
                SubagentStopReasonV1::Service,
                "service",
            ),
            (
                json!({
                    "type": "result", "is_error": true, "result": "bad request",
                    "terminal_reason": "api_error", "api_error_status": 400,
                }),
                SubagentStopReasonV1::ProductError,
                "product-error",
            ),
            (
                json!({
                    "type": "result", "is_error": true, "result": "budget",
                    "terminal_reason": "budget_exceeded",
                }),
                SubagentStopReasonV1::Limit,
                "limit",
            ),
            (
                json!({
                    "type": "result", "is_error": true, "result": "stopped",
                    "terminal_reason": "aborted_streaming",
                }),
                SubagentStopReasonV1::Aborted,
                "aborted",
            ),
        ] {
            let (outcome, _) = run(vec![terminal], ClaudePermissionModeV1::DontAsk);
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
    fn an_unauthenticated_child_is_reported_as_an_actionable_access_failure() {
        // The exact terminal event a logged-out CLI produces.
        let (outcome, _) = run(
            vec![json!({
                "type": "result",
                "subtype": "success",
                "is_error": true,
                "result": "Not logged in \u{b7} Please run /login",
                "terminal_reason": "api_error",
                "api_error_status": null,
                "permission_denials": [],
            })],
            ClaudePermissionModeV1::DontAsk,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::AccessPolicy);
        let diagnostic = outcome.diagnostic.expect("diagnostic");
        assert!(diagnostic.contains("access-policy"));
        assert!(diagnostic.contains("not authenticated"));
    }

    #[test]
    fn denied_actions_are_reported_without_hiding_the_answer() {
        let (outcome, _) = run(
            vec![json!({
                "type": "result",
                "is_error": false,
                "result": "Done under policy",
                "terminal_reason": "completed",
                "permission_denials": [{"tool": "Bash"}, {"tool": "Write"}],
            })],
            ClaudePermissionModeV1::DontAsk,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Completed);
        assert_eq!(outcome.answer.as_deref(), Some("Done under policy"));
        assert!(
            outcome
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("denied 2 action(s)")),
            "expected the denial fact in {:?}",
            outcome.diagnostic
        );
    }

    #[test]
    fn a_stream_that_ends_without_a_result_is_a_process_failure() {
        let (outcome, _) = run(
            vec![ScriptedStreamV1::assistant("partial")],
            ClaudePermissionModeV1::DontAsk,
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
    fn a_cancelled_run_settles_aborted() {
        let token = CancellationToken::default();
        let mut stream = ScriptedStreamV1::cancelling_after(
            vec![ScriptedStreamV1::assistant("working")],
            1,
            token.clone(),
        );
        let outcome = run_claude_one_shot(
            &mut stream,
            &config(ClaudePermissionModeV1::DontAsk),
            &request(),
            &token,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
    }

    #[test]
    fn a_pre_cancelled_run_never_submits_the_task() {
        let token = CancellationToken::default();
        token.cancel();
        let mut stream = ScriptedStreamV1::new(vec![ScriptedStreamV1::result("ignored", false)]);
        let outcome = run_claude_one_shot(
            &mut stream,
            &config(ClaudePermissionModeV1::DontAsk),
            &request(),
            &token,
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
        assert!(stream.submitted.is_empty());
    }

    #[test]
    fn an_expired_deadline_settles_as_a_limit() {
        let mut stream = ScriptedStreamV1::new(vec![ScriptedStreamV1::assistant("working")]);
        stream.quiet_when_exhausted = true;
        let mut request = request();
        request.deadline = Duration::from_millis(5);
        let outcome = run_claude_one_shot(
            &mut stream,
            &config(ClaudePermissionModeV1::DontAsk),
            &request,
            &CancellationToken::default(),
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Limit);
    }

    #[test]
    fn configuration_validation_refuses_hostile_input() {
        let mut relative = config(ClaudePermissionModeV1::DontAsk);
        relative.executable = PathBuf::from("claude");
        assert_eq!(
            ClaudeOneShotBackendV1::new(relative).err(),
            Some(ClaudeOneShotErrorV1::InvalidExecutable)
        );

        let mut unlimited = config(ClaudePermissionModeV1::DontAsk);
        unlimited.limits.maximum_messages = 0;
        assert_eq!(
            ClaudeOneShotBackendV1::new(unlimited).err(),
            Some(ClaudeOneShotErrorV1::InvalidLimits)
        );

        let mut injected = config(ClaudePermissionModeV1::DontAsk);
        injected.arguments = vec!["--permission-mode\0bypass".to_owned()];
        assert_eq!(
            ClaudeOneShotBackendV1::new(injected).err(),
            Some(ClaudeOneShotErrorV1::InvalidArguments)
        );

        let backend = ClaudeOneShotBackendV1::new(config(ClaudePermissionModeV1::Plan))
            .expect("valid configuration");
        assert_eq!(backend.name(), "claude-code");
    }

    #[test]
    fn a_startup_failure_is_reported_as_a_process_outcome() {
        let mut missing = config(ClaudePermissionModeV1::DontAsk);
        missing.executable = PathBuf::from("/nonexistent/claude");
        let backend = ClaudeOneShotBackendV1::new(missing).expect("configuration is valid");
        let outcome = backend.run(&request(), &CancellationToken::default());
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Process);
        assert!(outcome.answer.is_none());
    }
}
