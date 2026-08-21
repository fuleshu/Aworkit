use std::{path::PathBuf, process::Command, time::Duration};
use thiserror::Error;
const MAX_OUTPUT: usize = 256 * 1024;
#[derive(Clone, Debug, Eq, PartialEq)] pub struct ProcessRequest { pub program: PathBuf, pub arguments: Vec<String>, pub working_directory: Option<PathBuf>, pub timeout: Duration }
#[derive(Clone, Debug, Eq, PartialEq)] pub struct ProcessResult { pub status: Option<i32>, pub stdout: Vec<u8>, pub stderr: Vec<u8>, pub timed_out: bool }
pub struct ProcessRunner;
impl ProcessRunner { pub fn run(request: &ProcessRequest) -> Result<ProcessResult, ProcessError> { if request.timeout.is_zero() { return Err(ProcessError::DeadlineElapsed); } if request.arguments.iter().any(|item| item.len() > 16 * 1024) { return Err(ProcessError::ArgumentTooLarge); } let mut command = Command::new(&request.program); command.args(&request.arguments); if let Some(path) = &request.working_directory { command.current_dir(path); } let output = command.output()?; if output.stdout.len().saturating_add(output.stderr.len()) > MAX_OUTPUT { return Err(ProcessError::OutputTooLarge); } Ok(ProcessResult { status: output.status.code(), stdout: output.stdout, stderr: output.stderr, timed_out: false }) } }
#[derive(Debug, Error)] pub enum ProcessError { #[error("process I/O failed: {0}")] Io(#[from] std::io::Error), #[error("argument too large")] ArgumentTooLarge, #[error("output too large")] OutputTooLarge, #[error("deadline elapsed")] DeadlineElapsed }
