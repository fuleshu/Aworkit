//! Command-local composition and coordinator-owned post-command updates.
use super::super::concurrency::{ChatCommandLease, ChatCommands, is_navigation};
use super::*;

#[derive(Default)]
pub(super) struct WorkerFeedback {
    pub defaults: Option<ChatDefaultsConfigurationV2>,
    pub health: Vec<(ProviderConfigurationV2, ProviderHealth)>,
}

pub(crate) struct ChatCommandWorker {
    runtime: DesktopRuntime,
    _lease: ChatCommandLease,
}

impl ChatCommandWorker {
    pub(crate) fn execute(&mut self, command: UiCommandInput) -> Result<UiCommandReceipt, String> {
        let result = self.runtime.command(command);
        // Refresh the compact sidebar summary even if this Chat is hidden.
        if let Err(error) = self.runtime.history.snapshot(0) {
            eprintln!("aworkit: could not refresh Chat summary: {error}");
        }
        result
    }
}

impl DesktopRuntime {
    pub(crate) fn chat_commands(&self) -> ChatCommands {
        self.chat_commands.clone()
    }

    pub(crate) fn command_chat_target(&self, input: &UiCommandInput) -> Result<String, String> {
        if let Some(target) = &input.target_id {
            self.history.identity(target)?;
            return Ok(target.clone());
        }
        Ok(self
            .history
            .current_chat_identity()?
            .ok_or("Chat identity unavailable")?
            .chat_id
            .to_string())
    }

    pub(crate) fn prepare_chat_worker(
        &self,
        chat_id: &str,
        command_id: &str,
    ) -> Result<ChatCommandWorker, String> {
        let history = self.history.for_chat(chat_id)?;
        let lease = self.chat_commands.reserve(chat_id, command_id)?;
        let pipeline = self
            .pipeline
            .for_chat(Arc::new(history.clone()))
            .unwrap_or_else(|| self.pipeline.clone());
        let runtime = Self {
            chat_workspaces: self.chat_workspaces.clone(),
            tool_plugin_directory: self.tool_plugin_directory.clone(),
            approvals: self.approvals.clone(),
            images: self.images.clone(),
            documents: self.documents.clone(),
            history,
            credentials: self
                .credentials
                .fork(&self.documents.settings().credentials)?,
            credential_journal: self.credential_journal.clone(),
            provider: self.provider.clone(),
            pipeline,
            project_coordinator: self.project_coordinator.clone(),
            provider_health: self.provider_health.clone(),
            legacy_provider_warning: self.legacy_provider_warning.clone(),
            management_repair: ManagementRepairGateway::default(),
            processed: HashMap::new(),
            cancellation_controller: self.cancellation_controller.clone(),
            chat_commands: self.chat_commands.clone(),
            worker_feedback: Some(WorkerFeedback::default()),
        };
        Ok(ChatCommandWorker {
            runtime,
            _lease: lease,
        })
    }

    pub(crate) fn finish_chat_worker(&mut self, worker: &mut ChatCommandWorker) {
        if let Some(feedback) = worker.runtime.worker_feedback.take() {
            if let Some(defaults) = feedback.defaults {
                if let Err(error) = self.documents.remember_chat_defaults(defaults) {
                    eprintln!("aworkit: could not remember Chat selections: {error}");
                }
            }
            for (provider, health) in feedback.health {
                let _ = self.record_provider_health(&provider, health);
            }
        }
    }

    pub fn snapshot(&self, after_sequence: u64) -> Result<RuntimeSnapshot, String> {
        self.snapshot_for_chat(after_sequence, None)
    }

    /// Queries never acquire a Chat worker. Live ownership is not crash recovery.
    pub fn snapshot_for_chat(
        &self,
        after_sequence: u64,
        chat_id: Option<&str>,
    ) -> Result<RuntimeSnapshot, String> {
        let history = match chat_id {
            Some(id) => self.history.for_chat(id)?,
            None => self.history.clone(),
        };
        let mut snapshot = self.snapshot_history(&history, after_sequence)?;
        snapshot.active_chat_ids = self.chat_commands.active_ids();
        if self.worker_feedback.is_none()
            && snapshot.active_chat_ids.contains(&snapshot.chat.chat_id)
        {
            snapshot.chat.phase = "running".into();
            snapshot.chat.recovery_pending = false;
            snapshot.chat.disabled_reason = None;
        }
        Ok(snapshot)
    }

    pub(super) fn guard_navigation(&self, input: &UiCommandInput) -> Result<(), String> {
        if is_navigation(&input.action) && matches!(input.action.as_str(), "delete_chat" | "fork") {
            let target = required_chat_target(input)?;
            if self.chat_commands.active_ids().contains(&target) {
                return Err("Stop this Chat before deleting or forking it.".into());
            }
            let history = self.history.for_chat(&target)?;
            if history
                .pending_effect_command_at_head(history.head()?)?
                .is_some()
            {
                return Err(
                    "Resolve this Chat's interrupted command before deleting or forking it.".into(),
                );
            }
            let snapshot = history.snapshot(0)?;
            if snapshot.chat.phase == "awaiting_approval" {
                return Err(
                    "Resolve this Chat's pending approval before deleting or forking it.".into(),
                );
            }
        }
        Ok(())
    }
}
