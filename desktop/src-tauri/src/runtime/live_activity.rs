//! Best-effort live Chat activity projection.
//!
//! These events make in-flight provider and tool work observable without
//! turning presentation delivery into canonical history or an authority path.

use std::sync::{Arc, Mutex};

use aworkit_capability_host::{
    ModelEventObserverV1, ModelEventV1, ModelToolEventV1,
};
use serde::Serialize;
use serde_json::Value;

use super::graph_pass::GraphNodeActivityV1;

const MAXIMUM_LIVE_BODY_BYTES: usize = 16 * 1024;

/// A transient, secret-safe activity update consumed by the Chat UI.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveChatActivityV1 {
    pub request_id: String,
    pub run_id: String,
    pub activity_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
}

/// Non-authoritative sink. Delivery failures never alter Run execution.
pub trait LiveChatActivityPort: Send + Sync {
    fn publish(&self, activity: LiveChatActivityV1);
}

#[derive(Default)]
pub(crate) struct NoopLiveChatActivity;

impl LiveChatActivityPort for NoopLiveChatActivity {
    fn publish(&self, _activity: LiveChatActivityV1) {}
}

pub(crate) fn noop_live_activity() -> Arc<dyn LiveChatActivityPort> {
    Arc::new(NoopLiveChatActivity)
}

/// Adapts provider-neutral model events into transient Chat cards.
pub(crate) struct ModelLiveActivityObserver {
    request_id: String,
    run_id: String,
    sink: Arc<dyn LiveChatActivityPort>,
    reasoning_body: Mutex<String>,
    response_body: Mutex<String>,
}

impl ModelLiveActivityObserver {
    pub(crate) fn new(
        request_id: String,
        run_id: String,
        sink: Arc<dyn LiveChatActivityPort>,
    ) -> Self {
        Self {
            request_id,
            run_id,
            sink,
            reasoning_body: Mutex::new(String::new()),
            response_body: Mutex::new(String::new()),
        }
    }

    fn reasoning(&self, body: &str, category: &str) {
        self.sink.publish(LiveChatActivityV1 {
            request_id: self.request_id.clone(),
            run_id: self.run_id.clone(),
            activity_id: format!("model.reasoning.{}", self.request_id),
            kind: "reasoning".into(),
            title: "Thinking".into(),
            body: append_live_text(&self.reasoning_body, body),
            status: "running".into(),
            reasoning_category: Some(category.into()),
            capability_id: None,
        });
    }

    fn response(&self, body: &str) {
        self.sink.publish(LiveChatActivityV1 {
            request_id: self.request_id.clone(),
            run_id: self.run_id.clone(),
            activity_id: format!("model.response.{}", self.request_id),
            kind: "response".into(),
            title: "Response".into(),
            body: append_live_text(&self.response_body, body),
            status: "running".into(),
            reasoning_category: None,
            capability_id: None,
        });
    }

    fn progress(&self, body: &str) {
        self.sink.publish(LiveChatActivityV1 {
            request_id: self.request_id.clone(),
            run_id: self.run_id.clone(),
            activity_id: format!("model.progress.{}", self.request_id),
            kind: "thinking".into(),
            title: "Working".into(),
            body: bounded_live_text(body),
            status: "running".into(),
            reasoning_category: Some("progress".into()),
            capability_id: None,
        });
    }

    fn tool_call(&self, call: &aworkit_capability_host::ModelToolCallV1) {
        self.sink.publish(LiveChatActivityV1 {
            request_id: self.request_id.clone(),
            run_id: self.run_id.clone(),
            activity_id: format!("tool.{}", call.call_id),
            kind: "tool".into(),
            title: call.capability_id.clone(),
            body: bounded_json(&call.arguments),
            status: "running".into(),
            reasoning_category: None,
            capability_id: Some(call.capability_id.clone()),
        });
    }
}

impl ModelEventObserverV1 for ModelLiveActivityObserver {
    fn model_event(&self, event: &ModelEventV1) {
        match event {
            ModelEventV1::ReasoningRaw(text) => self.reasoning(text, "source_provided"),
            ModelEventV1::ReasoningSummary(text) => self.reasoning(text, "summary"),
            ModelEventV1::Progress(text) => self.progress(text),
            ModelEventV1::AssistantOutput(text) => self.response(text),
            ModelEventV1::Usage { .. } => {}
        }
    }

    fn model_tool_event(&self, event: &ModelToolEventV1) {
        match event {
            ModelToolEventV1::ReasoningRaw { text } => {
                self.reasoning(text, "source_provided");
            }
            ModelToolEventV1::ReasoningSummary { text } => self.reasoning(text, "summary"),
            ModelToolEventV1::Progress { text } => self.progress(text),
            ModelToolEventV1::ToolCall { call } => self.tool_call(call),
            ModelToolEventV1::AssistantOutput { text } => self.response(text),
            ModelToolEventV1::Usage { .. } => {}
        }
    }
}

/// Publishes one graph transition at the instant the pass records it.
pub(crate) fn publish_graph_activity(
    sink: &Arc<dyn LiveChatActivityPort>,
    request_id: &str,
    run_id: &str,
    activity: &GraphNodeActivityV1,
) {
    sink.publish(LiveChatActivityV1 {
        request_id: request_id.to_owned(),
        run_id: run_id.to_owned(),
        activity_id: format!("node.{request_id}.{}", activity.node_id),
        kind: "step".into(),
        title: activity.label.clone(),
        body: bounded_live_text(&format!("{}: {}", activity.node_type, activity.summary)),
        status: activity.status.clone(),
        reasoning_category: None,
        capability_id: None,
    });
}

fn append_live_text(buffer: &Mutex<String>, chunk: &str) -> String {
    let mut body = buffer.lock().unwrap_or_else(|poison| poison.into_inner());
    body.push_str(chunk);
    if body.len() > MAXIMUM_LIVE_BODY_BYTES {
        let mut boundary = body
            .len()
            .saturating_sub(MAXIMUM_LIVE_BODY_BYTES.saturating_sub(3));
        while !body.is_char_boundary(boundary) {
            boundary = boundary.saturating_add(1);
        }
        *body = format!("…{}", &body[boundary..]);
    }
    body.clone()
}

pub(crate) fn bounded_json(value: &Value) -> String {
    bounded_live_text(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into()))
}

pub(crate) fn bounded_live_text(value: &str) -> String {
    if value.len() <= MAXIMUM_LIVE_BODY_BYTES {
        return value.to_owned();
    }
    let mut boundary = MAXIMUM_LIVE_BODY_BYTES.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    format!("{}…", &value[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<LiveChatActivityV1>>);

    impl LiveChatActivityPort for RecordingSink {
        fn publish(&self, activity: LiveChatActivityV1) {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(activity);
        }
    }

    #[test]
    fn model_chunks_are_published_cumulatively_before_settlement() {
        let sink = Arc::new(RecordingSink::default());
        let observer = ModelLiveActivityObserver::new(
            "request.1".into(),
            "run.1".into(),
            sink.clone(),
        );
        observer.model_event(&ModelEventV1::ReasoningRaw("first ".into()));
        observer.model_event(&ModelEventV1::ReasoningRaw("second".into()));
        observer.model_event(&ModelEventV1::AssistantOutput("answer ".into()));
        observer.model_event(&ModelEventV1::AssistantOutput("chunk".into()));

        let activities = sink
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(activities[0].body, "first ");
        assert_eq!(activities[1].body, "first second");
        assert_eq!(activities[2].kind, "response");
        assert_eq!(activities[3].body, "answer chunk");
    }
}
