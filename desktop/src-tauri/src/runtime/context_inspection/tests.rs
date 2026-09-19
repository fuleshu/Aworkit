use super::*;
use crate::runtime::semantic_events::{SemanticEventDraft, envelope};

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
    apply_edit(&events, "agent.1", &mut current).unwrap();
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
    let mut unrelated = document().request();
    apply_edit(&events, "agent.2", &mut unrelated).unwrap();
    assert_eq!(unrelated.input["messages"][0]["content"], "Original system");
    // Rebuilding another provider turn does not duplicate the edited prefix.
    let mut next = document().request();
    apply_edit(&events, "agent.1", &mut next).unwrap();
    assert_eq!(current, next);
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

/// A long Chat can hold any number of images and any total image size. There is
/// no aggregate image budget in Aworkit: the provider's image capacity is the
/// only limit, and a provider rejection of it is reported to the model rather
/// than ending the Agent node. Applying such a budget in the durable checkpoint
/// ended Agent nodes with "tool authority rejected the provider request".
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
