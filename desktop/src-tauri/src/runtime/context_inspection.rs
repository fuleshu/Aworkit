//! Inspect and revise model context without rewriting the canonical conversation.

use super::semantic_events::CoreEventEnvelope;
use aworkit_capability_host::{
    ModelToolContextV1, ModelToolDefinitionV1, ModelToolExchangeV1, ModelToolRequestV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const MAX_CONTEXT_BYTES: usize = 768 * 1024;

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
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_CONTEXT_BYTES {
            return Err("Context exceeds the 768 KiB durable limit.".into());
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
        aworkit_capability_host::model_images::validate_image_attachments(&images)
            .map_err(|e| e.to_string())?;
        self.request()
            .validate()
            .map_err(|e| format!("Invalid model context: {e}"))
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
    events: &'a [CoreEventEnvelope],
) -> Option<&'a str> {
    let mut parent = event.payload.get("parentSpanId")?.as_str()?;
    for _ in 0..32 {
        let ancestor = events
            .iter()
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
    events: &[CoreEventEnvelope],
    node_id: &str,
) -> Result<ContextSelection, String> {
    let source = events
        .iter()
        .rev()
        .find(|event| {
            (event.kind == "context.edited" && event.payload["nodeId"] == node_id)
                || (event.kind == "span.started"
                    && event.payload["spanKind"] == "model_call"
                    && model_node(event, events) == Some(node_id))
        })
        .ok_or("No model context is available for this node.")?;
    let mut document = if source.kind == "context.edited" {
        serde_json::from_value(source.payload["document"].clone())
            .map_err(|e| format!("Invalid saved context: {e}"))?
    } else {
        ContextDocument::from_input(&source.payload["input"])?
    };
    let mut sequence = source.sequence;
    if source.kind != "context.edited" {
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

/// Apply an explicit revision to this node only, then add subsequent conversation
/// and this invocation's new exchanges. Historical tool calls are context, never executed.
pub(crate) fn apply_edit(
    events: &[CoreEventEnvelope],
    node_id: &str,
    current: &mut ModelToolRequestV1,
) -> Result<(), String> {
    let Some(edit) = events
        .iter()
        .rev()
        .find(|e| e.kind == "context.edited" && e.payload["nodeId"] == node_id)
    else {
        return Ok(());
    };
    let mut document: ContextDocument =
        serde_json::from_value(edit.payload["document"].clone()).map_err(|e| e.to_string())?;
    if document.tools != current.tools {
        return Err("Saved context tools differ from this node's frozen tools.".into());
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
    Ok(())
}

#[cfg(test)]
mod tests;
