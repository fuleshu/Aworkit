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
    // The absolute summary budget is gone: a frozen policy that still carries
    // maxTokens deserializes and ignores the value.
    let legacy: Policy = serde_json::from_value(json!({"maxTokens": 4096})).unwrap();
    legacy.validate(Some(128000)).unwrap();
    for value in [
        json!({"thresholdRatio":0}),
        json!({"retainRatio":0.9}),
        json!({"retainTokens":0,"retainRatio":0.1}),
        json!({"headChars":81920}),
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
    let pinned = pinned_user_units(&s, 3, u64::MAX);
    assert_eq!(pinned.len(), 2, "both user turns are pinned");
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first direction"));
    assert!(matches!(&pinned[1], Unit::Message(m) if m.content == "second direction"));
}

#[test]
fn the_user_budget_bounds_later_turns_but_not_the_first() {
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
    // A later turn that does not fit the budget is left to the summary, and the
    // first turn is kept regardless.
    let pinned = pinned_user_units(&s, 3, 20);
    assert_eq!(pinned.len(), 1);
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first"));
    let all = pinned_user_units(&s, 3, u64::MAX);
    assert_eq!(all.len(), 2);
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
    let pinned = pinned_user_units(&s, s.len(), u64::MAX);
    assert_eq!(pinned.len(), 1, "{pinned:?}");
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first"));
}

#[test]
fn generated_state_is_recognised_and_never_pinned_as_user_direction() {
    // Each label the runtime emits is recognised, and a compaction carries none
    // of them forward: they are re-derived from records instead.
    for label in [GOAL_STATE_LABEL, TASK_STATE_LABEL, FILES_STATE_LABEL] {
        assert!(is_generated_state(&format!("{label}0; durable state)")));
    }
    assert!(!is_generated_state("Current tasks I should do"));
    let mut r = request();
    r.input["messages"] = json!([
        {"role":"system","content":"system"},
        {"role":"user","content":"first"}
    ]);
    r.exchanges = vec![exchange("result")];
    r.context_messages = vec![
        ModelToolContextV1 {
            after_exchanges: 0,
            content: format!(
                "{GOAL_STATE_LABEL}active; durable state for this Chat, not a new instruction):\nold goal"
            ),
            ..Default::default()
        },
        ModelToolContextV1 {
            after_exchanges: 1,
            content: format!(
                "{FILES_STATE_LABEL}1 total; durable state for this Run, not a new instruction):\n/tmp/a"
            ),
            ..Default::default()
        },
    ];
    let s = units(&r).unwrap();
    let pinned = pinned_user_units(&s, s.len(), u64::MAX);
    assert_eq!(pinned.len(), 1, "{pinned:?}");
    assert!(matches!(&pinned[0], Unit::Message(m) if m.content == "first"));
}
#[test]
fn pruning_preserves_unicode_rich_blocks_ids_and_is_idempotent() {
    let policy = Policy::default();
    let mut r = request();
    r.exchanges.push(exchange(&"😀".repeat(100_000)));
    let changes = prune(&mut r, &policy, 0);
    let removed = 100_000 - policy.head_chars - policy.tail_chars;
    assert_eq!(changes[0].chars_before, 100_000);
    assert_eq!(
        changes[0].chars_after,
        policy.head_chars + policy.tail_chars + prune_marker(removed).chars().count()
    );
    assert!(
        changes[0].chars_after < changes[0].chars_before,
        "pruning never expands a result"
    );
    assert_eq!(r.exchanges[0].results[0].call_id, "c1");
    let pruned_text = r.exchanges[0].results[0].content.as_str().unwrap();
    assert!(pruned_text.contains(&format!("{removed} characters removed")));
    assert!(pruned_text.contains("incomplete"));
    assert!(prune(&mut r, &policy, 0).is_empty());
    r.exchanges[0].results[0].content = json!({"content":[{"type":"text","text":"a".repeat(40_000)},{"type":"image","data":"opaque"},{"type":"text","text":"b".repeat(40_000)}]});
    prune(&mut r, &policy, 0);
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
    prune(&mut r, &policy, 0);
    assert_eq!(
        r.exchanges[0].results[0].content[1],
        json!({"type":"image","data":"opaque"})
    );
    assert!(prune(&mut r, &policy, 0).is_empty());
}

#[test]
fn pruning_reduces_only_old_successful_results_and_records_the_estimate() {
    let policy = Policy::default();
    let mut r = request();
    r.exchanges.push(exchange(&"oldest".repeat(8_000)));
    r.exchanges.push(exchange(&"middle".repeat(8_000)));
    r.exchanges.push(exchange(&"newest".repeat(8_000)));
    r.exchanges[0].results[0].is_error = true;
    let before = estimate(&r).unwrap();
    let changes = prune(&mut r, &policy, 1);
    assert_eq!(changes.len(), 1, "one candidate: {changes:?}");
    assert_eq!(changes[0].exchange, 1);
    assert_eq!(
        r.exchanges[0].results[0].content.as_str().unwrap().len(),
        "oldest".len() * 8_000,
        "a failed result stays verbatim"
    );
    assert_eq!(
        r.exchanges[2].results[0].content.as_str().unwrap().len(),
        "newest".len() * 8_000,
        "the newest exchange is not a candidate"
    );
    let removed: u64 = changes
        .iter()
        .map(|pruned| pruned.tokens_before - pruned.tokens_after)
        .sum();
    assert!(removed > 0);
    assert_eq!(
        removed,
        before - estimate(&r).unwrap(),
        "the recorded reduction is the pressure drop"
    );
}

#[test]
fn a_failed_summary_prompt_shrinks_from_the_oldest_unit_and_keeps_its_shape() {
    let directive = INSTRUCTION.trim_end();
    let mut surface = vec![
        Unit::Message(ModelToolContextV1 {
            role: Some("user".into()),
            content: "direction".into(),
            ..Default::default()
        }),
        Unit::Exchange(exchange("first")),
        Unit::Exchange(exchange("second")),
        Unit::Message(ModelToolContextV1 {
            content: directive.into(),
            ..Default::default()
        }),
    ];
    assert!(shrink_summary(&mut surface));
    assert_eq!(surface.len(), 3, "the oldest unit left first");
    assert!(matches!(&surface[1], Unit::Exchange(e) if e.results[0].content == json!("second")));
    assert!(shrink_summary(&mut surface));
    assert_eq!(surface.len(), 2);
    // The base user message and the trailing directive are never candidates.
    assert!(!shrink_summary(&mut surface));
    assert!(matches!(&surface[0], Unit::Message(m) if m.content == "direction"));
    assert!(matches!(&surface[1], Unit::Message(m) if m.content == directive));
}

#[test]
fn pruning_never_touches_result_images() {
    let policy = Policy::default();
    let mut r = request();
    let mut big = exchange(&"x".repeat(50_000));
    big.results[0].images = vec![aworkit_capability_host::model_images::ImageAttachmentV1 {
        id: "a".repeat(64),
        name: "image.one".into(),
        mime_type: "image/png".into(),
        byte_length: 1234,
    }];
    r.exchanges.push(big);
    let images = r.exchanges[0].results[0].images.clone();
    let changes = prune(&mut r, &policy, 0);
    assert_eq!(changes.len(), 1);
    assert_eq!(r.exchanges[0].results[0].images, images);
}

#[test]
fn the_prune_gate_is_exclusive_and_never_grows_a_result() {
    let policy = Policy::default();
    let mut r = request();
    r.exchanges
        .push(exchange(&"y".repeat(policy.threshold_chars + 1)));
    let changes = prune(&mut r, &policy, 0);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].chars_after < changes[0].chars_before);
    let mut small = request();
    small
        .exchanges
        .push(exchange(&"y".repeat(policy.threshold_chars)));
    assert!(
        prune(&mut small, &policy, 0).is_empty(),
        "a result exactly at the gate is already small enough"
    );
}
#[test]
fn every_budget_is_a_share_of_the_window() {
    let p = Policy::default();
    // The provider's own output reservation comes out of the window first, but
    // never more than the target: a model that declares more output than its
    // window (393,216 against 32,000) would otherwise collapse the window to
    // zero, and a zero window has no threshold to cross.
    assert_eq!(p.output_reservation(32_768, Some(393_216)), 8_192);
    assert_eq!(p.output_reservation(262_144, None), 0);
    assert_eq!(p.effective_window(262_144, Some(32_768)), 229_376);
    assert_eq!(p.effective_window(262_144, None), 262_144);
    assert_eq!(p.effective_window(32_768, Some(393_216)), 24_576);
    assert_eq!(p.effective_window(32_000, Some(393_216)), 24_000);

    // Every compaction aims at a quarter of the window, whatever the window is.
    assert_eq!(p.target(32_768), 8_192);
    assert_eq!(p.target(100_000), 25_000);
    assert_eq!(p.target(262_144), 65_536);
    assert_eq!(p.target(1_048_576), 262_144);

    // With room to spare the plan lands exactly on the target, and both parts
    // keep the same share of it: nothing is clipped to a constant, so a large
    // context keeps proportionally more summary and proportionally more history.
    for window in [100_000, 262_144, 1_048_576] {
        let plan = p.replacement_plan(window, 12_400, None);
        assert_eq!(
            12_400 + plan.retain + plan.summary,
            p.target(window),
            "one compaction lands exactly on the target in a {window}-token window"
        );
        let budget = plan.retain + plan.summary;
        assert_eq!(
            plan.summary,
            (budget as f64 * SUMMARY_BUDGET_SHARE).floor() as u64,
            "the summary keeps its share in a {window}-token window"
        );
    }
    assert_eq!(
        p.replacement_plan(100_000, 12_400, None),
        ReplacementPlan {
            retain: 7_787,
            summary: 4_813
        }
    );
    assert_eq!(
        p.replacement_plan(262_144, 12_400, None),
        ReplacementPlan {
            retain: 32_839,
            summary: 20_297
        },
        "the summary grows with the window instead of stopping at a cap"
    );
    assert_eq!(
        p.replacement_plan(1_048_576, 12_400, None),
        ReplacementPlan {
            retain: 154_342,
            summary: 95_402
        },
        "a 1M window gets a 1M summary and a 1M tail"
    );
    // The two parts keep the same ratio at every window - to within the token a
    // floor rounds away - which is what makes this a share rather than a
    // schedule of chosen numbers.
    let small = p.replacement_plan(100_000, 12_400, None);
    let large = p.replacement_plan(1_048_576, 12_400, None);
    for plan in [small, large] {
        let budget = (plan.retain + plan.summary) as f64;
        assert!(
            (plan.summary as f64 / budget - SUMMARY_BUDGET_SHARE).abs() < 0.001,
            "the summary keeps its share of the budget: {plan:?}"
        );
    }

    // The only bound left that is not a context share is the model's own
    // declared output: a summary is never asked to be longer than that, and
    // whatever the summary cannot use stays verbatim.
    assert_eq!(
        p.replacement_plan(1_048_576, 12_400, Some(64_000)),
        ReplacementPlan {
            retain: 185_744,
            summary: 64_000
        }
    );
    assert_eq!(
        p.replacement_plan(1_048_576, 12_400, Some(1_000)),
        ReplacementPlan {
            retain: 248_744,
            summary: 1_000
        }
    );

    // Below the target the replacement floor is still a share of the window, the
    // target is missed, and the run compacts anyway - with a diagnostic that
    // derives the window which would have reached the target instead of naming a
    // constant.
    assert_eq!(
        p.replacement_plan(32_768, 12_400, None),
        ReplacementPlan {
            retain: 1_620,
            summary: 1_001
        },
        "the floor plan is 8% of the window, split by the same ratio"
    );
    assert_eq!(
        p.replacement_plan(32_768, 12_400, None).retain
            + p.replacement_plan(32_768, 12_400, None).summary,
        p.minimum_replacement(32_768),
        "the promise the advisory and can_reduce make"
    );
    assert!(p.can_reduce(32_768, 12_400), "32k can still be reduced");
    assert_eq!(
        p.minimum_window(12_400),
        72_942,
        "the smallest window that reaches the target is derived from the fixed context"
    );
    assert!(
        p.target(72_941) >= 12_400 + p.minimum_replacement(72_941),
        "and the window just below it already reaches it, floor effects aside"
    );
    let advisory = p.compaction_advisory(32_768, 12_400).unwrap();
    for expected in ["25%", "8192", "12400", "8%", "2621", "72942", "still runs"] {
        assert!(
            advisory.contains(expected),
            "the advisory explains {expected}: {advisory}"
        );
    }
    assert!(
        p.compaction_advisory(100_000, 12_400).is_none(),
        "above the floor there is nothing to report"
    );

    // The one remaining stand-down is about headroom, not about a constant: a
    // fixed context that already reaches the trigger leaves nothing to gain, so
    // compacting would only oscillate. The boundary is where the floor
    // replacement no longer fits under the trigger.
    assert!(p.can_reduce(262_144, 12_400));
    assert!(!p.can_reduce(17_222, 12_400));
    assert!(
        p.can_reduce(17_223, 12_400),
        "one token over the boundary reduces"
    );
    assert!(p.compaction_advisory(17_222, 12_400).is_some());
    // Wherever it stands down, compacting could not have helped: the fixed
    // context plus the smallest replacement already reaches the trigger.
    for window in [17_222_u64, 20_000, 32_768, 262_144] {
        if p.can_reduce(window, 12_400) {
            assert!(12_400 + p.minimum_replacement(window) < p.threshold(window));
        }
    }

    // A manual zero retention must not zero the generated replacement.
    let manual: Policy = serde_json::from_value(json!({"retainTokens":0})).unwrap();
    assert_eq!(manual.retention(262_144), 0);
    assert_eq!(
        manual.replacement_plan(262_144, 12_400, None),
        p.replacement_plan(262_144, 12_400, None)
    );
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
    prune(&mut r, &Policy::default(), 0);
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
