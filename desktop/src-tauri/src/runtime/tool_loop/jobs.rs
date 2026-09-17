//! Chat-owned shell jobs. The broker authorizes each operation; this registry owns
//! process lifetime beyond any one invocation and persists identity without replay.
mod contract;
mod registry;
#[cfg(test)]
mod tests;
use super::*;
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
        if !controls_available(&self.context.bindings) {
            return Err("Managed shell execution requires job_output, job_input, job_stop and job_list in this Chat's frozen tools. Enable them and create a new Chat.".into());
        }
        if cancellation.is_cancelled() || self.context.cancellation.is_cancelled() {
            return Err("shell launch cancelled".into());
        }
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
        let id = self.runtime.jobs.start(
            &self.context.chat_id,
            self.record.proposal.proposal_id.as_str(),
            &spec,
            interactive,
            self.context.cancellation.clone(),
        )?;
        let value = self.runtime.jobs.output(
            &self.context.chat_id,
            &id,
            None,
            maximum,
            wait.min(Duration::from_secs(10)),
            cancellation,
        )?;
        let summary = if value["running"] == true {
            format!("Shell continues as {id}. Use job_output or job_stop.")
        } else {
            format!("Shell job {id} finished.")
        };
        Ok((value, summary))
    }
}
