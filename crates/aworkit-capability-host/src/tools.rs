//! Built-in shell and Python adapters with explicit authority modes.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use thiserror::Error;

use crate::{
    CancellationToken, ControlledProcessResult, PlatformProcessPort, ProcessError, ProcessSpecV1,
};

const MAX_SCRIPT_BYTES: usize = 256 * 1024;
const MAX_TOOL_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolAuthorityModeV1 {
    /// Unrestricted same-user host execution. A working directory is not a sandbox.
    HostCommand,
    HostShell,
    HostPython,
    /// Requires a separately verified isolation backend and is never downgraded.
    SandboxedPython,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostToolLimitsV1 {
    pub timeout: Duration,
    pub maximum_output_bytes: usize,
    pub cancellation_grace: Duration,
}

impl Default for HostToolLimitsV1 {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            maximum_output_bytes: MAX_TOOL_OUTPUT_BYTES,
            cancellation_grace: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArgumentVectorInvocationV1 {
    pub mode: ToolAuthorityModeV1,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub limits: HostToolLimitsV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellInvocationV1 {
    pub mode: ToolAuthorityModeV1,
    pub shell_program: PathBuf,
    pub command_text: String,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub limits: HostToolLimitsV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PythonInvocationV1 {
    pub mode: ToolAuthorityModeV1,
    pub interpreter: PathBuf,
    pub script: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub limits: HostToolLimitsV1,
}

pub struct BuiltInProcessTools<P> {
    platform: P,
}

impl<P: PlatformProcessPort> BuiltInProcessTools<P> {
    #[must_use]
    pub fn new(platform: P) -> Self {
        Self { platform }
    }

    pub fn execute_argv(
        &self,
        invocation: &ArgumentVectorInvocationV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ToolAdapterError> {
        if invocation.mode != ToolAuthorityModeV1::HostCommand {
            return Err(ToolAdapterError::AuthorityModeMismatch);
        }
        self.execute(
            invocation.program.clone(),
            invocation.arguments.clone(),
            invocation.working_directory.clone(),
            invocation.environment.clone(),
            &invocation.limits,
            cancellation,
        )
    }

    pub fn execute_shell(
        &self,
        invocation: &ShellInvocationV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ToolAdapterError> {
        if invocation.mode != ToolAuthorityModeV1::HostShell {
            return Err(ToolAdapterError::AuthorityModeMismatch);
        }
        if invocation.command_text.is_empty()
            || invocation.command_text.len() > MAX_SCRIPT_BYTES
            || invocation.command_text.contains('\0')
        {
            return Err(ToolAdapterError::InputBound);
        }
        #[cfg(windows)]
        let arguments = vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            invocation.command_text.clone(),
        ];
        #[cfg(not(windows))]
        let arguments = vec!["-c".to_owned(), invocation.command_text.clone()];
        self.execute(
            invocation.shell_program.clone(),
            arguments,
            invocation.working_directory.clone(),
            invocation.environment.clone(),
            &invocation.limits,
            cancellation,
        )
    }

    pub fn execute_python(
        &self,
        invocation: &PythonInvocationV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ToolAdapterError> {
        match invocation.mode {
            ToolAuthorityModeV1::HostPython => {}
            ToolAuthorityModeV1::SandboxedPython => {
                return Err(ToolAdapterError::VerifiedIsolationUnavailable);
            }
            _ => return Err(ToolAdapterError::AuthorityModeMismatch),
        }
        if invocation.script.is_empty()
            || invocation.script.len() > MAX_SCRIPT_BYTES
            || invocation.script.contains('\0')
        {
            return Err(ToolAdapterError::InputBound);
        }
        let mut arguments = Vec::with_capacity(invocation.arguments.len() + 2);
        arguments.push("-I".to_owned());
        arguments.push("-c".to_owned());
        arguments.push(invocation.script.clone());
        arguments.extend(invocation.arguments.clone());
        self.execute(
            invocation.interpreter.clone(),
            arguments,
            invocation.working_directory.clone(),
            invocation.environment.clone(),
            &invocation.limits,
            cancellation,
        )
    }

    fn execute(
        &self,
        program: PathBuf,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
        environment: BTreeMap<String, String>,
        limits: &HostToolLimitsV1,
        cancellation: &CancellationToken,
    ) -> Result<ControlledProcessResult, ToolAdapterError> {
        if limits.maximum_output_bytes == 0
            || limits.maximum_output_bytes > MAX_TOOL_OUTPUT_BYTES
            || limits.timeout.is_zero()
        {
            return Err(ToolAdapterError::InputBound);
        }
        Ok(self.platform.execute(
            &ProcessSpecV1 {
                program,
                arguments,
                working_directory,
                environment,
                timeout: limits.timeout,
                maximum_output_bytes: limits.maximum_output_bytes,
                cancellation_grace: limits.cancellation_grace,
            },
            cancellation,
        )?)
    }
}

#[derive(Debug, Error)]
pub enum ToolAdapterError {
    #[error("approved authority mode does not match this adapter")]
    AuthorityModeMismatch,
    #[error("tool input or resource bound is invalid")]
    InputBound,
    #[error("sandboxed Python requires a verified isolation backend; no downgrade is permitted")]
    VerifiedIsolationUnavailable,
    #[error(transparent)]
    Process(#[from] ProcessError),
}
