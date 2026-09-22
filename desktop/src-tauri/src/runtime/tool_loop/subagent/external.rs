//! One-shot delegation to a configured external agent product.
//!
//! Unlike an in-process child, an external delegation is a single unattended
//! process run: it has a durable child identity for inspection and lineage, but
//! no continuation, no steering and no tool inheritance. Only the product's
//! final answer returns to the parent, together with bounded unattended facts
//! such as denied actions.
//!
//! The frozen binding already carries the exact resolved target, so nothing is
//! read from Settings here; a later Settings change cannot alter a running
//! Chat's delegation.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aworkit_capability_host::{
    ExternalAgentBackendRegistryV1, MAXIMUM_DEADLINE, OneShotDelegationV1, SubagentStartOptionsV1,
    SubagentStopReasonV1,
};
use aworkit_protocol::StableId;
use serde_json::{Value, json};

use super::super::*;
use super::{ChildKindV1, ChildStatusV1, SubagentChildFrameV1, publish_child_fact};
use crate::runtime::external_agent::build_delegation_backend;

impl FileToolDispatcherV1 {
    /// Runs one unattended one-shot delegation to a configured external agent.
    ///
    /// The delegation always records its child frame before returning, so a
    /// failure is inspectable exactly like a success and recovery never replays
    /// an acknowledged external effect.
    pub(in crate::runtime::tool_loop) fn run_external_agent(
        &self,
        // A one-shot delegation registers no job and suspends for no approval,
        // so the envelope carries nothing this path needs.
        _envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let StoredFileToolLimitV1::ExternalAgent {
            backend,
            executable,
            arguments,
            working_directory,
            permission_mode,
            model,
            reasoning_effort,
        } = &self.record.binding.limit
        else {
            return Err("external delegation requires the frozen external-agent contract".into());
        };
        let task = self.record.call.arguments["task"]
            .as_str()
            .ok_or_else(|| "external delegation task is invalid".to_owned())?
            .to_owned();
        let child_id = digest_id(
            "child.external",
            &format!(
                "{}:{}:{}",
                self.context.chat_id,
                self.record.outer_invocation_id.as_str(),
                self.record.call.call_id
            ),
        )
        .map_err(|error| error.to_string())?
        .to_string();
        // The child runs in the target's own directory when it fixes one, and in
        // the Chat's frozen workspace otherwise.
        let working_directory = working_directory
            .as_ref()
            .map_or_else(|| self.context.workspace.root.clone(), PathBuf::from);
        let deadline = self.remaining_deadline()?;

        let backend_impl =
            build_delegation_backend(backend, executable, arguments, permission_mode)?;
        let request = OneShotDelegationV1 {
            run_id: StableId::parse(child_id.clone()).map_err(|error| error.to_string())?,
            task: task.clone(),
            working_directory,
            deadline,
            options: SubagentStartOptionsV1 {
                model: model.clone(),
                reasoning_effort: reasoning_effort.clone(),
                ..SubagentStartOptionsV1::default()
            },
        };
        // The registry refuses a frozen target whose product cannot express the
        // requested start options, so an unusable combination fails loudly
        // instead of being silently dropped.
        let mut registry = ExternalAgentBackendRegistryV1::new();
        registry
            .register(backend_impl)
            .map_err(|error| error.to_string())?;
        let backend_handle = registry
            .resolve(backend, &request)
            .map_err(|error| error.to_string())?;
        let outcome = backend_handle.run(&request, cancellation);

        let frame = self.external_child_frame(&child_id, &task, backend, permission_mode, &outcome);
        self.persist_child(&frame)?;
        publish_child_fact(&self.run_events, &frame, frame.status)?;

        match outcome.stop_reason {
            SubagentStopReasonV1::Completed => {
                let Some(answer) = outcome.answer else {
                    return Err(format!(
                        "external {backend} delegation completed without a final answer (child {child_id})"
                    ));
                };
                let mut result = json!({"childId": child_id, "answer": answer});
                if let Some(notice) = outcome.diagnostic {
                    result["notice"] = Value::String(notice);
                }
                Ok((
                    result,
                    format!(
                        "External {backend} agent returned its final answer (child {child_id})."
                    ),
                ))
            }
            stop_reason => Err(format!(
                "external {backend} delegation ended as {} (child {child_id}): {}",
                stop_reason_name(stop_reason),
                outcome
                    .diagnostic
                    .unwrap_or_else(|| "no further product detail was reported".to_owned())
            )),
        }
    }

    /// Time left in the Run's frozen deadline, bounded by the delegation seam.
    fn remaining_deadline(&self) -> Result<Duration, String> {
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);
        let remaining = self
            .context
            .deadline_epoch_millis
            .saturating_sub(now_millis);
        if remaining < 1_000 {
            return Err("the Run deadline leaves no time for an external delegation".into());
        }
        Ok(Duration::from_millis(remaining).min(MAXIMUM_DEADLINE))
    }

    /// Builds the terminal one-shot child frame for one external delegation.
    fn external_child_frame(
        &self,
        child_id: &str,
        task: &str,
        backend: &str,
        permission_mode: &str,
        outcome: &aworkit_capability_host::SubagentOutcomeV1,
    ) -> SubagentChildFrameV1 {
        let status = match outcome.stop_reason {
            SubagentStopReasonV1::Completed => ChildStatusV1::Completed,
            SubagentStopReasonV1::Aborted => ChildStatusV1::Cancelled,
            _ => ChildStatusV1::Failed,
        };
        let now = crate::runtime::history::now_label();
        SubagentChildFrameV1 {
            child_id: child_id.to_owned(),
            chat_id: self.context.chat_id.clone(),
            run_id: self.context.run_id.to_string(),
            node_id: self.context.node_id.to_string(),
            parent_invocation_id: self.record.outer_invocation_id.to_string(),
            parent_call_id: self.record.call.call_id.clone(),
            parent_child_id: None,
            // An external delegation is a leaf: Aworkit never nests one.
            depth: 1,
            kind: ChildKindV1::External,
            projection_hash: None,
            projection_items: 0,
            projection_dropped: 0,
            // Aworkit grants the product no tools, and the product's own policy
            // is reported in the summary rather than described as a sandbox.
            inherited_tool_ids: Vec::new(),
            read_only: true,
            head_revision: 1,
            status,
            task: task.to_owned(),
            context_text: format!(
                "External {backend} agent. Product permission mode: {permission_mode}. Only the final answer returns to this Chat."
            ),
            input: json!([{"role": "user", "content": task}]),
            exchanges: Vec::new(),
            final_text: outcome.answer.clone().unwrap_or_default(),
            blocked_actions: Vec::new(),
            model_turns: 1,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// Stable name of one stop reason for a model-facing failure message.
fn stop_reason_name(reason: SubagentStopReasonV1) -> &'static str {
    match reason {
        SubagentStopReasonV1::Completed => "completed",
        SubagentStopReasonV1::Aborted => "cancelled",
        SubagentStopReasonV1::Limit => "a bound was reached",
        SubagentStopReasonV1::AccessPolicy => "an access policy refused it",
        SubagentStopReasonV1::Service => "the product service failed",
        SubagentStopReasonV1::Transport => "the transport failed",
        SubagentStopReasonV1::ProductError => "the product reported an error",
        SubagentStopReasonV1::InvalidResult => "no usable answer was produced",
        SubagentStopReasonV1::Process => "the product process failed",
        SubagentStopReasonV1::Unknown => "an unclassified failure",
    }
}
