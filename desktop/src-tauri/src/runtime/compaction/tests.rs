use super::*;
use aworkit_capability_host::{ModelToolCallV1, ModelToolDefinitionV1, ModelToolResultV1};

fn request() -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        input: json!({"messages":[{"role":"system","content":"system"},{"role":"user","content":"question"}]}),
        parameters: Default::default(),
        tools: vec![ModelToolDefinitionV1 {
            capability_id: "read".into(),
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
        }],
        exchanges: Vec::new(),
        context_messages: Vec::new(),
        retry_notice: None,
    }
}
fn exchange(text: &str) -> ModelToolExchangeV1 {
    ModelToolExchangeV1 {
        assistant_content: vec![ModelAssistantContentV1::ToolCall {
            call: ModelToolCallV1 {
                call_id: "c1".into(),
                provider_call_id: Some("c1".into()),
                capability_id: "read".into(),
                name: "read".into(),
                arguments: json!({}),
                provider_context: None,
            },
        }],
        results: vec![ModelToolResultV1 {
            images: Vec::new(),
            call_id: "c1".into(),
            content: json!(text),
            is_error: false,
        }],
    }
}
#[test]
fn policy_defaults_exclusive_retention_and_capacity_validation() {
    let p = Policy::default();
    p.validate(Some(128000)).unwrap();
    assert_eq!(p.threshold(128000), 102400);
    assert_eq!(p.retention(128000), 20480);
    assert_eq!(p.max_tokens, 8192);
    for value in [
        json!({"thresholdRatio":0}),
        json!({"retainRatio":0.9}),
        json!({"retainTokens":0,"retainRatio":0.1}),
        json!({"headChars":81920}),
        json!({"maxTokens":0}),
    ] {
        assert!(serde_json::from_value::<Policy>(value)
            .unwrap()
            .validate(Some(128000))
            .is_err());
    }
    assert!(serde_json::from_value::<Policy>(json!({"typo":1})).is_err());
    let p: Policy = serde_json::from_value(json!({"retainTokens":0})).unwrap();
    p.validate(None).unwrap();
    assert_eq!(p.retention(128000), 0);
}
#[test]
fn surface_round_trip_keeps_input_injections_signatures_and_parallel_pairs() {
    let mut r = request();
    r.exchanges.push(exchange("result"));
    r.context_messages = vec![
        ModelToolContextV1 {
            after_input_messages: Some(1),
            instruction_event_id: Some("authentic".into()),
            content: "instructions".into(),
            ..Default::default()
        },
        ModelToolContextV1 {
            after_exchanges: 1,
            role: Some("assistant".into()),
            content: "answer".into(),
            ..Default::default()
        },
        ModelToolContextV1 {
            after_exchanges: 1,
            content: "next question".into(),
            ..Default::default()
        },
    ];
    let before = units(&r).unwrap();
    replace_units(&mut r, &before).unwrap();
    r.validate().unwrap();
    let after = units(&r).unwrap();
    assert_eq!(
        before.iter().map(Unit::tokens).collect::<Vec<_>>(),
        after.iter().map(Unit::tokens).collect::<Vec<_>>()
    );
    assert_eq!(
        r.context_messages[0].instruction_event_id.as_deref(),
        Some("authentic")
    );
    assert_eq!(r.exchanges[0], exchange("result"));
}
#[test]
fn selection_keeps_the_last_unit_even_for_zero_and_never_splits_tool_pairs() {
    let mut r = request();
    r.exchanges = vec![exchange(&"x".repeat(6000)), exchange("last")];
    let s = units(&r).unwrap();
    assert_eq!(select_prefix(&s, 0), Some(2));
    assert_eq!(select_prefix(&s, 100_000), None);
    let cut = select_prefix(&s, 50).unwrap();
    assert!(matches!(s[cut], Unit::Exchange(_)));
}

#[test]
fn a_replacement_pins_real_user_turns_in_original_order() {
    let mut r = request();
    r.input["messages"] = json!([
        {"role":"system","content":"system"},
        {"role":"user","content":"first direction"}
    ]);
    r.exchanges = vec![exchange(&"x".repeat(4000)), exchange("mid")];
    r.context_messages.push(ModelToolContextV1 {
        after_exchanges: 1,
        content: "second direction".into(),
        ..Default::default()
    });
    let s = units(&r).unwrap();
    // user(first), exchange, user(second), exchange
    assert_eq!(s.len(), 4);
    let pinned = pinned_user_units(&s, 3);
    assert_eq!(pinned.len(), 2, "both user turns are pinned");
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first direction"));
    assert!(matches!(&pinned[1], Unit::Message(m) if m.content == "second direction"));
}

#[test]
fn an_oversized_late_user_turn_cannot_defeat_the_reduction() {
    let mut r = request();
    r.input["messages"] = json!([
        {"role":"system","content":"system"},
        {"role":"user","content":"first"}
    ]);
    r.exchanges = vec![exchange("small")];
    r.context_messages.push(ModelToolContextV1 {
        after_exchanges: 1,
        content: "y".repeat(40_000),
        ..Default::default()
    });
    let s = units(&r).unwrap();
    let pinned = pinned_user_units(&s, 3);
    assert_eq!(
        pinned.len(),
        1,
        "pinning may cost at most half of what is freed"
    );
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first"));
}

#[test]
fn a_prior_checkpoint_and_instructions_are_not_pinned_as_user_turns() {
    let mut r = request();
    r.input["messages"] = json!([
        {"role":"system","content":"system"},
        {"role":"user","content":"first"}
    ]);
    r.exchanges = vec![exchange("result")];
    r.context_messages = vec![
        ModelToolContextV1 {
            after_input_messages: Some(1),
            instruction_event_id: Some("authentic".into()),
            content: "instructions".into(),
            ..Default::default()
        },
        ModelToolContextV1 {
            after_exchanges: 0,
            content: frame_summary("an older checkpoint"),
            ..Default::default()
        },
    ];
    let s = units(&r).unwrap();
    let pinned = pinned_user_units(&s, s.len());
    assert_eq!(pinned.len(), 1, "{pinned:?}");
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first"));
}
#[test]
fn pruning_preserves_unicode_rich_blocks_errors_ids_and_is_idempotent() {
    let policy = Policy::default();
    let mut r = request();
    r.exchanges.push(exchange(&"😀".repeat(100_000)));
    r.exchanges[0].results[0].is_error = true;
    let changes = prune(&mut r, &policy);
    let removed = 100_000 - policy.head_chars - policy.tail_chars;
    assert_eq!(changes[0].chars_before, 100_000);
    assert_eq!(
        changes[0].chars_after,
        policy.head_chars + policy.tail_chars + prune_marker(removed).chars().count()
    );
    assert_eq!(r.exchanges[0].results[0].call_id, "c1");
    assert!(r.exchanges[0].results[0].is_error);
    let pruned_text = r.exchanges[0].results[0].content.as_str().unwrap();
    assert!(pruned_text.contains(&format!("{removed} characters removed")));
    assert!(pruned_text.contains("incomplete"));
    assert!(prune(&mut r, &policy).is_empty());
    r.exchanges[0].results[0].content = json!({"content":[{"type":"text","text":"a".repeat(40_000)},{"type":"image","data":"opaque"},{"type":"text","text":"b".repeat(40_000)}]});
    prune(&mut r, &policy);
    assert_eq!(
        r.exchanges[0].results[0].content["content"][1],
        json!({"type":"image","data":"opaque"})
    );
    assert!(r.exchanges[0].results[0].content["content"][2]["text"]
        .as_str()
        .unwrap()
        .ends_with(&"b".repeat(policy.tail_chars)));
    r.exchanges[0].results[0].content =
        json!([{ "type":"text","text":"x".repeat(80_000) },{"type":"image","data":"opaque"}]);
    prune(&mut r, &policy);
    assert_eq!(
        r.exchanges[0].results[0].content[1],
        json!({"type":"image","data":"opaque"})
    );
    assert!(prune(&mut r, &policy).is_empty());
}
#[test]
fn usage_anchor_tracks_reductions_and_is_invalidated_by_a_header_change() {
    let mut r = request();
    r.exchanges.push(exchange(&"x".repeat(200_000)));
    let estimated = estimate(&r).unwrap();
    let anchor = Anchor {
        header_hash: header_hash(&r),
        estimated,
        reported: estimated + 1000,
    };
    assert_eq!(pressure(&r, Some(&anchor)).unwrap(), estimated + 1000);
    prune(&mut r, &Policy::default());
    assert_eq!(
        pressure(&r, Some(&anchor)).unwrap(),
        estimate(&r).unwrap() + 1000
    );
    r.input["messages"][0]["content"] = json!("different system");
    assert_eq!(pressure(&r, Some(&anchor)).unwrap(), estimate(&r).unwrap());
    assert_eq!(text_tokens("😀😀😀"), 2);
}

#[test]
fn image_dispatch_follows_the_frozen_model_capability() {
    assert_eq!(
        image_dispatch(&json!({"imageInput": true})).unwrap(),
        ImageDispatchV1::Attach,
        "a model with image input receives the bytes"
    );
    assert_eq!(
        image_dispatch(&json!({"imageInput": false})).unwrap(),
        ImageDispatchV1::Reference,
        "a model without image input receives references"
    );
    // A context frozen before the capability existed, or without an answer,
    // keeps the previous behaviour: attaching bytes it may not need beats
    // silently dropping image input from an existing Chat.
    assert_eq!(image_dispatch(&json!({})).unwrap(), ImageDispatchV1::Attach);
    assert!(image_dispatch(&json!({"imageInput": "yes"})).is_err());
}
