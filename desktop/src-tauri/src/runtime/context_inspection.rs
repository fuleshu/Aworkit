//! Inspect and revise model context without rewriting the canonical conversation.

use super::semantic_events::CoreEventEnvelope;
use aworkit_capability_host::{
    ModelAssistantContentV1, ModelToolContextV1, ModelToolDefinitionV1, ModelToolExchangeV1,
    ModelToolRequestV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Durable context documents may hold a full large-model context. The byte
/// trigger in compaction is derived from the token threshold, so this bound must
/// stay comfortably above it rather than firing before the token pressure does.
pub(crate) const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;

/// Counts serialized bytes without retaining them.
#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl std::io::Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Provider-neutral prompt, before image bytes and credentials are materialized.
/// Parameters and capability authority belong to the frozen workflow, not this editor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContextDocument {
    pub input: Value,
    pub tools: Vec<ModelToolDefinitionV1>,
    pub exchanges: Vec<ModelToolExchangeV1>,
    #[serde(default)]
    pub context_messages: Vec<ModelToolContextV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_notice: Option<String>,
}

impl ContextDocument {
    pub(crate) fn from_request(request: &ModelToolRequestV1) -> Self {
        Self {
            input: request.input.clone(),
            tools: request.tools.clone(),
            exchanges: request.exchanges.clone(),
            context_messages: request.context_messages.clone(),
            retry_notice: request.retry_notice.clone(),
        }
    }
    pub(crate) fn from_input(input: &Value) -> Result<Self, String> {
        let tool_request = input.get("input").is_some() && input.get("tools").is_some();
        serde_json::from_value(json!({
            "input": if tool_request { &input["input"] } else { input },
            "tools": input.get("tools").cloned().unwrap_or(json!([])),
            "exchanges": input.get("exchanges").cloned().unwrap_or(json!([])),
            "contextMessages": input.get("contextMessages").cloned().unwrap_or(json!([])),
            "retryNotice": input.get("retryNotice"),
        }))
        .map_err(|e| format!("Model context is unavailable: {e}"))
    }

    pub(crate) fn request(&self) -> ModelToolRequestV1 {
        ModelToolRequestV1 {
            input: self.input.clone(),
            tools: self.tools.clone(),
            exchanges: self.exchanges.clone(),
            context_messages: self.context_messages.clone(),
            retry_notice: self.retry_notice.clone(),
            parameters: BTreeMap::new(),
        }
    }

    pub(crate) fn append_message(&mut self, role: &str, content: String, images: Vec<Value>) {
        if content.is_empty() && images.is_empty() {
            return;
        }
        self.context_messages.push(ModelToolContextV1 {
            after_exchanges: self.exchanges.len(),
            content,
            role: Some(role.into()),
            images,
            ..Default::default()
        });
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        // Measure the durable shape without materializing a second multi-megabyte
        // JSON buffer; this runs for every model turn.
        let mut counter = ByteCounter::default();
        serde_json::to_writer(&mut counter, self).map_err(|e| e.to_string())?;
        if counter.bytes > MAX_CONTEXT_BYTES {
            return Err("Context exceeds the 4 MiB durable limit.".into());
        }
        // Durable contexts permit only the same compact message shape used by Chat.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            messages: Vec<super::pipeline::WorkflowMessageV1>,
        }
        let input: Input = serde_json::from_value(self.input.clone()).map_err(|e| {
            format!("input.messages must contain role, content and optional images: {e}")
        })?;
        let mut images = input
            .messages
            .iter()
            .flat_map(|m| m.images.clone())
            .collect::<Vec<_>>();
        for context in &self.context_messages {
            let refs: Vec<aworkit_capability_host::model_images::ImageAttachmentV1> =
                serde_json::from_value(json!(context.images))
                    .map_err(|e| format!("Invalid context images: {e}"))?;
            images.extend(refs);
        }
        images.extend(self.exchanges.iter().flat_map(|e| &e.results).flat_map(|r| r.images.clone()));
        // A durable context is never bounded by an image count or a total image
        // size: every image a Chat holds stays in it, and only each image itself
        // is checked here.
        for image in &images {
            image.validate().map_err(|e| e.to_string())?;
        }
        self.request()
            .validate()
            .map_err(|e| format!("Invalid model context: {e}"))
    }
}

/// Admission of a durable recorded selection under this pass's frozen tools.
///
/// Tool definitions are interface, not history: this build resolves them for
/// every pass and a Chat keeps only the authority that selects capabilities. A
/// checkpoint or a saved edit therefore records the selection of the pass that
/// wrote it, while the acting node's frozen selection is the only set a provider
/// may be offered now. Adopting the current interface is the normal case; a
/// recorded call whose capability this pass no longer selects cannot be
/// represented in a provider request at all.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ContextAdmissionV1 {
    /// The recorded history is representable under the current selection.
    Admitted,
    /// Capabilities the recorded history calls that this pass does not select.
    /// Durable evidence stays untouched and the caller keeps its own context.
    Unavailable(Vec<String>),
}

/// Re-points recorded calls at the provider name this pass offers for their
/// capability. `capability_id` is the immutable identity that settles a call, so
/// a refreshed description, schema or provider alias never invalidates committed
/// history, and no recorded projection is ever rewritten in the event store.
pub(crate) fn admit_current_tools(
    exchanges: &mut [ModelToolExchangeV1],
    tools: &[ModelToolDefinitionV1],
) -> ContextAdmissionV1 {
    let mut unavailable: Vec<String> = Vec::new();
    for exchange in exchanges {
        for content in exchange.assistant_content.iter_mut() {
            if let ModelAssistantContentV1::ToolCall { call } = content {
                match tools
                    .iter()
                    .find(|tool| tool.capability_id == call.capability_id)
                {
                    Some(definition) => call.name.clone_from(&definition.name),
                    None if !unavailable.contains(&call.capability_id) => {
                        unavailable.push(call.capability_id.clone());
                    }
                    None => {}
                }
            }
        }
    }
    if unavailable.is_empty() {
        ContextAdmissionV1::Admitted
    } else {
        ContextAdmissionV1::Unavailable(unavailable)
    }
}

#[derive(Clone)]
pub(crate) struct ContextSelection {
    pub node_id: String,
    pub document: ContextDocument,
    pub sequence: u64,
}

/// Match a model request to its graph node; child-agent prompts have separate ownership.
pub(crate) fn model_node<'a>(
    event: &'a CoreEventEnvelope,
    events: &'a [impl std::borrow::Borrow<CoreEventEnvelope>],
) -> Option<&'a str> {
    let mut parent = event.payload.get("parentSpanId")?.as_str()?;
    for _ in 0..32 {
        let ancestor = events
            .iter()
            .map(std::borrow::Borrow::borrow)
            .find(|e| e.kind == "span.started" && e.span_id.as_deref() == Some(parent))?;
        if ancestor.payload["spanKind"] == "external_agent" {
            return None;
        }
        if ancestor.payload["spanKind"] == "graph_node" {
            return ancestor.payload["nodeId"].as_str();
        }
        parent = ancestor.payload.get("parentSpanId")?.as_str()?;
    }
    None
}

/// The latest prompt for a node plus its final answer, or a subsequently saved edit.
pub(crate) fn select_context(
    events: &[impl std::borrow::Borrow<CoreEventEnvelope>],
    node_id: &str,
) -> Result<ContextSelection, String> {
    let events: Vec<&CoreEventEnvelope> = events.iter().map(std::borrow::Borrow::borrow).collect();
    let source = events
        .iter()
        .rev()
        .find(|event| {
            (matches!(event.kind.as_str(), "context.edited" | "context.checkpoint")
                && event.payload["nodeId"] == node_id
                && event.payload["child"].is_null())
                || (event.kind == "span.started"
                    && event.payload["spanKind"] == "model_call"
                    && model_node(event, &events) == Some(node_id))
        })
        .ok_or("No model context is available for this node.")?;
    let mut document = if matches!(
        source.kind.as_str(),
        "context.edited" | "context.checkpoint"
    ) {
        serde_json::from_value(if source.kind == "context.checkpoint" {
            source.payload["snapshot"]["document"].clone()
        } else {
            source.payload["document"].clone()
        })
        .map_err(|e| format!("Invalid saved context: {e}"))?
    } else {
        ContextDocument::from_input(&source.payload["input"])?
    };
    let mut sequence = source.sequence;
    if source.kind == "span.started" {
        if let Some(completed) = events
            .iter()
            .find(|e| e.kind == "span.completed" && e.span_id == source.span_id)
        {
            if let Some(output) = completed.payload["output"].as_array() {
                if !output.iter().any(|e| e["kind"] == "tool_call") {
                    let text = output
                        .iter()
                        .filter(|e| e["kind"] == "assistant_output")
                        .filter_map(|e| e["text"].as_str())
                        .collect::<String>();
                    document.append_message("assistant", text, Vec::new());
                    sequence = completed.sequence;
                }
            }
        }
    }
    Ok(ContextSelection {
        node_id: node_id.into(),
        document,
        sequence,
    })
}

/// The most recent explicit revision saved for one node, with the sequence that
/// owns it. Decoding is separate from applying so a caller can decide admission
/// before it mutates anything.
#[derive(Clone, Debug)]
pub(crate) struct SavedContextEditV1 {
    pub sequence: u64,
    pub document: ContextDocument,
}

/// Decode the latest saved revision for this node, if any.
pub(crate) fn saved_edit(
    events: &[impl std::borrow::Borrow<CoreEventEnvelope>],
    node_id: &str,
) -> Result<Option<SavedContextEditV1>, String> {
    events
        .iter()
        .map(std::borrow::Borrow::borrow)
        .rev()
        .find(|event| event.kind == "context.edited" && event.payload["nodeId"] == node_id)
        .map(|event| {
            serde_json::from_value(event.payload["document"].clone())
                .map(|document| SavedContextEditV1 {
                    sequence: event.sequence,
                    document,
                })
                .map_err(|error| error.to_string())
        })
        .transpose()
}

/// Admit a saved revision's recorded calls under this pass's frozen tools, in
/// place, so a caller can decide admission before it mutates its own request.
pub(crate) fn admit_edit(
    edit: &mut SavedContextEditV1,
    tools: &[ModelToolDefinitionV1],
) -> ContextAdmissionV1 {
    admit_current_tools(&mut edit.document.exchanges, tools)
}

/// Apply an explicit revision to this node only, then add subsequent conversation
/// and this invocation's new exchanges. Historical tool calls are context, never executed.
///
/// The revision records the interface of the pass that saved it, while the acting
/// node's frozen selection is the only set a provider may be offered. The current
/// definitions are therefore adopted; a revision whose recorded calls name a
/// capability this pass no longer selects cannot be represented at all and is
/// declined, leaving the caller's request untouched.
pub(crate) fn apply_edit(
    events: &[impl std::borrow::Borrow<CoreEventEnvelope>],
    edit: &SavedContextEditV1,
    current: &mut ModelToolRequestV1,
) -> Result<ContextAdmissionV1, String> {
    let events: Vec<&CoreEventEnvelope> = events.iter().map(std::borrow::Borrow::borrow).collect();
    let mut document = edit.document.clone();
    if let ContextAdmissionV1::Unavailable(capabilities) =
        admit_current_tools(&mut document.exchanges, &current.tools)
    {
        return Ok(ContextAdmissionV1::Unavailable(capabilities));
    }
    // Messages committed after the edit are added once, at the end of its prefix.
    for event in events.iter().filter(|e| e.sequence > edit.sequence) {
        let role = match event.kind.as_str() {
            "message.user" => "user",
            "message.assistant" => "assistant",
            _ => continue,
        };
        document.append_message(
            role,
            event.payload["body"].as_str().unwrap_or_default().into(),
            event.payload["attachments"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }
    let offset = document.exchanges.len();
    // Preserve transient recovery/advisory context before continuing the edited prefix.
    if let Some(notice) = document.retry_notice.take() {
        document.append_message("user", notice, Vec::new());
    }
    for mut context in current.context_messages.clone() {
        context.after_exchanges += offset;
        document.context_messages.push(context);
    }
    document.exchanges.extend(current.exchanges.clone());
    current.input = document.input;
    current.context_messages = document.context_messages;
    current.exchanges = document.exchanges;
    // The pass keeps its own tool definitions: the revision supplied history and
    // recorded calls were already re-pointed at this pass's provider names.
    Ok(ContextAdmissionV1::Admitted)
}

#[cfg(test)]
mod tests;
