use super::*;
use crate::runtime::semantic_events::{SemanticEventDraft, envelope};
use aworkit_capability_host::{ModelToolCallV1, ModelToolResultV1};

fn event(sequence: u64, kind: &str, payload: Value) -> CoreEventEnvelope {
    envelope(
        "chat.context",
        "main",
        sequence,
        SemanticEventDraft::new(kind, payload),
    )
}
fn document() -> ContextDocument {
    ContextDocument::from_input(&json!({"messages":[{"role":"system","content":"Original system"},{"role":"user","content":"Question"}]})).unwrap()
}
fn fixture() -> Vec<CoreEventEnvelope> {
    vec![
        event(
            1,
            "span.started",
            json!({"spanId":"node","spanKind":"graph_node","nodeId":"agent.1"}),
        ),
        event(
            2,
            "span.started",
            json!({"spanId":"loop","spanKind":"agent_loop","parentSpanId":"node"}),
        ),
        event(
            3,
            "span.started",
            json!({"spanId":"model","spanKind":"model_call","parentSpanId":"loop","input":document().input}),
        ),
        event(
            4,
            "span.completed",
            json!({"spanId":"model","output":[{"kind":"assistant_output","text":"The answer"}]}),
        ),
    ]
}

#[test]
fn context_projection_includes_final_answer_and_excludes_subagent() {
    let mut events = fixture();
    let selection = select_context(&events, "agent.1").unwrap();
    assert_eq!(selection.sequence, 4);
    assert_eq!(
        selection.document.context_messages[0].role.as_deref(),
        Some("assistant")
    );
    assert_eq!(selection.document.context_messages[0].content, "The answer");
    events.push(event(
        5,
        "span.started",
        json!({"spanId":"child","spanKind":"external_agent","parentSpanId":"loop"}),
    ));
    events.push(event(6, "span.started", json!({"spanId":"child-model","spanKind":"model_call","parentSpanId":"child","input":{"messages":[{"role":"user","content":"Child context"}]}})));
    assert_eq!(select_context(&events, "agent.1").unwrap().sequence, 4);
}

#[test]
fn edit_replaces_system_and_preserves_follow_up_order_without_rewriting_events() {
    let mut events = fixture();
    let mut edited = select_context(&events, "agent.1").unwrap().document;
    edited.input["messages"][0]["content"] = json!("Edited system");
    edited.context_messages[0].content = "Edited answer".into();
    events.push(event(
        5,
        "context.edited",
        json!({"nodeId":"agent.1","document":edited}),
    ));
    events.push(event(6, "message.user", json!({"body":"Follow up"})));
    let original = events.clone();
    let mut current = document().request();
    let edit = saved_edit(&events, "agent.1").unwrap().unwrap();
    assert_eq!(
        apply_edit(&events, &edit, &mut current).unwrap(),
        ContextAdmissionV1::Admitted
    );
    assert_eq!(current.input["messages"][0]["content"], "Edited system");
    assert_eq!(
        current
            .context_messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Edited answer", "Follow up"]
    );
    assert_eq!(events, original);
    // Another node has no saved revision and is never touched by this one.
    assert!(saved_edit(&events, "agent.2").unwrap().is_none());
    let mut unrelated = document().request();
    let unrelated_before = unrelated.clone();
    assert_eq!(
        apply_edit(&events, &edit, &mut unrelated).unwrap(),
        ContextAdmissionV1::Admitted
    );
    assert_ne!(unrelated, unrelated_before);
    // Rebuilding another provider turn does not duplicate the edited prefix.
    let mut next = document().request();
    apply_edit(&events, &edit, &mut next).unwrap();
    assert_eq!(current, next);
}

/// A revision records the interface of the pass that saved it. The acting pass's
/// frozen selection is the only set a provider may be offered, so the current
/// definitions are adopted: a refreshed provider alias, description or schema
/// never invalidates a Chat's committed history, and the recorded call is
/// re-pointed at the name this pass offers for the same capability.
#[test]
fn saved_edit_adopts_the_current_tool_interface_and_repoints_recorded_calls() {
    let mut events = fixture();
    let mut edited = select_context(&events, "agent.1").unwrap().document;
    edited.exchanges.push(ModelToolExchangeV1 {
        assistant_content: vec![ModelAssistantContentV1::ToolCall {
            call: ModelToolCallV1 {
                call_id: "call.1".into(),
                provider_call_id: Some("call.1".into()),
                capability_id: "tool.files.read".into(),
                name: "read_legacy".into(),
                arguments: json!({"path":"src/file.txt"}),
                provider_context: None,
            },
        }],
        results: vec![ModelToolResultV1 {
            call_id: "call.1".into(),
            content: json!("file"),
            is_error: false,
            images: Vec::new(),
        }],
    });
    events.push(event(
        5,
        "context.edited",
        json!({"nodeId":"agent.1","document":edited}),
    ));
    let mut current = document_request_with_tools(json!({
        "capabilityId":"tool.files.read","name":"read","description":"Read a file.",
        "inputSchema":{"type":"object"}
    }));
    let edit = saved_edit(&events, "agent.1").unwrap().unwrap();
    assert_eq!(
        apply_edit(&events, &edit, &mut current).unwrap(),
        ContextAdmissionV1::Admitted
    );
    assert_eq!(current.tools[0].name, "read");
    match &current.exchanges[0].assistant_content[0] {
        ModelAssistantContentV1::ToolCall { call } => assert_eq!(call.name, "read"),
        other => panic!("expected a recorded tool call, got {other:?}"),
    }
    // The committed revision itself is untouched evidence.
    assert_eq!(
        serde_json::to_value(&edited.exchanges).unwrap(),
        events
            .iter()
            .find(|e| e.kind == "context.edited")
            .map(|e| e.payload["document"]["exchanges"].clone())
            .unwrap()
    );
}

/// A revision that calls a capability this pass does not select cannot be
/// projected at all. It is declined with the capability named, and the caller's
/// own request stays exactly as it was.
#[test]
fn saved_edit_declines_recorded_calls_this_pass_cannot_offer() {
    let mut events = fixture();
    let mut edited = select_context(&events, "agent.1").unwrap().document;
    edited.exchanges.push(ModelToolExchangeV1 {
        assistant_content: vec![ModelAssistantContentV1::ToolCall {
            call: ModelToolCallV1 {
                call_id: "call.1".into(),
                provider_call_id: Some("call.1".into()),
                capability_id: "tool.shell.host".into(),
                name: "shell".into(),
                arguments: json!({"command":"dir"}),
                provider_context: None,
            },
        }],
        results: vec![ModelToolResultV1 {
            call_id: "call.1".into(),
            content: json!("listing"),
            is_error: false,
            images: Vec::new(),
        }],
    });
    events.push(event(
        5,
        "context.edited",
        json!({"nodeId":"agent.1","document":edited}),
    ));
    let mut current = document_request_with_tools(json!({
        "capabilityId":"tool.files.read","name":"read","description":"Read a file.",
        "inputSchema":{"type":"object"}
    }));
    let before = current.clone();
    let edit = saved_edit(&events, "agent.1").unwrap().unwrap();
    assert_eq!(
        apply_edit(&events, &edit, &mut current).unwrap(),
        ContextAdmissionV1::Unavailable(vec!["tool.shell.host".into()])
    );
    assert_eq!(current, before);
}

/// Builds a provider request whose only tool is `tool`, in the shape the tests
/// above need; the input matches the fixture document.
fn document_request_with_tools(tool: Value) -> ModelToolRequestV1 {
    let mut request = document().request();
    request.tools = vec![serde_json::from_value(tool).unwrap()];
    request
}

#[test]
fn context_rejects_bad_roles_broken_tool_pairs_and_materialized_image_data() {
    let mut doc = document();
    doc.validate().unwrap();
    doc.input["messages"][0]["role"] = json!("tool");
    assert!(doc.validate().is_err());
    doc = document();
    doc.context_messages.push(ModelToolContextV1 {
        after_exchanges: 1,
        content: "Out of order".into(),
        ..Default::default()
    });
    assert!(doc.validate().is_err());
    doc = document();
    doc.input["messages"][1]["images"] = json!([{"data":"abc"}]);
    assert!(doc.validate().is_err());
    doc = document();
    doc.exchanges.push(ModelToolExchangeV1 {
        assistant_content: vec![],
        results: vec![],
    });
    assert!(doc.validate().is_err());
}

/// A long Chat can hold any number of images and any total image size. The
/// durable checkpoint never applies an image budget: the provider's image
/// capacity is the only limit, and a provider rejection of it is reported to
/// the model rather than ending the Agent node. Applying such a budget in the
/// durable checkpoint ended Agent nodes with "tool authority rejected the
/// provider request". How many images one dispatch attaches is bounded later,
/// at materialization, where exceeding it degrades to a reference.
#[test]
fn accumulated_image_context_is_never_bounded_by_a_durable_checkpoint() {
    let mut doc = document();
    doc.context_messages.push(ModelToolContextV1 {
        after_exchanges: 0,
        content: "Image evidence".into(),
        images: (0..25)
            .map(|index| {
                json!({
                    "id": format!("{:064x}", index + 1),
                    "name": format!("image-{index}.png"),
                    "mimeType": "image/png",
                    "byteLength": 4 * 1024 * 1024,
                })
            })
            .collect(),
        ..Default::default()
    });
    doc.validate()
        .expect("image count and total image bytes are never a durable limit");
    // A single malformed image is still rejected.
    doc.context_messages[0].images[0]["id"] = json!("../private");
    assert!(doc.validate().is_err());
}
