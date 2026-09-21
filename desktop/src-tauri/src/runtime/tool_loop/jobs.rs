//! Chat-owned process jobs. The broker authorizes each operation; this registry owns
//! process lifetime beyond any one invocation and persists identity without replay.
mod contract;
mod registry;
#[cfg(test)]
mod tests;
use super::*;
use aworkit_capability_host::ProcessSpecV1;
pub(super) use contract::*;
pub(super) use registry::{ChildJobHandle, ChildJobInfo, JobRegistry};

impl FileToolDispatcherV1 {
    pub(super) fn execute_job(
        &self,
        operation: &str,
        envelope: &ApprovedInvocationEnvelopeV1,
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
        // A delegated child is a job too. Its settled state resumes the child
        // conversation instead of being read as a finished process.
        if let Some(job_id) = args["jobId"].as_str()
            && let Some(info) = self.runtime.jobs.child_job(owner, job_id)?
        {
            return self.control_child_job(operation, job_id, &info, envelope, cancellation);
        }
        let value = self
            .runtime
            .jobs
            .control_scoped(owner, self.context.delegation.as_ref().map(StableId::as_str), operation, args, cancellation)?;
        Ok((value, format!("{operation} completed.")))
    }

    /// Applies the shared job control surface to a delegated child job.
    fn control_child_job(
        &self,
        operation: &str,
        _job_id: &str,
        info: &ChildJobInfo,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let owner = &self.context.chat_id;
        let args = &self.record.call.arguments;
        match operation {
            "job_output" | "job_keep" => {
                let value =
                    self.runtime
                        .jobs
                        .control_scoped(owner, None, operation, args, cancellation)?;
                Ok((value, format!("{operation} completed.")))
            }
            "job_stop" => {
                let value =
                    self.runtime
                        .jobs
                        .control_scoped(owner, None, "job_stop", args, cancellation)?;
                self.close_child_scope(&info.child_id)?;
                Ok((
                    value,
                    format!("Stopped subagent {} and closed its scope.", info.child_id),
                ))
            }
            "job_input" => {
                let text = args["text"]
                    .as_str()
                    .ok_or("text missing")?
                    .to_owned();
                if info.running {
                    let value =
                        self.runtime
                            .jobs
                            .control_scoped(owner, None, "job_input", args, cancellation)?;
                    return Ok((
                        value,
                        format!("Steered subagent {} at its next step boundary.", info.child_id),
                    ));
                }
                let run_id = self.record.proposal.run_id.clone();
                let frame = self
                    .runtime
                    .records
                    .subagent_child(&run_id, &info.child_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("subagent {} is unavailable", info.child_id))?;
                let value = self.start_continuation_job(envelope, frame, text)?;
                Ok((
                    value,
                    format!(
                        "Subagent {} continues in the background; observe it with job_output.",
                        info.child_id
                    ),
                ))
            }
            _ => Err("unknown subagent job operation".into()),
        }
    }

    /// Marks a child scope cancelled after its job stopped, so a stopped child
    /// can never be resumed accidentally. Repeating it is a no-op.
    fn close_child_scope(&self, child_id: &str) -> Result<(), String> {
        let run_id = self.record.proposal.run_id.clone();
        let Some(frame) = self
            .runtime
            .records
            .subagent_child(&run_id, child_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        if frame.status == super::subagent::ChildStatusV1::Cancelled {
            return Ok(());
        }
        let updated = super::subagent::SubagentChildFrameV1 {
            head_revision: frame.head_revision.saturating_add(1),
            status: super::subagent::ChildStatusV1::Cancelled,
            updated_at: crate::runtime::history::now_label(),
            ..frame
        };
        self.runtime
            .records
            .record_subagent_child(&updated)
            .map_err(|error| error.to_string())
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
        // Job control is intrinsic: the controls accompany the capability that
        // launches work, so every frozen selection that can reach this dispatch
        // already holds them. A Chat is never left on a second, older behaviour
        // by a switch that happened to stay off, and no bookkeeping state of a
        // Chat may refuse to start work the Chat is authorised to run.
        debug_assert!(
            controls_available(&self.context.bindings),
            "job control accompanies a bound host shell/Python capability"
        );
        if cancellation.is_cancelled() || self.context.cancellation.is_cancelled() {
            return Err(format!("{label} launch cancelled"));
        }
        let id = self.runtime.jobs.start_scoped(
            &self.context.chat_id,
            self.record.proposal.proposal_id.as_str(),
            Some(self.record.outer_invocation_id.as_str()),
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
