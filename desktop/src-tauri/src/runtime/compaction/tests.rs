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
        json!({"headChars":8192}),
        json!({"maxTokens":0}),
    ] {
        assert!(
            serde_json::from_value::<Policy>(value)
                .unwrap()
                .validate(Some(128000))
                .is_err()
        );
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
fn pruning_preserves_unicode_rich_blocks_errors_ids_and_is_idempotent() {
    let mut r = request();
    r.exchanges.push(exchange(&"😀".repeat(10000)));
    r.exchanges[0].results[0].is_error = true;
    let changes = prune(&mut r, &Policy::default());
    assert_eq!(changes[0].chars_before, 10000);
    assert_eq!(changes[0].chars_after, 5120 + PRUNE_MARKER.chars().count());
    assert_eq!(r.exchanges[0].results[0].call_id, "c1");
    assert!(r.exchanges[0].results[0].is_error);
    assert!(prune(&mut r, &Policy::default()).is_empty());
    r.exchanges[0].results[0].content = json!({"content":[{"type":"text","text":"a".repeat(6000)},{"type":"image","data":"opaque"},{"type":"text","text":"b".repeat(6000)}]});
    prune(&mut r, &Policy::default());
    assert_eq!(
        r.exchanges[0].results[0].content["content"][1],
        json!({"type":"image","data":"opaque"})
    );
    assert!(
        r.exchanges[0].results[0].content["content"][2]["text"]
            .as_str()
            .unwrap()
            .ends_with(&"b".repeat(1024))
    );
    r.exchanges[0].results[0].content =
        json!([{ "type":"text","text":"x".repeat(9000) },{"type":"image","data":"opaque"}]);
    prune(&mut r, &Policy::default());
    assert_eq!(
        r.exchanges[0].results[0].content[1],
        json!({"type":"image","data":"opaque"})
    );
    assert!(prune(&mut r, &Policy::default()).is_empty());
}
#[test]
fn usage_anchor_tracks_reductions_and_is_invalidated_by_a_header_change() {
    let mut r = request();
    r.exchanges.push(exchange(&"x".repeat(20000)));
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
