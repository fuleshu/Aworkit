//! User Stop preserves a continuation; only the next accepted input consumes it.
use super::*;

impl DesktopRuntime {
    pub(super) fn steering_request_id(
        &self,
        input: &UiCommandInput,
    ) -> Result<Option<StableId>, String> {
        if input.action != "enqueue" {
            return Ok(None);
        }
        let events = self.history.committed_events_shared()?;
        // Retrying a staged command must keep its original continuation choice.
        if let Some(started) = events.iter().find(|event| {
            event.kind == "command.started" && event.payload["requestId"] == input.command_id
        }) {
            return started.payload["steerFromRequestId"]
                .as_str()
                .map(|id| StableId::parse(id.to_owned()).map_err(|error| error.to_string()))
                .transpose();
        }
        for event in events.iter().rev() {
            match event.kind.as_str() {
                "chat.turn_stopped" => {
                    return event.payload["continuationRequestId"]
                        .as_str()
                        .map(|id| StableId::parse(id.to_owned()).map_err(|error| error.to_string()))
                        .transpose();
                }
                "message.user" | "message.assistant" | "chat.cancelled" => break,
                _ => {}
            }
        }
        Ok(None)
    }

    /// Settle the interrupted request without turning the Chat terminal. The
    /// graph checkpoint is durable before its reference becomes visible here.
    pub(super) fn settle_requested_stop(
        &mut self,
        input: &UiCommandInput,
        fingerprint: &str,
        result: &WorkflowExecutionResultV1,
    ) -> Result<Option<UiCommandReceipt>, String> {
        let Some(stop) = self
            .cancellation_controller
            .take_request(result.chat_id.as_str(), result.run_id.as_str())
        else {
            return Ok(None);
        };
        let stopped_node = self.pipeline.stopped_node(&result.request_id)?;
        let created_at = now_label();
        let mut facts = self
            .history
            .open_span_terminal_facts("cancelled", "Response stopped by the user.", &created_at)?
            .into_iter()
            .map(|fact| ("span.cancelled", fact))
            .collect::<Vec<_>>();
        facts.push(("chat.turn_stopped", json!({
            "createdAt":created_at,"stopCommandId":stop.command_id,"commandId":input.command_id,
            "chatId":stop.chat_id,"runId":stop.run_id,"body":"Response stopped by the user.",
            "continuationRequestId":stopped_node.as_ref().map(|_| result.request_id.to_string()),
            "nodeId":stopped_node,
        })));
        self.history
            .append(
                input.command_id.as_str(),
                fingerprint,
                self.history.head()?,
                facts,
            )
            .map(Some)
    }
}
