//! Bounded Codex App Server standard-I/O handshake.
//!
//! This adapter performs only the inert Settings probe: initialize one
//! user-selected local process, inspect its authentication state and model
//! catalog, then terminate the complete process group. It does not create a
//! Codex thread, start a turn, grant an approval, or retain a native session.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use command_group::{CommandGroup, GroupChild};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use zeroize::Zeroizing;

const MAXIMUM_ARGUMENTS: usize = 512;
const MAXIMUM_ARGUMENT_BYTES: usize = 64 * 1024;
const MAXIMUM_ENVIRONMENT_ENTRIES: usize = 256;
const MAXIMUM_ENVIRONMENT_BYTES: usize = 256 * 1024;
const MAXIMUM_PATH_BYTES: usize = 16 * 1024;
const MAXIMUM_MODEL_ID_BYTES: usize = 512;

/// One environment value injected into the selected process without making
/// the value printable or serializable.
pub struct CodexAppServerEnvironmentV1 {
    name: String,
    value: Zeroizing<String>,
}

impl CodexAppServerEnvironmentV1 {
    pub fn new(name: String, value: Zeroizing<String>) -> Self {
        Self { name, value }
    }
}

/// Runtime bounds for one transient Settings handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexAppServerProbeLimitsV1 {
    pub timeout: Duration,
    pub maximum_message_bytes: usize,
    pub maximum_messages: usize,
    pub maximum_models: usize,
}

impl Default for CodexAppServerProbeLimitsV1 {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            maximum_message_bytes: 1024 * 1024,
            maximum_messages: 512,
            maximum_models: 512,
        }
    }
}

/// Exact argv-only process configuration for a Codex App Server probe.
pub struct CodexAppServerProbeConfigV1 {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    /// Codex normally needs its existing user configuration and login state.
    /// When false, the child starts with only `environment` entries.
    pub inherit_environment: bool,
    pub environment: Vec<CodexAppServerEnvironmentV1>,
    pub limits: CodexAppServerProbeLimitsV1,
}

/// Non-secret authentication facts reported by `account/read`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexAppServerAccountV1 {
    pub account_type: Option<String>,
    pub requires_openai_auth: bool,
}

/// Honest protocol features implemented by the stable Codex adapter surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexAppServerCapabilitiesV1 {
    pub progress: bool,
    pub continuation: bool,
    pub cancellation: bool,
    pub approvals: bool,
}

/// Secret-free result of a successful initialize/account/model probe.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexAppServerProbeResultV1 {
    pub protocol: String,
    pub server_identity: Option<String>,
    pub platform_family: Option<String>,
    pub platform_os: Option<String>,
    pub account: CodexAppServerAccountV1,
    pub model_ids: Vec<String>,
    pub capabilities: CodexAppServerCapabilitiesV1,
}

/// Performs the documented `initialize` → `initialized` → `account/read` and
/// `model/list` exchange, then tears down the process group.
pub fn probe_codex_app_server_v1(
    config: CodexAppServerProbeConfigV1,
) -> Result<CodexAppServerProbeResultV1, CodexAppServerProbeError> {
    validate_config(&config)?;
    let executable = canonical_executable(&config.executable)?;
    let mut command = Command::new(executable);
    command
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !config.inherit_environment {
        command.env_clear();
    }
    for entry in &config.environment {
        command.env(&entry.name, entry.value.as_str());
    }
    if let Some(working_directory) = &config.working_directory {
        command.current_dir(canonical_directory(working_directory)?);
    }

    let mut child = ManagedGroupChild::spawn(&mut command)?;
    let stdin = child.stdin()?;
    let stdout = child.stdout()?;
    let stderr = child.stderr()?;
    let (sender, receiver) = mpsc::sync_channel(8);
    let maximum_message_bytes = config.limits.maximum_message_bytes;
    let stdout_reader = thread::spawn(move || {
        read_json_lines(stdout, maximum_message_bytes, &sender);
    });
    let stderr_reader = thread::spawn(move || drain_discarded(stderr));

    let started = Instant::now();
    let mut writer = stdin;
    write_message(
        &mut writer,
        &json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "clientInfo": {
                    "name": "aworkit",
                    "title": "Aworkit",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
        maximum_message_bytes,
    )?;
    let mut observed_messages = 0_usize;
    let initialize =
        receive_response(&receiver, 1, &mut observed_messages, started, config.limits)?;
    write_message(
        &mut writer,
        &json!({"method":"initialized","params":{}}),
        maximum_message_bytes,
    )?;
    write_message(
        &mut writer,
        &json!({
            "method": "account/read",
            "id": 2,
            "params": {"refreshToken": false}
        }),
        maximum_message_bytes,
    )?;
    write_message(
        &mut writer,
        &json!({
            "method": "model/list",
            "id": 3,
            "params": {"limit": config.limits.maximum_models, "includeHidden": false}
        }),
        maximum_message_bytes,
    )?;

    let mut account = None;
    let mut models = None;
    while account.is_none() || models.is_none() {
        let message = receive_message(&receiver, &mut observed_messages, started, config.limits)?;
        match response_id(&message) {
            Some(2) => account = Some(response_result(message)?),
            Some(3) => models = Some(response_result(message)?),
            _ => {}
        }
    }

    drop(writer);
    child.terminate();
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    let (server_identity, platform_family, platform_os) = parse_initialize(&initialize)?;
    let account = parse_account(account.expect("account response was collected"))?;
    let model_ids = parse_models(
        models.expect("model response was collected"),
        config.limits.maximum_models,
    )?;
    Ok(CodexAppServerProbeResultV1 {
        protocol: "codex-app-server-jsonrpc-stdio".to_owned(),
        server_identity,
        platform_family,
        platform_os,
        account,
        model_ids,
        capabilities: CodexAppServerCapabilitiesV1 {
            progress: true,
            continuation: true,
            cancellation: true,
            approvals: true,
        },
    })
}

fn validate_config(config: &CodexAppServerProbeConfigV1) -> Result<(), CodexAppServerProbeError> {
    let limits = config.limits;
    if limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(60)
        || limits.maximum_message_bytes == 0
        || limits.maximum_message_bytes > 4 * 1024 * 1024
        || limits.maximum_messages == 0
        || limits.maximum_messages > 4_096
        || limits.maximum_models == 0
        || limits.maximum_models > 4_096
    {
        return Err(CodexAppServerProbeError::InvalidLimits);
    }
    if config.arguments.len() > MAXIMUM_ARGUMENTS
        || config.arguments.iter().map(String::len).sum::<usize>() > MAXIMUM_ARGUMENT_BYTES
        || config
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
    {
        return Err(CodexAppServerProbeError::InvalidArguments);
    }
    if config.environment.len() > MAXIMUM_ENVIRONMENT_ENTRIES
        || config
            .environment
            .iter()
            .map(|entry| entry.name.len().saturating_add(entry.value.len()))
            .sum::<usize>()
            > MAXIMUM_ENVIRONMENT_BYTES
    {
        return Err(CodexAppServerProbeError::InvalidEnvironment);
    }
    let mut names = BTreeSet::new();
    for entry in &config.environment {
        if !valid_environment_name(&entry.name)
            || entry.value.contains('\0')
            || !names.insert(fold_environment_name(&entry.name))
        {
            return Err(CodexAppServerProbeError::InvalidEnvironment);
        }
    }
    if path_text(&config.executable).is_none()
        || config
            .working_directory
            .as_deref()
            .is_some_and(|path| path_text(path).is_none())
    {
        return Err(CodexAppServerProbeError::InvalidPath);
    }
    Ok(())
}

fn canonical_executable(path: &Path) -> Result<PathBuf, CodexAppServerProbeError> {
    if !path.is_absolute() {
        return Err(CodexAppServerProbeError::InvalidPath);
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| CodexAppServerProbeError::Launch)?;
    if !canonical.is_file() || path_text(&canonical).is_none() {
        return Err(CodexAppServerProbeError::InvalidPath);
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, CodexAppServerProbeError> {
    if !path.is_absolute() {
        return Err(CodexAppServerProbeError::InvalidPath);
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| CodexAppServerProbeError::Launch)?;
    if !canonical.is_dir() || path_text(&canonical).is_none() {
        return Err(CodexAppServerProbeError::InvalidPath);
    }
    Ok(canonical)
}

fn path_text(path: &Path) -> Option<&str> {
    path.to_str().filter(|value| {
        !value.is_empty() && value.len() <= MAXIMUM_PATH_BYTES && !value.contains('\0')
    })
}

fn valid_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('=')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn fold_environment_name(value: &str) -> String {
    if cfg!(windows) {
        value.to_ascii_uppercase()
    } else {
        value.to_owned()
    }
}

fn write_message(
    writer: &mut impl Write,
    value: &Value,
    maximum_message_bytes: usize,
) -> Result<(), CodexAppServerProbeError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| CodexAppServerProbeError::Protocol)?;
    if bytes.len().saturating_add(1) > maximum_message_bytes {
        return Err(CodexAppServerProbeError::MessageTooLarge);
    }
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| CodexAppServerProbeError::Transport)
}

enum ReaderMessage {
    Line(Vec<u8>),
    TooLarge,
    Transport,
    Eof,
}

fn read_json_lines(
    source: impl std::io::Read,
    maximum_message_bytes: usize,
    sender: &mpsc::SyncSender<ReaderMessage>,
) {
    let mut reader = BufReader::with_capacity(8 * 1024, source);
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(_) => {
                let _ = sender.send(ReaderMessage::Transport);
                return;
            }
        };
        if available.is_empty() {
            let _ = sender.send(ReaderMessage::Eof);
            return;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index.saturating_add(1));
        if line.len().saturating_add(consumed) > maximum_message_bytes {
            reader.consume(consumed);
            let _ = sender.send(ReaderMessage::TooLarge);
            return;
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_none() {
            continue;
        }
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        if !line.is_empty()
            && sender
                .send(ReaderMessage::Line(std::mem::take(&mut line)))
                .is_err()
        {
            return;
        }
    }
}

fn drain_discarded(mut source: impl std::io::Read) {
    // Keep draining after the peer exceeds one protocol-message bound. Stopping
    // here could fill the child's stderr pipe and deadlock an otherwise valid
    // handshake; no stderr bytes are retained or returned to the caller.
    let _ = std::io::copy(&mut source, &mut std::io::sink());
}

fn receive_response(
    receiver: &Receiver<ReaderMessage>,
    id: u64,
    observed_messages: &mut usize,
    started: Instant,
    limits: CodexAppServerProbeLimitsV1,
) -> Result<Value, CodexAppServerProbeError> {
    loop {
        let message = receive_message(receiver, observed_messages, started, limits)?;
        if response_id(&message) == Some(id) {
            return response_result(message);
        }
    }
}

fn receive_message(
    receiver: &Receiver<ReaderMessage>,
    observed_messages: &mut usize,
    started: Instant,
    limits: CodexAppServerProbeLimitsV1,
) -> Result<Value, CodexAppServerProbeError> {
    let remaining = limits
        .timeout
        .checked_sub(started.elapsed())
        .ok_or(CodexAppServerProbeError::TimedOut)?;
    let message = receiver
        .recv_timeout(remaining)
        .map_err(|_| CodexAppServerProbeError::TimedOut)?;
    *observed_messages = observed_messages.saturating_add(1);
    if *observed_messages > limits.maximum_messages {
        return Err(CodexAppServerProbeError::MessageLimit);
    }
    match message {
        ReaderMessage::Line(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| CodexAppServerProbeError::Protocol)
        }
        ReaderMessage::TooLarge => Err(CodexAppServerProbeError::MessageTooLarge),
        ReaderMessage::Transport => Err(CodexAppServerProbeError::Transport),
        ReaderMessage::Eof => Err(CodexAppServerProbeError::Exited),
    }
}

fn response_id(value: &Value) -> Option<u64> {
    value.get("id").and_then(Value::as_u64)
}

fn response_result(value: Value) -> Result<Value, CodexAppServerProbeError> {
    if value.get("error").is_some() {
        return Err(CodexAppServerProbeError::RequestRejected);
    }
    value
        .get("result")
        .cloned()
        .ok_or(CodexAppServerProbeError::Protocol)
}

fn parse_initialize(
    value: &Value,
) -> Result<(Option<String>, Option<String>, Option<String>), CodexAppServerProbeError> {
    let object = value
        .as_object()
        .ok_or(CodexAppServerProbeError::Protocol)?;
    Ok((
        optional_bounded_string(object.get("userAgent"), 512)?,
        optional_bounded_string(object.get("platformFamily"), 128)?,
        optional_bounded_string(object.get("platformOs"), 128)?,
    ))
}

fn parse_account(value: Value) -> Result<CodexAppServerAccountV1, CodexAppServerProbeError> {
    let object = value
        .as_object()
        .ok_or(CodexAppServerProbeError::Protocol)?;
    let requires_openai_auth = object
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .ok_or(CodexAppServerProbeError::Protocol)?;
    let account_type = match object.get("account") {
        None | Some(Value::Null) => None,
        Some(Value::Object(account)) => optional_bounded_string(account.get("type"), 128)?,
        Some(_) => return Err(CodexAppServerProbeError::Protocol),
    };
    Ok(CodexAppServerAccountV1 {
        account_type,
        requires_openai_auth,
    })
}

fn parse_models(value: Value, maximum: usize) -> Result<Vec<String>, CodexAppServerProbeError> {
    let values = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(CodexAppServerProbeError::Protocol)?;
    if values.len() > maximum {
        return Err(CodexAppServerProbeError::ModelLimit);
    }
    let mut models = Vec::with_capacity(values.len());
    let mut unique = BTreeSet::new();
    for value in values {
        let object = value
            .as_object()
            .ok_or(CodexAppServerProbeError::Protocol)?;
        let id = object
            .get("id")
            .or_else(|| object.get("model"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= MAXIMUM_MODEL_ID_BYTES && !id.contains('\0'))
            .ok_or(CodexAppServerProbeError::Protocol)?;
        if unique.insert(id.to_owned()) {
            models.push(id.to_owned());
        }
    }
    Ok(models)
}

fn optional_bounded_string(
    value: Option<&Value>,
    maximum: usize,
) -> Result<Option<String>, CodexAppServerProbeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty() && value.len() <= maximum && !value.contains('\0') =>
        {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(CodexAppServerProbeError::Protocol),
    }
}

struct ManagedGroupChild {
    child: Option<GroupChild>,
}

impl ManagedGroupChild {
    fn spawn(command: &mut Command) -> Result<Self, CodexAppServerProbeError> {
        command
            .group_spawn()
            .map(|child| Self { child: Some(child) })
            .map_err(|_| CodexAppServerProbeError::Launch)
    }

    fn stdin(&mut self) -> Result<std::process::ChildStdin, CodexAppServerProbeError> {
        self.child
            .as_mut()
            .and_then(|child| child.inner().stdin.take())
            .ok_or(CodexAppServerProbeError::Launch)
    }

    fn stdout(&mut self) -> Result<std::process::ChildStdout, CodexAppServerProbeError> {
        self.child
            .as_mut()
            .and_then(|child| child.inner().stdout.take())
            .ok_or(CodexAppServerProbeError::Launch)
    }

    fn stderr(&mut self) -> Result<std::process::ChildStderr, CodexAppServerProbeError> {
        self.child
            .as_mut()
            .and_then(|child| child.inner().stderr.take())
            .ok_or(CodexAppServerProbeError::Launch)
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ManagedGroupChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Sanitized probe failures never retain peer output, environment values, or
/// credential material.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CodexAppServerProbeError {
    #[error("Codex App Server probe limits are invalid")]
    InvalidLimits,
    #[error("Codex App Server executable or working-directory path is invalid")]
    InvalidPath,
    #[error("Codex App Server argument vector is invalid or too large")]
    InvalidArguments,
    #[error("Codex App Server environment is invalid or too large")]
    InvalidEnvironment,
    #[error("Codex App Server process could not be started")]
    Launch,
    #[error("Codex App Server transport failed")]
    Transport,
    #[error("Codex App Server exited before completing the handshake")]
    Exited,
    #[error("Codex App Server handshake timed out")]
    TimedOut,
    #[error("Codex App Server returned an invalid protocol message")]
    Protocol,
    #[error("Codex App Server rejected a handshake request")]
    RequestRejected,
    #[error("Codex App Server message exceeded the configured size bound")]
    MessageTooLarge,
    #[error("Codex App Server emitted too many messages during the handshake")]
    MessageLimit,
    #[error("Codex App Server returned too many models")]
    ModelLimit,
}
