//! Span-aware producers for the canonical semantic Chat event stream.
//!
//! This module owns no sequence and no presentation callback. Every producer
//! submits a semantic draft through the injected committer; only the exact
//! durably committed envelope is retained for execution bookkeeping.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{
    CancellationToken, ModelEventObserverV1, ModelEventV1, ModelToolEventV1,
};
use serde_json::{Value, json};

use super::{
    graph_pass::GraphNodeActivityV1,
    semantic_events::{CoreEventEnvelope, SemanticEventCommitter, SemanticEventDraft},
};

#[derive(Default)]
struct RunEventState {
    events: Vec<CoreEventEnvelope>,
    started_spans: BTreeSet<String>,
    terminal_spans: BTreeSet<String>,
    parent_spans: BTreeMap<String, String>,
    active_model_node: Option<String>,
    agent_loop_span: Option<String>,
    active_subagent_span: Option<String>,
    tool_requests: BTreeMap<String, String>,
    commit_error: Option<String>,
}

/// Serializes all semantic proposals for one command execution.
pub(crate) struct RunEventStream {
    request_id: String,
    run_id: String,
    committer: Arc<dyn SemanticEventCommitter>,
    cancellation: CancellationToken,
    publish_lock: Mutex<()>,
    state: Mutex<RunEventState>,
}

impl RunEventStream {
    /// Review rationale and usage are separate from assistant messages/reasoning.
    pub(crate) fn publish_approval_review(
        &self,
        call: &aworkit_capability_host::ModelToolCallV1,
        review: &super::approvals::reviewer::ReviewDecision,
    ) {
        self.publish(SemanticEventDraft::new(
            "approval.reviewed",
            json!({
                "requestId":self.request_id,"runId":self.run_id,"createdAt":now_label(),
                "callId":call.call_id,"capabilityId":call.capability_id,
                "decision":review.decision,"reason":review.reason,
                "inputTokens":review.input_tokens,"outputTokens":review.output_tokens,
            }),
        ));
    }
    pub(crate) fn new(
        request_id: String,
        run_id: String,
        committer: Arc<dyn SemanticEventCommitter>,
        cancellation: CancellationToken,
    ) -> Self {
        let state = match committer.committed_events() {
            Ok(events) => rehydrate_state(&request_id, &run_id, events),
            Err(error) => RunEventState {
                commit_error: Some(error),
                ..RunEventState::default()
            },
        };
        let stream = Self {
            request_id,
            run_id,
            committer,
            cancellation,
            publish_lock: Mutex::new(()),
            state: Mutex::new(state),
        };
        stream.ensure_root_span();
        stream
    }

    pub(crate) fn belongs_to(&self, request_id: &str, run_id: &str) -> bool {
        self.request_id == request_id && self.run_id == run_id
    }

    fn run_span_id(&self) -> String {
        format!("span.run.{}.{}", self.run_id, self.request_id)
    }

    fn ensure_root_span(&self) {
        self.start_span(
            self.run_span_id(),
            None,
            "run",
            "run",
            "Run".to_owned(),
            None,
            Value::Null,
        );
    }

    fn publish(&self, draft: SemanticEventDraft) -> Option<CoreEventEnvelope> {
        let _publish = self
            .publish_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .commit_error
            .is_some()
        {
            return None;
        }
        match self.committer.commit(vec![draft]) {
            Ok(mut committed) => {
                let event = committed.pop()?;
                self.state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .events
                    .push(event.clone());
                Some(event)
            }
            Err(error) => {
                self.cancellation.cancel();
                self.state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .commit_error = Some(error);
                None
            }
        }
    }

    pub(crate) fn ensure_healthy(&self) -> Result<(), String> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .commit_error
            .clone()
            .map_or(Ok(()), Err)
    }

    pub(crate) fn events(&self) -> Vec<CoreEventEnvelope> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .events
            .clone()
    }

    fn start_span(
        &self,
        span_id: String,
        parent_span_id: Option<String>,
        span_kind: &str,
        semantic_role: &str,
        title: String,
        input: Option<Value>,
        details: Value,
    ) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.started_spans.contains(&span_id) {
                return;
            }
            if let Some(parent) = parent_span_id.as_ref() {
                if !state.started_spans.contains(parent) || state.terminal_spans.contains(parent) {
                    self.cancellation.cancel();
                    state.commit_error = Some(format!(
                        "span '{span_id}' cannot start under missing or terminal parent '{parent}'"
                    ));
                    return;
                }
                state.parent_spans.insert(span_id.clone(), parent.clone());
            }
            state.started_spans.insert(span_id.clone());
        }
        let mut payload = json!({
            "schemaVersion": 1,
            "requestId": self.request_id,
            "runId": self.run_id,
            "spanId": span_id,
            "parentSpanId": parent_span_id,
            "spanKind": span_kind,
            "semanticRole": semantic_role,
            "title": title,
            "status": "running",
            "createdAt": now_label(),
            "hasInput": input.is_some(),
            "input": input,
        });
        merge_details(&mut payload, details);
        self.publish(SemanticEventDraft::new("span.started", payload));
    }

    fn update_span(&self, span_id: &str, status: &str, body: String, details: Value) {
        let mut payload = json!({
            "schemaVersion": 1,
            "requestId": self.request_id,
            "runId": self.run_id,
            "spanId": span_id,
            "status": status,
            "body": body,
            "createdAt": now_label(),
        });
        merge_details(&mut payload, details);
        self.publish(SemanticEventDraft::new("span.updated", payload));
    }

    fn terminal_span(
        &self,
        span_id: &str,
        status: &str,
        body: String,
        output: Option<Value>,
        details: Value,
    ) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.terminal_spans.contains(span_id) {
                return;
            }
            if !state.started_spans.contains(span_id) {
                self.cancellation.cancel();
                state.commit_error = Some(format!(
                    "span '{span_id}' cannot terminate before it starts"
                ));
                return;
            }
            if let Some(open_child) = state.parent_spans.iter().find_map(|(child, parent)| {
                (parent == span_id && !state.terminal_spans.contains(child)).then_some(child)
            }) {
                self.cancellation.cancel();
                state.commit_error = Some(format!(
                    "span '{span_id}' cannot terminate while child '{open_child}' is open"
                ));
                return;
            }
            state.terminal_spans.insert(span_id.to_owned());
        }
        let kind = match status {
            "cancelled" | "skipped" => "span.cancelled",
            "completed" | "succeeded" => "span.completed",
            _ => "span.failed",
        };
        let mut payload = json!({
            "schemaVersion": 1,
            "requestId": self.request_id,
            "runId": self.run_id,
            "spanId": span_id,
            "status": status,
            "body": body,
            "createdAt": now_label(),
            "hasOutput": output.is_some(),
            "output": output,
        });
        merge_details(&mut payload, details);
        self.publish(SemanticEventDraft::new(kind, payload));
    }

    fn agent_loop_span(&self) -> String {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(span_id) = &state.active_subagent_span
            && state.started_spans.contains(span_id)
            && !state.terminal_spans.contains(span_id)
        {
            return span_id.clone();
        }
        let parent = state
            .active_model_node
            .clone()
            .unwrap_or_else(|| self.run_span_id());
        if let Some(span_id) = &state.agent_loop_span
            && !state.terminal_spans.contains(span_id)
            && state.parent_spans.get(span_id) == Some(&parent)
        {
            return span_id.clone();
        }
        let span_id = format!("span.agent-loop.{}.{}", self.request_id, parent);
        state.agent_loop_span = Some(span_id.clone());
        drop(state);
        self.start_span(
            span_id.clone(),
            Some(parent),
            "agent_loop",
            "agent_loop",
            "Agent".to_owned(),
            None,
            Value::Null,
        );
        span_id
    }

    pub(crate) fn publish_graph_activity(&self, activity: &GraphNodeActivityV1) {
        let span_id = format!(
            "span.node.{}.{}.{}",
            self.run_id, self.request_id, activity.node_id
        );
        let details = json!({
            "nodeId": activity.node_id,
            "nodeType": activity.node_type,
            "label": activity.label,
        });
        if activity.status == "started" {
            self.start_span(
                span_id.clone(),
                Some(self.run_span_id()),
                "graph_node",
                &activity.node_type,
                activity.label.clone(),
                activity.input.clone(),
                details,
            );
            if matches!(activity.node_type.as_str(), "agent" | "model_call") {
                self.state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .active_model_node = Some(span_id);
            }
            return;
        }
        self.start_span(
            span_id.clone(),
            Some(self.run_span_id()),
            "graph_node",
            &activity.node_type,
            activity.label.clone(),
            activity.input.clone(),
            details.clone(),
        );
        if activity.status == "waiting" {
            self.update_span(&span_id, "waiting", activity.summary.clone(), details);
            return;
        }
        if self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active_model_node
            .as_deref()
            == Some(span_id.as_str())
        {
            self.settle_agent_loop(&activity.status, activity.output.clone());
        }
        self.terminal_span(
            &span_id,
            &activity.status,
            activity.summary.clone(),
            activity.output.clone(),
            details,
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.active_model_node.as_deref() == Some(span_id.as_str()) {
            state.active_model_node = None;
            state.agent_loop_span = None;
        }
    }

    pub(crate) fn publish_tool_started(&self, call: &aworkit_capability_host::ModelToolCallV1) {
        let span_id = format!("span.tool.{}", call.call_id);
        let span_kind = if call.capability_id == "tool.subagent" {
            "external_agent"
        } else {
            "tool_call"
        };
        let causation = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .tool_requests
            .get(&call.call_id)
            .cloned();
        self.start_span(
            span_id.clone(),
            Some(self.agent_loop_span()),
            span_kind,
            "tool",
            call.capability_id.clone(),
            Some(tool_call_input(call)),
            json!({
                "callId": call.call_id,
                "capabilityId": call.capability_id,
                "causationEventId": causation,
            }),
        );
        if call.capability_id == "tool.subagent" {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !state.terminal_spans.contains(&span_id) {
                state.active_subagent_span = Some(span_id);
            }
        }
    }

    pub(crate) fn publish_tool_terminal(
        &self,
        call: &aworkit_capability_host::ModelToolCallV1,
        status: &str,
        body: String,
        output: Value,
    ) {
        let span_id = format!("span.tool.{}", call.call_id);
        self.publish_tool_started(call);
        self.terminal_span(
            &span_id,
            status,
            body,
            Some(output),
            json!({
                "callId": call.call_id,
                "capabilityId": call.capability_id,
            }),
        );
        if call.capability_id == "tool.subagent" {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.active_subagent_span.as_deref() == Some(span_id.as_str()) {
                state.active_subagent_span = None;
            }
        }
    }

    /// Keeps an approval-gated tool span open across suspension. The resumed
    /// command will rehydrate this same span and close it with the actual
    /// approved or denied settlement.
    pub(crate) fn publish_tool_waiting(
        &self,
        call: &aworkit_capability_host::ModelToolCallV1,
        body: String,
        output: Value,
    ) {
        let span_id = format!("span.tool.{}", call.call_id);
        self.publish_tool_started(call);
        self.update_span(
            &span_id,
            "waiting",
            body,
            json!({
                "callId": call.call_id,
                "capabilityId": call.capability_id,
                "output": output,
            }),
        );
    }

    fn settle_agent_loop(&self, status: &str, output: Option<Value>) {
        let span_id = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .agent_loop_span
            .clone();
        if let Some(span_id) = span_id {
            if status == "waiting" || status == "awaiting_approval" {
                self.update_span(
                    &span_id,
                    "waiting",
                    "Agent is waiting.".to_owned(),
                    Value::Null,
                );
            } else {
                self.terminal_span(
                    &span_id,
                    status,
                    "Agent loop settled.".to_owned(),
                    output,
                    Value::Null,
                );
            }
        }
    }
}

#[derive(Default)]
struct ModelObserverState {
    turn: u32,
    current_model_span: Option<String>,
}

/// Converts provider callbacks into ModelCall-scoped semantic events.
pub(crate) struct ModelRunEventObserver {
    stream: Arc<RunEventStream>,
    state: Mutex<ModelObserverState>,
}

impl ModelRunEventObserver {
    pub(crate) fn new(stream: Arc<RunEventStream>) -> Self {
        // Approval resume creates a fresh observer for the same durable
        // request. Continue after the highest committed turn so a new provider
        // callback can never target an already-terminal model span.
        let turn = stream
            .events()
            .iter()
            .filter(|event| {
                event.kind == "span.started"
                    && event.payload.get("spanKind").and_then(Value::as_str) == Some("model_call")
            })
            .filter_map(|event| {
                event
                    .payload
                    .get("turn")
                    .and_then(Value::as_u64)
                    .and_then(|turn| u32::try_from(turn).ok())
            })
            .max()
            .unwrap_or(0);
        Self {
            stream,
            state: Mutex::new(ModelObserverState {
                turn,
                current_model_span: None,
            }),
        }
    }

    fn current_model_span(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .current_model_span
            .clone()
    }

    fn content_delta(&self, channel: &str, text: &str, classification: &str) {
        let Some(span_id) = self.current_model_span() else {
            return;
        };
        self.stream.publish(SemanticEventDraft::new(
            "span.content_delta",
            json!({
                "schemaVersion": 1,
                "requestId": self.stream.request_id,
                "runId": self.stream.run_id,
                "spanId": span_id,
                "channel": channel,
                "sourceClassification": classification,
                "append": text,
                "body": text,
                "status": "running",
                "createdAt": now_label(),
            }),
        ));
    }

    fn usage(&self, input_tokens: u64, output_tokens: u64) {
        let Some(span_id) = self.current_model_span() else {
            return;
        };
        self.stream.publish(SemanticEventDraft::new(
            "span.usage",
            json!({
                "schemaVersion": 1,
                "requestId": self.stream.request_id,
                "runId": self.stream.run_id,
                "spanId": span_id,
                "inputTokens": input_tokens,
                "outputTokens": output_tokens,
                "createdAt": now_label(),
            }),
        ));
    }

    fn tool_requested(&self, call: &aworkit_capability_host::ModelToolCallV1) {
        let Some(span_id) = self.current_model_span() else {
            return;
        };
        if let Some(event) = self.stream.publish(SemanticEventDraft::new(
            "tool.requested",
            json!({
                "schemaVersion": 1,
                "requestId": self.stream.request_id,
                "runId": self.stream.run_id,
                "spanId": span_id,
                "callId": call.call_id,
                "capabilityId": call.capability_id,
                "input": tool_call_input(call),
                "createdAt": now_label(),
            }),
        )) {
            self.stream
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .tool_requests
                .insert(call.call_id.clone(), event.event_id);
        }
    }

    pub(crate) fn settle(&self, status: &str) {
        let current = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .current_model_span
            .take();
        if let Some(span_id) = current {
            self.stream.terminal_span(
                &span_id,
                status,
                "Model call ended without a normal provider completion.".to_owned(),
                None,
                Value::Null,
            );
        }
    }

    pub(crate) fn reasoning_snapshot(&self) -> Option<(String, String)> {
        let mut body = String::new();
        let mut category = None;
        for event in self.stream.events() {
            if event.kind != "span.content_delta"
                || event.payload.get("channel").and_then(Value::as_str) != Some("reasoning")
            {
                continue;
            }
            if let Some(text) = event.payload.get("append").and_then(Value::as_str) {
                body.push_str(text);
            }
            category = event
                .payload
                .get("sourceClassification")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(category);
        }
        (!body.is_empty()).then(|| {
            (
                body,
                category.unwrap_or_else(|| "source_provided".to_owned()),
            )
        })
    }
}

impl ModelEventObserverV1 for ModelRunEventObserver {
    fn model_turn_started(&self, input: &Value) {
        let turn = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.turn = state.turn.saturating_add(1);
            state.turn
        };
        let span_id = format!("span.model.{}.turn.{turn}", self.stream.request_id);
        self.stream.start_span(
            span_id.clone(),
            Some(self.stream.agent_loop_span()),
            "model_call",
            "model_call",
            format!("Model call {turn}"),
            Some(input.clone()),
            json!({"turn": turn}),
        );
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .current_model_span = Some(span_id);
    }

    fn model_event(&self, event: &ModelEventV1) {
        match event {
            ModelEventV1::ReasoningRaw(text) => {
                self.content_delta("reasoning", text, "source_provided")
            }
            ModelEventV1::ReasoningSummary(text) => {
                self.content_delta("reasoning", text, "summary")
            }
            ModelEventV1::Progress(text) => self.content_delta("progress", text, "progress"),
            ModelEventV1::AssistantOutput(text) => {
                self.content_delta("assistant_output", text, "assistant_output")
            }
            ModelEventV1::Usage {
                input_tokens,
                output_tokens,
            } => self.usage(*input_tokens, *output_tokens),
        }
    }

    fn model_tool_event(&self, event: &ModelToolEventV1) {
        match event {
            ModelToolEventV1::ReasoningRaw { text } => {
                self.content_delta("reasoning", text, "source_provided")
            }
            ModelToolEventV1::ReasoningSummary { text } => {
                self.content_delta("reasoning", text, "summary")
            }
            ModelToolEventV1::Progress { text } => self.content_delta("progress", text, "progress"),
            ModelToolEventV1::ToolCall { call } => self.tool_requested(call),
            ModelToolEventV1::AssistantOutput { text } => {
                self.content_delta("assistant_output", text, "assistant_output")
            }
            ModelToolEventV1::Usage {
                input_tokens,
                output_tokens,
            } => self.usage(*input_tokens, *output_tokens),
        }
    }

    fn model_turn_completed(&self, output: &Value, status: &str) {
        let current = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .current_model_span
            .take();
        if let Some(span_id) = current {
            self.stream.terminal_span(
                &span_id,
                status,
                "Canonical model result settled.".to_owned(),
                Some(output.clone()),
                Value::Null,
            );
        }
    }
}

fn rehydrate_state(
    request_id: &str,
    run_id: &str,
    events: Vec<CoreEventEnvelope>,
) -> RunEventState {
    let mut state = RunEventState::default();
    let mut model_nodes = Vec::new();
    let mut agent_loops = Vec::new();
    let mut subagents = Vec::new();
    for event in events {
        if event.payload.get("requestId").and_then(Value::as_str) != Some(request_id)
            || event.payload.get("runId").and_then(Value::as_str) != Some(run_id)
        {
            continue;
        }
        if event.kind == "span.started" {
            if let Some(span_id) = event.span_id.clone() {
                state.started_spans.insert(span_id.clone());
                if let Some(parent) = event.payload.get("parentSpanId").and_then(Value::as_str) {
                    state
                        .parent_spans
                        .insert(span_id.clone(), parent.to_owned());
                }
                match event.payload.get("spanKind").and_then(Value::as_str) {
                    Some("agent_loop") => agent_loops.push(span_id.clone()),
                    Some("external_agent") => subagents.push(span_id.clone()),
                    Some("graph_node")
                        if matches!(
                            event.payload.get("semanticRole").and_then(Value::as_str),
                            Some("agent" | "model_call")
                        ) =>
                    {
                        model_nodes.push(span_id);
                    }
                    _ => {}
                }
            }
        } else if matches!(
            event.kind.as_str(),
            "span.completed" | "span.failed" | "span.cancelled"
        ) {
            if let Some(span_id) = event.span_id.clone() {
                state.terminal_spans.insert(span_id);
            }
        } else if event.kind == "tool.requested" {
            if let Some(call_id) = event.payload.get("callId").and_then(Value::as_str) {
                state
                    .tool_requests
                    .insert(call_id.to_owned(), event.event_id.clone());
            }
        }
        state.events.push(event);
    }
    state.active_model_node = model_nodes
        .into_iter()
        .rev()
        .find(|span_id| !state.terminal_spans.contains(span_id));
    state.agent_loop_span = state.active_model_node.as_ref().and_then(|parent| {
        agent_loops.into_iter().rev().find(|span_id| {
            !state.terminal_spans.contains(span_id)
                && state.parent_spans.get(span_id) == Some(parent)
        })
    });
    state.active_subagent_span = subagents
        .into_iter()
        .rev()
        .find(|span_id| !state.terminal_spans.contains(span_id));
    state
}

fn merge_details(target: &mut Value, details: Value) {
    let (Some(target), Value::Object(details)) = (target.as_object_mut(), details) else {
        return;
    };
    target.extend(details);
}

/// Provider correlation stays available without leaking provider-private
/// context into the user-visible semantic stream.
fn tool_call_input(call: &aworkit_capability_host::ModelToolCallV1) -> Value {
    json!({
        "callId": call.call_id,
        "providerCallId": call.provider_call_id,
        "capabilityId": call.capability_id,
        "name": call.name,
        "arguments": call.arguments,
    })
}

fn now_label() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::semantic_events::ephemeral_semantic_event_committer;

    #[test]
    fn model_tool_model_sequence_uses_sibling_spans_and_committed_order() {
        let stream = Arc::new(RunEventStream::new(
            "request.1".into(),
            "run.1".into(),
            ephemeral_semantic_event_committer(),
            CancellationToken::default(),
        ));
        let observer = ModelRunEventObserver::new(stream.clone());
        observer.model_turn_started(&json!({"messages": []}));
        observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "Need files".into(),
        });
        let call = aworkit_capability_host::ModelToolCallV1 {
            call_id: "call.1".into(),
            provider_call_id: Some("call.1".into()),
            capability_id: "tool.files.list".into(),
            name: "list_files".into(),
            arguments: json!({"path":"."}),
            provider_context: None,
        };
        observer.model_tool_event(&ModelToolEventV1::ToolCall { call: call.clone() });
        observer.model_tool_event(&ModelToolEventV1::Usage {
            input_tokens: 11,
            output_tokens: 7,
        });
        observer.model_turn_completed(&json!({"toolCall":"call.1"}), "completed");
        stream.publish_tool_started(&call);
        stream.publish_tool_terminal(&call, "completed", "Listed files".into(), json!(["a"]));
        observer.model_turn_started(&json!({"toolResult":["a"]}));

        let events = stream.events();
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
        );
        let tool_start = events
            .iter()
            .find(|event| {
                event.kind == "span.started" && event.span_id.as_deref() == Some("span.tool.call.1")
            })
            .expect("tool start");
        let tool_parent = tool_start
            .payload
            .get("parentSpanId")
            .and_then(Value::as_str)
            .expect("tool parent");
        assert!(tool_parent.starts_with("span.agent-loop.request.1."));
        assert!(tool_start.causation_event_id.is_some());
        assert!(events.iter().any(|event| {
            event.kind == "span.usage"
                && event.payload.get("inputTokens").and_then(Value::as_u64) == Some(11)
                && event.payload.get("outputTokens").and_then(Value::as_u64) == Some(7)
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.started"
                    && event.payload.get("spanKind").and_then(Value::as_str) == Some("model_call"))
                .count(),
            2
        );
    }

    #[test]
    fn subagent_model_and_tool_spans_remain_owned_by_the_subagent() {
        let stream = Arc::new(RunEventStream::new(
            "request.subagent".into(),
            "run.subagent".into(),
            ephemeral_semantic_event_committer(),
            CancellationToken::default(),
        ));
        let observer = ModelRunEventObserver::new(stream.clone());
        let subagent = aworkit_capability_host::ModelToolCallV1 {
            call_id: "call.subagent".into(),
            provider_call_id: Some("call.subagent".into()),
            capability_id: "tool.subagent".into(),
            name: "spawn_subagent".into(),
            arguments: json!({"task":"Inspect the project"}),
            provider_context: None,
        };
        observer.model_turn_started(&json!({"messages": []}));
        observer.model_tool_event(&ModelToolEventV1::ToolCall {
            call: subagent.clone(),
        });
        observer.model_turn_completed(&json!({"toolCall":"call.subagent"}), "completed");
        stream.publish_tool_started(&subagent);

        observer.model_turn_started(&json!({"messages":["Inspect the project"]}));
        observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "I should list the files.".into(),
        });
        let child_tool = aworkit_capability_host::ModelToolCallV1 {
            call_id: "call.child.list".into(),
            provider_call_id: Some("call.child.list".into()),
            capability_id: "tool.files.list".into(),
            name: "list_files".into(),
            arguments: json!({"path":"."}),
            provider_context: None,
        };
        observer.model_tool_event(&ModelToolEventV1::ToolCall {
            call: child_tool.clone(),
        });
        observer.model_turn_completed(&json!({"toolCall":"call.child.list"}), "completed");
        stream.publish_tool_started(&child_tool);
        stream.publish_tool_terminal(
            &child_tool,
            "completed",
            "Listed files".into(),
            json!(["Cargo.toml"]),
        );
        observer.model_turn_started(&json!({"toolResult":["Cargo.toml"]}));
        observer.model_tool_event(&ModelToolEventV1::AssistantOutput {
            text: "The project contains Cargo.toml.".into(),
        });
        observer.model_turn_completed(
            &json!({"assistant":"The project contains Cargo.toml."}),
            "completed",
        );
        stream.publish_tool_terminal(
            &subagent,
            "completed",
            "Subagent completed".into(),
            json!({"finalText":"The project contains Cargo.toml."}),
        );

        let events = stream.events();
        let subagent_span = "span.tool.call.subagent";
        let child_starts = events.iter().filter(|event| {
            event.kind == "span.started"
                && matches!(
                    event.payload.get("spanKind").and_then(Value::as_str),
                    Some("model_call" | "tool_call")
                )
                && event.payload.get("parentSpanId").and_then(Value::as_str) == Some(subagent_span)
        });
        assert_eq!(
            child_starts.count(),
            3,
            "two child model calls and one child tool"
        );
        assert!(events.iter().any(|event| {
            event.kind == "span.completed"
                && event.span_id.as_deref() == Some(subagent_span)
                && event.payload["output"]["finalText"]
                    == Value::String("The project contains Cargo.toml.".into())
        }));
    }

    #[test]
    fn suspended_stream_rehydrates_open_spans_without_duplicate_starts() {
        let committer = ephemeral_semantic_event_committer();
        let first = RunEventStream::new(
            "request.resume".into(),
            "run.resume".into(),
            committer.clone(),
            CancellationToken::default(),
        );
        first.publish_graph_activity(&GraphNodeActivityV1 {
            node_id: "gate.1".into(),
            node_type: "approval".into(),
            label: "Approval".into(),
            status: "started".into(),
            summary: "Approval started".into(),
            input: Some(json!({"proposal":"review"})),
            output: None,
        });
        first.publish_graph_activity(&GraphNodeActivityV1 {
            node_id: "gate.1".into(),
            node_type: "approval".into(),
            label: "Approval".into(),
            status: "waiting".into(),
            summary: "Waiting".into(),
            input: Some(json!({"proposal":"review"})),
            output: None,
        });

        let resumed = RunEventStream::new(
            "request.resume".into(),
            "run.resume".into(),
            committer,
            CancellationToken::default(),
        );
        resumed.publish_graph_activity(&GraphNodeActivityV1 {
            node_id: "gate.1".into(),
            node_type: "approval".into(),
            label: "Approval".into(),
            status: "completed".into(),
            summary: "Approved".into(),
            input: None,
            output: Some(json!({"approved":true})),
        });
        resumed.ensure_healthy().unwrap();

        let events = resumed.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.started"
                    && event.payload.get("spanKind").and_then(Value::as_str) == Some("run"))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.started"
                    && event.payload.get("nodeId").and_then(Value::as_str) == Some("gate.1"))
                .count(),
            1
        );
        assert!(events.iter().any(|event| {
            event.kind == "span.completed"
                && event.payload.get("nodeId").and_then(Value::as_str) == Some("gate.1")
        }));
    }

    #[test]
    fn resumed_observer_continues_after_terminal_model_turns() {
        let committer = ephemeral_semantic_event_committer();
        let first = Arc::new(RunEventStream::new(
            "request.approval-resume".into(),
            "run.approval-resume".into(),
            committer.clone(),
            CancellationToken::default(),
        ));
        first.publish_graph_activity(&GraphNodeActivityV1 {
            node_id: "agent.1".into(),
            node_type: "agent".into(),
            label: "Agent".into(),
            status: "started".into(),
            summary: "Agent started".into(),
            input: None,
            output: None,
        });
        let observer = ModelRunEventObserver::new(first);
        observer.model_turn_started(&json!({"messages": []}));
        observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "I need approval.".into(),
        });
        observer.model_turn_completed(&json!({"toolCall":"call.edit"}), "completed");

        let resumed = Arc::new(RunEventStream::new(
            "request.approval-resume".into(),
            "run.approval-resume".into(),
            committer,
            CancellationToken::default(),
        ));
        let resumed_observer = ModelRunEventObserver::new(resumed.clone());
        resumed_observer.model_turn_started(&json!({"toolResult": ["approved"]}));
        resumed_observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "The edit is approved.".into(),
        });
        resumed_observer.model_turn_completed(&json!({"assistant":"done"}), "completed");
        resumed.ensure_healthy().unwrap();

        let model_spans = resumed
            .events()
            .into_iter()
            .filter(|event| {
                event.kind == "span.started"
                    && event.payload.get("spanKind").and_then(Value::as_str) == Some("model_call")
            })
            .filter_map(|event| event.span_id)
            .collect::<Vec<_>>();
        assert_eq!(
            model_spans,
            vec![
                "span.model.request.approval-resume.turn.1",
                "span.model.request.approval-resume.turn.2",
            ]
        );
    }

    #[test]
    fn approval_suspension_keeps_the_tool_span_open_for_resume() {
        let committer = ephemeral_semantic_event_committer();
        let first = Arc::new(RunEventStream::new(
            "request.tool-approval".into(),
            "run.tool-approval".into(),
            committer.clone(),
            CancellationToken::default(),
        ));
        first.publish_graph_activity(&GraphNodeActivityV1 {
            node_id: "agent.1".into(),
            node_type: "agent".into(),
            label: "Agent".into(),
            status: "started".into(),
            summary: "Agent started".into(),
            input: None,
            output: None,
        });
        let call = aworkit_capability_host::ModelToolCallV1 {
            call_id: "call.approved-edit".into(),
            provider_call_id: Some("call.approved-edit".into()),
            capability_id: "tool.files.edit".into(),
            name: "edit_file".into(),
            arguments: json!({"path":"notes.txt"}),
            provider_context: None,
        };
        first.publish_tool_waiting(&call, "Approval required".into(), json!({"pending":true}));
        first.ensure_healthy().unwrap();

        let resumed = RunEventStream::new(
            "request.tool-approval".into(),
            "run.tool-approval".into(),
            committer,
            CancellationToken::default(),
        );
        resumed.publish_tool_started(&call);
        resumed.publish_tool_terminal(&call, "completed", "Edited file".into(), json!({"ok":true}));
        resumed.ensure_healthy().unwrap();

        let events = resumed.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.started"
                    && event.span_id.as_deref() == Some("span.tool.call.approved-edit"))
                .count(),
            1
        );
        assert!(events.iter().any(|event| {
            event.kind == "span.updated"
                && event.span_id.as_deref() == Some("span.tool.call.approved-edit")
                && event.payload.get("status").and_then(Value::as_str) == Some("waiting")
        }));
        assert!(events.iter().any(|event| {
            event.kind == "span.completed"
                && event.span_id.as_deref() == Some("span.tool.call.approved-edit")
        }));
    }
}
