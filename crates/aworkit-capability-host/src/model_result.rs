//! Canonical completed-turn projection for raw provider event evidence.

use serde::Serialize;
use serde_json::Value;

use crate::{ModelAssistantContentV1, ModelEventV1, ModelToolCallV1, ModelToolEventV1};

/// A provider-neutral terminal event. Streaming text fragments are combined
/// into one entry per kind while calls and usage retain their typed structure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelResultEventV1 {
    AssistantOutput {
        text: String,
    },
    ToolCall {
        call: ModelToolCallV1,
    },
    ReasoningRaw {
        text: String,
    },
    ReasoningSummary {
        text: String,
    },
    Progress {
        text: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
}

/// One reusable interpretation of a completed provider turn.
///
/// Raw dispatch evidence remains untouched. Consumers use this projection for
/// assistant text, usage, ordered tool history, and compact terminal JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelTurnProjectionV1 {
    pub assistant_text: String,
    pub assistant_content: Vec<ModelAssistantContentV1>,
    pub calls: Vec<ModelToolCallV1>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub compacted_events: Vec<ModelResultEventV1>,
}

impl ModelTurnProjectionV1 {
    /// JSON stored on the terminal model-call span and shown by presentation
    /// layers. Serialization is infallible for the closed event contract.
    #[must_use]
    pub fn output_value(&self) -> Value {
        serde_json::to_value(&self.compacted_events).unwrap_or(Value::Null)
    }
}

/// Projects a completed text-only model stream without modifying its evidence.
#[must_use]
pub fn project_model_events(events: &[ModelEventV1]) -> ModelTurnProjectionV1 {
    let mut projection = ProjectionBuilder::default();
    for event in events {
        match event {
            ModelEventV1::AssistantOutput(text) => projection.text(TextKind::Assistant, text),
            ModelEventV1::ReasoningRaw(text) => projection.text(TextKind::ReasoningRaw, text),
            ModelEventV1::ReasoningSummary(text) => {
                projection.text(TextKind::ReasoningSummary, text);
            }
            ModelEventV1::Progress(text) => projection.text(TextKind::Progress, text),
            ModelEventV1::Usage {
                input_tokens,
                output_tokens,
            } => projection.usage(*input_tokens, *output_tokens),
        }
    }
    projection.finish()
}

/// Projects a completed tool-capable model stream without modifying its
/// evidence or losing the ordering required for the next provider request.
#[must_use]
pub fn project_model_tool_events(events: &[ModelToolEventV1]) -> ModelTurnProjectionV1 {
    let mut projection = ProjectionBuilder::default();
    for event in events {
        match event {
            ModelToolEventV1::AssistantOutput { text } => {
                projection.text(TextKind::Assistant, text);
            }
            ModelToolEventV1::ToolCall { call } => projection.tool_call(call),
            ModelToolEventV1::ReasoningRaw { text } => {
                projection.text(TextKind::ReasoningRaw, text);
            }
            ModelToolEventV1::ReasoningSummary { text } => {
                projection.text(TextKind::ReasoningSummary, text);
            }
            ModelToolEventV1::Progress { text } => projection.text(TextKind::Progress, text),
            ModelToolEventV1::Usage {
                input_tokens,
                output_tokens,
            } => projection.usage(*input_tokens, *output_tokens),
        }
    }
    projection.finish()
}

#[derive(Clone, Copy)]
enum TextKind {
    Assistant,
    ReasoningRaw,
    ReasoningSummary,
    Progress,
}

#[derive(Default)]
struct ProjectionBuilder {
    assistant_text: String,
    assistant_content: Vec<ModelAssistantContentV1>,
    calls: Vec<ModelToolCallV1>,
    input_tokens: u64,
    output_tokens: u64,
    compacted_events: Vec<ModelResultEventV1>,
    assistant_index: Option<usize>,
    reasoning_raw_index: Option<usize>,
    reasoning_summary_index: Option<usize>,
    progress_index: Option<usize>,
    usage_index: Option<usize>,
}

impl ProjectionBuilder {
    fn text(&mut self, kind: TextKind, text: &str) {
        if matches!(kind, TextKind::Assistant) {
            self.assistant_text.push_str(text);
            match self.assistant_content.last_mut() {
                Some(ModelAssistantContentV1::Text { text: accumulated }) => {
                    accumulated.push_str(text);
                }
                _ => self.assistant_content.push(ModelAssistantContentV1::Text {
                    text: text.to_owned(),
                }),
            }
        }

        let existing = match kind {
            TextKind::Assistant => self.assistant_index,
            TextKind::ReasoningRaw => self.reasoning_raw_index,
            TextKind::ReasoningSummary => self.reasoning_summary_index,
            TextKind::Progress => self.progress_index,
        };
        if let Some(index) = existing {
            append_text(&mut self.compacted_events[index], text);
            return;
        }

        let index = self.compacted_events.len();
        self.compacted_events.push(match kind {
            TextKind::Assistant => ModelResultEventV1::AssistantOutput {
                text: text.to_owned(),
            },
            TextKind::ReasoningRaw => ModelResultEventV1::ReasoningRaw {
                text: text.to_owned(),
            },
            TextKind::ReasoningSummary => ModelResultEventV1::ReasoningSummary {
                text: text.to_owned(),
            },
            TextKind::Progress => ModelResultEventV1::Progress {
                text: text.to_owned(),
            },
        });
        match kind {
            TextKind::Assistant => self.assistant_index = Some(index),
            TextKind::ReasoningRaw => self.reasoning_raw_index = Some(index),
            TextKind::ReasoningSummary => self.reasoning_summary_index = Some(index),
            TextKind::Progress => self.progress_index = Some(index),
        }
    }

    fn tool_call(&mut self, call: &ModelToolCallV1) {
        self.calls.push(call.clone());
        self.assistant_content
            .push(ModelAssistantContentV1::ToolCall { call: call.clone() });
        self.compacted_events
            .push(ModelResultEventV1::ToolCall { call: call.clone() });
    }

    fn usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        let event = ModelResultEventV1::Usage {
            input_tokens,
            output_tokens,
        };
        if let Some(index) = self.usage_index {
            self.compacted_events[index] = event;
        } else {
            self.usage_index = Some(self.compacted_events.len());
            self.compacted_events.push(event);
        }
    }

    fn finish(self) -> ModelTurnProjectionV1 {
        ModelTurnProjectionV1 {
            assistant_text: self.assistant_text,
            assistant_content: self.assistant_content,
            calls: self.calls,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            compacted_events: self.compacted_events,
        }
    }
}

fn append_text(event: &mut ModelResultEventV1, append: &str) {
    match event {
        ModelResultEventV1::AssistantOutput { text }
        | ModelResultEventV1::ReasoningRaw { text }
        | ModelResultEventV1::ReasoningSummary { text }
        | ModelResultEventV1::Progress { text } => text.push_str(append),
        ModelResultEventV1::ToolCall { .. } | ModelResultEventV1::Usage { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn plain_projection_compacts_every_fragment_of_the_same_kind() {
        let events = vec![
            ModelEventV1::ReasoningRaw("The".to_owned()),
            ModelEventV1::ReasoningRaw(" user just said \"".to_owned()),
            ModelEventV1::Progress("Preparing.".to_owned()),
            ModelEventV1::ReasoningRaw("hello\".".to_owned()),
            ModelEventV1::AssistantOutput("Hello ".to_owned()),
            ModelEventV1::AssistantOutput("there!".to_owned()),
            ModelEventV1::Usage {
                input_tokens: 8,
                output_tokens: 3,
            },
        ];

        let projection = project_model_events(&events);

        assert_eq!(projection.assistant_text, "Hello there!");
        assert_eq!((projection.input_tokens, projection.output_tokens), (8, 3));
        assert_eq!(events.len(), 7);
        assert_eq!(
            projection.output_value(),
            json!([
                {"kind":"reasoning_raw","text":"The user just said \"hello\"."},
                {"kind":"progress","text":"Preparing."},
                {"kind":"assistant_output","text":"Hello there!"},
                {"kind":"usage","input_tokens":8,"output_tokens":3}
            ])
        );
    }

    #[test]
    fn tool_projection_preserves_history_order_while_compacting_terminal_text() {
        let call = ModelToolCallV1 {
            call_id: "call.1".to_owned(),
            provider_call_id: Some("provider.1".to_owned()),
            capability_id: "tool.echo".to_owned(),
            name: "echo".to_owned(),
            arguments: json!({"value":"hi"}),
            provider_context: None,
        };
        let events = vec![
            ModelToolEventV1::AssistantOutput {
                text: "Before ".to_owned(),
            },
            ModelToolEventV1::AssistantOutput {
                text: "call".to_owned(),
            },
            ModelToolEventV1::ToolCall { call: call.clone() },
            ModelToolEventV1::AssistantOutput {
                text: " after".to_owned(),
            },
            ModelToolEventV1::Usage {
                input_tokens: 5,
                output_tokens: 2,
            },
        ];

        let projection = project_model_tool_events(&events);

        assert_eq!(projection.assistant_text, "Before call after");
        assert_eq!(projection.calls, vec![call.clone()]);
        assert_eq!(
            projection.assistant_content,
            vec![
                ModelAssistantContentV1::Text {
                    text: "Before call".to_owned(),
                },
                ModelAssistantContentV1::ToolCall { call },
                ModelAssistantContentV1::Text {
                    text: " after".to_owned(),
                },
            ]
        );
        assert_eq!(
            projection
                .compacted_events
                .iter()
                .filter(|event| matches!(event, ModelResultEventV1::AssistantOutput { .. }))
                .count(),
            1
        );
        assert_eq!(events.len(), 5);
    }
}
