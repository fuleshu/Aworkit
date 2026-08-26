//! Ordered Run activity events shared by live Chat and durable replay.
//!
//! Producers publish one typed transition through this stream. The stream
//! assigns the only Run-local sequence, retains the event for settlement, and
//! forwards the same envelope to the presentation callback. Repeated deltas
//! share an `activity_id`; each transition still has its own `event_id`.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender, SyncSender},
    },
    thread,
    time::Duration,
};

use aworkit_capability_host::{ModelEventObserverV1, ModelEventV1, ModelToolEventV1};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graph_pass::GraphNodeActivityV1;

/// One immutable transition in a Run's ordered activity stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunEventEnvelopeV1 {
    pub schema_version: u16,
    pub request_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event_id: String,
    pub activity_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    /// `append` adds streamed text, `replace` replaces data, and `retain`
    /// changes lifecycle metadata without touching existing data.
    pub data_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
}

/// Durable semantic activity reduced from one or more ordered transitions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunActivitySnapshotV1 {
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub activity_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
}

/// Best-effort subscriber. It observes sequenced stream order but owns no
/// authority and cannot affect execution if presentation delivery fails.
pub trait RunEventPort: Send + Sync {
    fn publish(&self, event: RunEventEnvelopeV1);
}

#[derive(Default)]
struct NoopRunEventPort;

impl RunEventPort for NoopRunEventPort {
    fn publish(&self, _event: RunEventEnvelopeV1) {}
}

pub(crate) fn noop_run_event_port() -> Arc<dyn RunEventPort> {
    Arc::new(NoopRunEventPort)
}

#[derive(Clone, Debug)]
pub(crate) struct RunEventDraftV1 {
    pub activity_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub data_mode: String,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub turn: Option<u32>,
    pub node_id: Option<String>,
    pub node_type: Option<String>,
    pub call_id: Option<String>,
    pub reasoning_category: Option<String>,
    pub capability_id: Option<String>,
}

impl RunEventDraftV1 {
    fn lifecycle(activity_id: String, kind: &str, title: String, status: &str) -> Self {
        Self {
            activity_id,
            kind: kind.to_owned(),
            title,
            body: String::new(),
            status: status.to_owned(),
            data_mode: "retain".to_owned(),
            input: None,
            output: None,
            turn: None,
            node_id: None,
            node_type: None,
            call_id: None,
            reasoning_category: None,
            capability_id: None,
        }
    }
}

#[derive(Default)]
struct RunEventStateV1 {
    next_sequence: u64,
    prefix: Vec<RunActivitySnapshotV1>,
    events: Vec<RunEventEnvelopeV1>,
}

enum RunEventDeliveryV1 {
    Event(RunEventEnvelopeV1),
    Flush(SyncSender<()>),
}

/// Per-execution event sequencer and callback fan-out.
///
/// The delivery mutex covers sequence allocation and callback publication, so
/// concurrent node producers cannot expose event N+1 before event N. The
/// callback itself is expected to enqueue/emit and return immediately.
pub(crate) struct RunEventStreamV1 {
    request_id: String,
    run_id: String,
    delivery_sender: Sender<RunEventDeliveryV1>,
    delivery: Mutex<()>,
    state: Mutex<RunEventStateV1>,
}

impl RunEventStreamV1 {
    pub(crate) fn new(
        request_id: String,
        run_id: String,
        subscriber: Arc<dyn RunEventPort>,
    ) -> Self {
        Self {
            request_id,
            run_id,
            delivery_sender: spawn_delivery_worker(subscriber),
            delivery: Mutex::new(()),
            state: Mutex::new(RunEventStateV1::default()),
        }
    }

    pub(crate) fn belongs_to(&self, request_id: &str, run_id: &str) -> bool {
        self.request_id == request_id && self.run_id == run_id
    }

    pub(crate) fn resume(
        request_id: String,
        run_id: String,
        subscriber: Arc<dyn RunEventPort>,
        prefix: Vec<RunActivitySnapshotV1>,
    ) -> Self {
        let next_sequence = prefix
            .iter()
            .map(|activity| activity.last_sequence)
            .max()
            .unwrap_or(0);
        Self {
            request_id,
            run_id,
            delivery_sender: spawn_delivery_worker(subscriber),
            delivery: Mutex::new(()),
            state: Mutex::new(RunEventStateV1 {
                next_sequence,
                prefix,
                events: Vec::new(),
            }),
        }
    }

    pub(crate) fn publish(&self, draft: RunEventDraftV1) -> RunEventEnvelopeV1 {
        let _delivery = self
            .delivery
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let envelope = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.next_sequence = state.next_sequence.saturating_add(1);
            let sequence = state.next_sequence;
            let envelope = RunEventEnvelopeV1 {
                schema_version: 1,
                request_id: self.request_id.clone(),
                run_id: self.run_id.clone(),
                sequence,
                event_id: format!("run.event.{}.{}", self.request_id, sequence),
                activity_id: draft.activity_id,
                kind: draft.kind,
                title: draft.title,
                body: draft.body,
                status: draft.status,
                data_mode: draft.data_mode,
                input: draft.input,
                output: draft.output,
                turn: draft.turn,
                node_id: draft.node_id,
                node_type: draft.node_type,
                call_id: draft.call_id,
                reasoning_category: draft.reasoning_category,
                capability_id: draft.capability_id,
            };
            state.events.push(envelope.clone());
            envelope
        };
        // An unbounded std channel only enqueues here; the Run never waits for
        // a WebView subscriber or any other presentation callback.
        let _ = self
            .delivery_sender
            .send(RunEventDeliveryV1::Event(envelope.clone()));
        envelope
    }

    /// Waits until all events published before this barrier have reached the
    /// callback. Pipelines use it immediately before returning a terminal or
    /// suspended result, preventing a late busy update from racing settlement.
    pub(crate) fn flush(&self) {
        let (acknowledge, acknowledged) = mpsc::sync_channel(0);
        {
            let _delivery = self
                .delivery
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if self
                .delivery_sender
                .send(RunEventDeliveryV1::Flush(acknowledge))
                .is_err()
            {
                return;
            }
        }
        // Presentation remains non-authoritative: a broken subscriber cannot
        // block durable settlement indefinitely.
        let _ = acknowledged.recv_timeout(Duration::from_millis(250));
    }

    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<RunEventEnvelopeV1> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .events
            .clone()
    }

    /// Reduces streamed deltas into durable semantic activities while retaining
    /// the first-observed order and source sequence range of each activity.
    pub(crate) fn activity_snapshots(&self) -> Vec<RunActivitySnapshotV1> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        reduce_run_events(&state.prefix, &state.events)
    }

    /// Returns activities created or changed after a durable suspension point.
    pub(crate) fn activity_snapshots_after(&self, sequence: u64) -> Vec<RunActivitySnapshotV1> {
        self.activity_snapshots()
            .into_iter()
            .filter(|activity| activity.last_sequence > sequence)
            .collect()
    }

    pub(crate) fn publish_graph_activity(&self, activity: &GraphNodeActivityV1) {
        let mut draft = RunEventDraftV1::lifecycle(
            format!("node.{}.{}", self.request_id, activity.node_id),
            "step",
            activity.label.clone(),
            &activity.status,
        );
        draft.body = activity.summary.clone();
        draft.data_mode = "replace".to_owned();
        draft.input = activity.input.clone();
        draft.output = activity.output.clone();
        draft.node_id = Some(activity.node_id.clone());
        draft.node_type = Some(activity.node_type.clone());
        self.publish(draft);
    }

    pub(crate) fn publish_tool_started(&self, call: &aworkit_capability_host::ModelToolCallV1) {
        let mut draft = RunEventDraftV1::lifecycle(
            format!("tool.{}", call.call_id),
            "tool",
            call.capability_id.clone(),
            "running",
        );
        draft.body = "Tool invocation started.".to_owned();
        draft.data_mode = "replace".to_owned();
        draft.input = serde_json::to_value(call).ok();
        draft.call_id = Some(call.call_id.clone());
        draft.capability_id = Some(call.capability_id.clone());
        self.publish(draft);
    }

    pub(crate) fn publish_tool_terminal(
        &self,
        call: &aworkit_capability_host::ModelToolCallV1,
        status: &str,
        body: String,
        output: Value,
    ) {
        let mut draft = RunEventDraftV1::lifecycle(
            format!("tool.{}", call.call_id),
            "tool",
            call.capability_id.clone(),
            status,
        );
        draft.body = body;
        draft.data_mode = "replace".to_owned();
        draft.output = Some(output);
        draft.call_id = Some(call.call_id.clone());
        draft.capability_id = Some(call.capability_id.clone());
        self.publish(draft);
    }
}

fn spawn_delivery_worker(subscriber: Arc<dyn RunEventPort>) -> Sender<RunEventDeliveryV1> {
    let (sender, receiver) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("aworkit-run-event-delivery".to_owned())
        .spawn(move || {
            while let Ok(delivery) = receiver.recv() {
                match delivery {
                    RunEventDeliveryV1::Event(event) => subscriber.publish(event),
                    RunEventDeliveryV1::Flush(acknowledge) => {
                        let _ = acknowledge.send(());
                    }
                }
            }
        });
    sender
}

fn reduce_run_events(
    prefix: &[RunActivitySnapshotV1],
    events: &[RunEventEnvelopeV1],
) -> Vec<RunActivitySnapshotV1> {
    let mut positions = BTreeMap::<String, usize>::new();
    let mut activities = prefix.to_vec();
    for (index, activity) in activities.iter().enumerate() {
        positions.insert(activity.activity_id.clone(), index);
    }
    for event in events {
        let index = if let Some(index) = positions.get(&event.activity_id) {
            *index
        } else {
            let index = activities.len();
            positions.insert(event.activity_id.clone(), index);
            activities.push(RunActivitySnapshotV1 {
                first_sequence: event.sequence,
                last_sequence: event.sequence,
                activity_id: event.activity_id.clone(),
                kind: event.kind.clone(),
                title: event.title.clone(),
                body: String::new(),
                status: event.status.clone(),
                input: None,
                output: None,
                turn: event.turn,
                node_id: event.node_id.clone(),
                node_type: event.node_type.clone(),
                call_id: event.call_id.clone(),
                reasoning_category: event.reasoning_category.clone(),
                capability_id: event.capability_id.clone(),
            });
            index
        };
        let activity = &mut activities[index];
        activity.last_sequence = event.sequence;
        activity.kind = event.kind.clone();
        activity.title = event.title.clone();
        activity.status = event.status.clone();
        activity.turn = event.turn.or(activity.turn);
        activity.node_id = event.node_id.clone().or_else(|| activity.node_id.clone());
        activity.node_type = event
            .node_type
            .clone()
            .or_else(|| activity.node_type.clone());
        activity.call_id = event.call_id.clone().or_else(|| activity.call_id.clone());
        activity.reasoning_category = event
            .reasoning_category
            .clone()
            .or_else(|| activity.reasoning_category.clone());
        activity.capability_id = event
            .capability_id
            .clone()
            .or_else(|| activity.capability_id.clone());
        if let Some(input) = &event.input {
            activity.input = Some(input.clone());
        }
        match event.data_mode.as_str() {
            "append" => {
                append_value(&mut activity.output, event.output.as_ref());
                activity.body.push_str(&event.body);
            }
            "replace" => {
                if let Some(output) = &event.output {
                    activity.output = Some(output.clone());
                }
                if !event.body.is_empty() {
                    activity.body.clone_from(&event.body);
                }
            }
            _ => {}
        }
    }
    activities
}

fn append_value(target: &mut Option<Value>, incoming: Option<&Value>) {
    let Some(incoming) = incoming else { return };
    match (target.as_mut(), incoming) {
        (Some(Value::String(current)), Value::String(chunk)) => current.push_str(chunk),
        _ => *target = Some(incoming.clone()),
    }
}

#[derive(Default)]
struct ModelObserverStateV1 {
    turn: u32,
    turn_open: bool,
    reasoning_seen: bool,
    response_seen: bool,
    progress_seen: bool,
    reasoning_category: Option<String>,
}

/// Converts provider callbacks into turn-scoped Run events.
pub(crate) struct ModelRunEventObserverV1 {
    stream: Arc<RunEventStreamV1>,
    state: Mutex<ModelObserverStateV1>,
}

impl ModelRunEventObserverV1 {
    pub(crate) fn new(stream: Arc<RunEventStreamV1>) -> Self {
        Self {
            stream,
            state: Mutex::new(ModelObserverStateV1::default()),
        }
    }

    fn current_turn(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .turn
    }

    fn text_delta(&self, kind: &str, title: &str, text: &str, category: Option<&str>) {
        let turn = self.current_turn();
        let mut draft = RunEventDraftV1::lifecycle(
            format!("model.{kind}.{}.turn.{turn}", self.stream.request_id),
            kind,
            title.to_owned(),
            "running",
        );
        draft.body = text.to_owned();
        draft.data_mode = "append".to_owned();
        draft.output = Some(Value::String(text.to_owned()));
        draft.turn = Some(turn);
        draft.reasoning_category = category.map(str::to_owned);
        self.stream.publish(draft);
    }

    fn reasoning(&self, text: &str, category: &str) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.reasoning_seen = true;
            state.reasoning_category = Some(category.to_owned());
        }
        self.text_delta("reasoning", "Thinking", text, Some(category));
    }

    fn response(&self, text: &str) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .response_seen = true;
        self.text_delta("response", "Response", text, None);
    }

    fn progress(&self, text: &str) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .progress_seen = true;
        self.text_delta("progress", "Working", text, Some("progress"));
    }

    fn tool_call(&self, call: &aworkit_capability_host::ModelToolCallV1) {
        let turn = self.current_turn();
        let mut draft = RunEventDraftV1::lifecycle(
            format!("tool.{}", call.call_id),
            "tool",
            call.capability_id.clone(),
            "requested",
        );
        draft.body = "The model requested this tool invocation.".to_owned();
        draft.data_mode = "replace".to_owned();
        draft.input = serde_json::to_value(call).ok();
        draft.turn = Some(turn);
        draft.call_id = Some(call.call_id.clone());
        draft.capability_id = Some(call.capability_id.clone());
        self.stream.publish(draft);
    }

    fn finish_open_activities(&self, status: &str) {
        let (turn, reasoning_seen, response_seen, progress_seen, category) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let snapshot = (
                state.turn,
                state.reasoning_seen,
                state.response_seen,
                state.progress_seen,
                state.reasoning_category.take(),
            );
            state.reasoning_seen = false;
            state.response_seen = false;
            state.progress_seen = false;
            snapshot
        };
        for (kind, title, seen, reasoning_category) in [
            ("reasoning", "Thinking", reasoning_seen, category),
            (
                "progress",
                "Working",
                progress_seen,
                Some("progress".to_owned()),
            ),
            ("response", "Response", response_seen, None),
        ] {
            if !seen {
                continue;
            }
            let mut draft = RunEventDraftV1::lifecycle(
                format!("model.{kind}.{}.turn.{turn}", self.stream.request_id),
                kind,
                title.to_owned(),
                status,
            );
            draft.turn = Some(turn);
            draft.reasoning_category = reasoning_category;
            self.stream.publish(draft);
        }
    }

    /// Closes any provider activity left open by an error outside the normal
    /// model-turn completion callback.
    pub(crate) fn settle(&self, status: &str) {
        self.finish_open_activities(status);
        let turn = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !state.turn_open {
                return;
            }
            state.turn_open = false;
            state.turn
        };
        let mut draft = RunEventDraftV1::lifecycle(
            format!("model.turn.{}.{}", self.stream.request_id, turn),
            "model_turn",
            format!("Model turn {turn}"),
            status,
        );
        draft.body = "Model turn ended without a normal provider completion.".to_owned();
        draft.data_mode = "replace".to_owned();
        draft.turn = Some(turn);
        self.stream.publish(draft);
    }

    pub(crate) fn reasoning_snapshot(&self) -> Option<(String, String)> {
        let mut body = String::new();
        let mut category = None;
        for activity in self.stream.activity_snapshots() {
            if activity.kind != "reasoning" || activity.body.is_empty() {
                continue;
            }
            body.push_str(&activity.body);
            category = activity.reasoning_category.or(category);
        }
        (!body.is_empty()).then(|| {
            (
                body,
                category.unwrap_or_else(|| "source_provided".to_owned()),
            )
        })
    }
}

impl ModelEventObserverV1 for ModelRunEventObserverV1 {
    fn model_turn_started(&self, input: &Value) {
        let turn = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.turn = state.turn.saturating_add(1);
            state.turn_open = true;
            state.reasoning_seen = false;
            state.response_seen = false;
            state.progress_seen = false;
            state.reasoning_category = None;
            state.turn
        };
        let mut draft = RunEventDraftV1::lifecycle(
            format!("model.turn.{}.{}", self.stream.request_id, turn),
            "model_turn",
            format!("Model turn {turn}"),
            "running",
        );
        draft.body = "Normalized model request accepted for dispatch.".to_owned();
        draft.data_mode = "replace".to_owned();
        draft.input = Some(input.clone());
        draft.turn = Some(turn);
        self.stream.publish(draft);
    }

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
            ModelToolEventV1::ReasoningRaw { text } => self.reasoning(text, "source_provided"),
            ModelToolEventV1::ReasoningSummary { text } => self.reasoning(text, "summary"),
            ModelToolEventV1::Progress { text } => self.progress(text),
            ModelToolEventV1::ToolCall { call } => self.tool_call(call),
            ModelToolEventV1::AssistantOutput { text } => self.response(text),
            ModelToolEventV1::Usage { .. } => {}
        }
    }

    fn model_turn_completed(&self, output: &Value, status: &str) {
        self.finish_open_activities(status);
        let turn = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.turn_open = false;
            state.turn
        };
        let mut draft = RunEventDraftV1::lifecycle(
            format!("model.turn.{}.{}", self.stream.request_id, turn),
            "model_turn",
            format!("Model turn {turn}"),
            status,
        );
        draft.body = "Normalized model event stream settled.".to_owned();
        draft.data_mode = "replace".to_owned();
        draft.output = Some(output.clone());
        draft.turn = Some(turn);
        self.stream.publish(draft);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct RecordingRunEventPort(Mutex<Vec<RunEventEnvelopeV1>>);

    impl RunEventPort for RecordingRunEventPort {
        fn publish(&self, event: RunEventEnvelopeV1) {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(event);
        }
    }

    #[test]
    fn turn_scoped_events_keep_real_order_and_reduce_streamed_text() {
        let stream = Arc::new(RunEventStreamV1::new(
            "request.1".to_owned(),
            "run.1".to_owned(),
            noop_run_event_port(),
        ));
        let observer = ModelRunEventObserverV1::new(stream.clone());
        observer.model_turn_started(&json!({"messages":[{"role":"user","content":"list"}]}));
        observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "Need ".into(),
        });
        observer.model_tool_event(&ModelToolEventV1::ReasoningRaw {
            text: "files".into(),
        });
        observer.model_turn_completed(&json!([{"kind":"usage"}]), "completed");
        observer.model_turn_started(&json!({"toolResult":{"files":["a.txt"]}}));

        let events = stream.events();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            (1..=6).collect::<Vec<_>>()
        );
        assert_eq!(events[0].kind, "model_turn");
        assert_eq!(events[1].kind, "reasoning");
        assert_eq!(events[4].kind, "model_turn");
        assert_eq!(events[5].turn, Some(2));
        let thinking = stream
            .activity_snapshots()
            .into_iter()
            .find(|activity| activity.kind == "reasoning")
            .expect("reasoning snapshot");
        assert_eq!(thinking.output, Some(Value::String("Need files".into())));
        assert_eq!(thinking.status, "completed");
    }

    #[test]
    fn abnormal_settlement_closes_every_open_model_activity() {
        let stream = Arc::new(RunEventStreamV1::new(
            "request.failed".to_owned(),
            "run.failed".to_owned(),
            noop_run_event_port(),
        ));
        let observer = ModelRunEventObserverV1::new(stream.clone());
        observer.model_turn_started(&json!({"messages":[]}));
        observer.model_event(&ModelEventV1::ReasoningRaw("partial".into()));
        observer.settle("failed");

        let activities = stream.activity_snapshots();
        assert_eq!(activities.len(), 2);
        assert!(
            activities
                .iter()
                .all(|activity| activity.status == "failed")
        );
        assert_eq!(activities[0].kind, "model_turn");
        assert_eq!(activities[1].kind, "reasoning");
    }

    #[test]
    fn asynchronous_callback_queue_preserves_sequence_and_flushes() {
        let subscriber = Arc::new(RecordingRunEventPort::default());
        let stream = Arc::new(RunEventStreamV1::new(
            "request.async".to_owned(),
            "run.async".to_owned(),
            subscriber.clone(),
        ));
        let observer = ModelRunEventObserverV1::new(stream.clone());
        observer.model_turn_started(&json!({"messages":[]}));
        observer.model_event(&ModelEventV1::Progress("one".into()));
        observer.model_event(&ModelEventV1::Progress("two".into()));
        observer.settle("failed");
        stream.flush();

        let delivered = subscriber
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(delivered.len(), stream.events().len());
        assert!(
            delivered
                .windows(2)
                .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
        );
        assert_eq!(
            delivered.last().map(|event| event.status.as_str()),
            Some("failed")
        );
    }
}
