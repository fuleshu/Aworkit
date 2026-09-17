//! Chat-owned process jobs. The broker authorizes each operation; this registry owns
//! process lifetime beyond any one invocation and persists identity without replay.
mod contract;
mod registry;
#[cfg(test)]
mod tests;
use super::*;
use aworkit_capability_host::ProcessSpecV1;
pub(super) use contract::*;
pub(super) use registry::JobRegistry;

impl FileToolDispatcherV1 {
    pub(super) fn execute_job(
        &self,
        operation: &str,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let owner = &self.context.chat_id;
        let args = &self.record.call.arguments;
        if operation == "shell_start" {
            return self.start_shell_job(
                Duration::ZERO,
                65536,
                args["interactive"].as_bool().unwrap_or(true),
                cancellation,
            );
        }
        if operation == "python_start" {
            return self.start_python_job(
                Duration::ZERO,
                65536,
                args["interactive"].as_bool().unwrap_or(true),
                cancellation,
            );
        }
        let value = self
            .runtime
            .jobs
            .control(owner, operation, args, cancellation)?;
        Ok((value, format!("{operation} completed.")))
    }

    pub(super) fn start_shell_job(
        &self,
        wait: Duration,
        maximum: usize,
        interactive: bool,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let spec = BuiltInProcessTools::<NativeProcessPort>::shell_spec(&ShellInvocationV1 {
            mode: ToolAuthorityModeV1::HostShell,
            shell_program: self
                .record
                .binding
                .options
                .executable
                .as_ref()
                .map(PathBuf::from)
                .map(Ok)
                .unwrap_or_else(shell_program)?,
            command_text: self.record.call.arguments["command"]
                .as_str()
                .ok_or("command is invalid")?
                .to_owned(),
            working_directory: Some(self.record.workspace.root.clone()),
            environment: BTreeMap::new(),
            limits: HostToolLimitsV1 {
                timeout: Duration::from_secs(30),
                maximum_output_bytes: maximum,
                cancellation_grace: Duration::from_millis(100),
            },
        })
        .map_err(|e| e.to_string())?;
        self.start_process_job(&spec, "Shell", wait, interactive, cancellation)
    }

    /// Python shares the exact job lifecycle, authority and final-response gate with shell.
    pub(super) fn start_python_job(
        &self,
        wait: Duration,
        maximum: usize,
        interactive: bool,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let mut spec = BuiltInProcessTools::<NativeProcessPort>::python_spec(&PythonInvocationV1 {
            mode: ToolAuthorityModeV1::HostPython,
            interpreter: self
                .record
                .binding
                .options
                .executable
                .as_ref()
                .map(PathBuf::from)
                .map(Ok)
                .unwrap_or_else(python_program)?,
            script: self.record.call.arguments["script"]
                .as_str()
                .ok_or("script is invalid")?
                .to_owned(),
            arguments: Vec::new(),
            working_directory: Some(self.record.workspace.root.clone()),
            environment: BTreeMap::new(),
            limits: HostToolLimitsV1 {
                timeout: Duration::from_secs(30),
                maximum_output_bytes: maximum,
                cancellation_grace: Duration::from_millis(100),
            },
        })
        .map_err(|e| e.to_string())?;
        // File-backed stdout would otherwise buffer progress until the script exits.
        // Preserve -I and the legacy Python argv while making managed jobs unbuffered.
        spec.arguments.insert(1, "-u".into());
        self.start_process_job(&spec, "Python", wait, interactive, cancellation)
    }

    fn start_process_job(
        &self,
        spec: &ProcessSpecV1,
        label: &str,
        wait: Duration,
        interactive: bool,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        if !controls_available(&self.context.bindings) {
            return Err("Managed execution requires job_output, job_input, job_stop and job_list in this Chat's frozen tools. Enable them and create a new Chat.".into());
        }
        if cancellation.is_cancelled() || self.context.cancellation.is_cancelled() {
            return Err(format!("{label} launch cancelled"));
        }
        let id = self.runtime.jobs.start(
            &self.context.chat_id,
            self.record.proposal.proposal_id.as_str(),
            spec,
            interactive,
            self.context.cancellation.clone(),
        )?;
        let value = self.runtime.jobs.output(
            &self.context.chat_id,
            &id,
            None,
            spec.maximum_output_bytes,
            wait.min(Duration::from_secs(10)),
            cancellation,
        )?;
        let summary = if value["running"] == true {
            format!("{label} continues as {id}. Use job_output or job_stop.")
        } else {
            format!("{label} job {id} finished.")
        };
        Ok((value, summary))
    }
}
