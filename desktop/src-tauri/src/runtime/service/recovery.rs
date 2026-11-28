//! Recovery keeps the staged action's identity and commits its receipt last.
use super::*;

impl DesktopRuntime {
    /// Reconciles the exact interrupted action. Never manufactures a fresh user turn.
    pub(super) fn recover_pending_effect(
        &mut self,
        resume_command_id: &str,
        resume_fingerprint: &str,
        _expected_version: u64,
    ) -> Result<UiCommandReceipt, String> {
        let head = self.history.head()?;
        let pending = self
            .history
            .pending_effect_command_at_head(head)?
            .ok_or_else(|| "There is no interrupted reply to continue.".to_owned())?;
        let fingerprint = command_fingerprint(&pending.command)?;
        if fingerprint != pending.command_hash {
            return Err("pending Chat command failed command integrity validation".into());
        }
        let pending_id = pending.command.command_id.clone();
        // What the interrupted attempt left behind, recorded with the recovery so
        // the reason survives rather than being replaced by a fresh dispatch.
        let mut recovery_report = json!({
            "status": "resumed",
            "reason": "The attempt was interrupted before a terminal result was committed.",
        });
        // An abrupt exit leaves its own bounded report: name it here, once, so the
        // recovery carries the panic and how far the history had got.
        if let Some(crash) = super::super::crash_report::last_report() {
            recovery_report["previousExit"] = crash;
        }
        match pending.command.action.as_str() {
            "question" => { self.complete_question(pending.command, fingerprint)?; }
            "approval" => { self.complete_approval(pending.command, fingerprint, Some(head))?; }
            "start" | "enqueue" | "compact_context" => {
                // A workflow replay reads brokered outcomes. Old child spans no
                // longer have a worker; the original run receives its own result.
                // Decision resumes, in contrast, still need their suspended spans.
                let suffix = format!(".{pending_id}");
                let facts: Vec<_> = self.history.open_span_terminal_facts(
                    "failed", "This attempt was interrupted.", &now_label(),
                )?.into_iter().filter(|fact| {
                    fact["spanId"].as_str().is_none_or(|id| !id.ends_with(&suffix))
                }).map(|fact| ("span.failed", fact)).collect();
                if !facts.is_empty() {
                    // Preparation is independently idempotent, not a receipt for
                    // the user's Continue action, which can still fail afterwards.
                    recovery_report["closedSpanCount"] = json!(facts.len());
                    self.history.append(
                        &format!("{resume_command_id}.prepare"), resume_fingerprint, head, facts,
                    )?;
                }
                self.complete_workflow_input(pending.command, fingerprint, Some(self.history.head()?))?;
            }
            _ => return Err("This interrupted action cannot be continued. Stop this reply to send a new message.".into()),
        }
        let receipt = self.history.append(
            resume_command_id,
            resume_fingerprint,
            self.history.head()?,
            vec![(
                "chat.recovery_completed",
                json!({
                    "createdAt": now_label(),
                    "recoveredCommandId": pending_id,
                    "lastCommittedSequence": head,
                    "report": recovery_report,
                }),
            )],
        )?;
        // The report is now durable evidence; do not repeat it on every recovery.
        super::super::crash_report::clear_last_report();
        Ok(receipt)
    }

    /// Resolves an unrecoverable pending effect without pretending it never
    /// started. This never invokes the pipeline. It records the exact staged
    /// command as outcome-uncertain, returns control, and preserves evidence so
    /// the user can safely create another Chat without an invisible orphan.
    pub(super) fn abandon_pending_effect(
        &mut self,
        command_id: &str,
        command_fingerprint: &str,
        expected_version: u64,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(expected_version)?;
        let pending = self
            .history
            .pending_effect_command_at_head(expected_version)?
            .ok_or_else(|| "there is no interrupted Chat command to abandon".to_owned())?;
        let frozen = self
            .history
            .pending_context_at_head(expected_version)?
            .or(self.history.current_frozen_context()?)
            .ok_or_else(|| "pending Chat recovery has no frozen execution context".to_owned())?;
        if frozen.context_hash != pending.frozen_context_hash {
            return Err("pending Chat command does not match its frozen context".into());
        }
        let context = &frozen.context;
        let created_at = now_label();
        let mut facts = Vec::new();
        let pending_started = self.history.command_started(&pending.command.command_id)?;
        if pending.command.action == "start" && !pending_started {
            facts.push((
                "chat.started",
                json!({
                    "workflowId":context.workflow_id,
                    "workflowVersion":context.workflow_version,
                    "frozenContextHash":frozen.context_hash,
                    "createdAt":created_at,
                    "chatId":context.identity.chat_id,
                    "runId":context.identity.run_id,
                    "projectId":context.project.as_ref().map(|project| project.project_id.as_str()),
                    "workspaceIdentityHash":context.project.as_ref().map(|project| project.workspace_identity_hash.as_str()),
                }),
            ));
        }
        if !pending_started && matches!(pending.command.action.as_str(), "start" | "enqueue") {
            let user_input = super::super::images::command_text(&pending.command.payload)?;
            facts.push((
                "message.user",
                message_fact(&user_input, &created_at, MessageUsageV1::default()),
            ));
        }
        facts.extend(self.history.cancel_open_questions(&created_at)?);
        {
            facts.extend(
                self.history
                    .open_span_terminal_facts("failed", "Reply stopped.", &created_at)?
                    .into_iter()
                    .map(|fact| ("span.failed", fact)),
            );
        }
        let mut recovery_report = json!({
            "createdAt": created_at,
            "status": "outcome_uncertain",
            "reason": "The reply stopped before a terminal result was committed.",
            "body": "The attempt may have reached the provider, but no conclusive terminal evidence was recorded. Automatic replay is forbidden; the reason and the last committed sequence are kept so the next dispatch resumes from them instead of starting over.",
            "lastCommittedSequence": expected_version,
            "pendingCommandId": pending.command.command_id,
            "automaticReplayAllowed": false,
        });
        if let Some(crash) = super::super::crash_report::last_report() {
            recovery_report["previousExit"] = crash;
        }
        facts.push(("chat.recovery_report", recovery_report));
        facts.push((
            "execution.failed",
            json!({
                "createdAt":created_at,
                "status":"outcome_uncertain",
                "title":"Reply stopped",
                "body":"This reply was stopped. Changes already made have been kept.",
                "providerId":context.provider_id,
                "modelId":context.model_id,
                "modelTierId":context.model_tier_id,
                "frozenContextHash":frozen.context_hash,
                "settlesCommandId":pending.command.command_id,
                "pendingCommandId":pending.command.command_id,
                "pendingCommandHash":pending.command_hash,
                "automaticReplayAllowed":false,
                "recoveryAbandoned":true,
                "lastCommittedSequence": expected_version,
            }),
        ));
        facts.push((
            "chat.turn_stopped",
            json!({
                "createdAt":created_at,"stopCommandId":command_id,
                "body":"This reply was stopped. Changes already made have been kept."
            }),
        ));
        let receipt = self.history
            .append(command_id, command_fingerprint, expected_version, facts)?;
        super::super::crash_report::clear_last_report();
        Ok(receipt)
    }
}
