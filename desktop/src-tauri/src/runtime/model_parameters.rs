//! Reasoning controls at the dispatch boundary.
//!
//! A workflow node or a saved model setting may carry reasoning controls the
//! resolved adapter has no consumer for. Dispatch drops what the binding cannot
//! honour and reports each drop as a capability warning, because a rejected
//! dispatch is a dead run (see `aworkit.workflow_worker.failure_policy`). The
//! saved documents keep the user's intent, so binding the tier to an adapter
//! that consumes the control restores it on the next pass.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::settings_v2::{ModelConfigurationV2, ProviderConfigurationV2};

/// A report is advisory prose for one pass, so it stays bounded. The graph is
/// small; this only guards a pathological document.
const MAXIMUM_WARNINGS: usize = 8;

/// The reasoning controls one adapter actually reads from a request.
///
/// `maxOutputTokens` belongs here because every adapter that dispatches a
/// completion applies it. `anthropic` takes no request parameters at all, so a
/// model or node setting there is always dropped.
fn consumed_controls(provider_kind: &str) -> &'static [&'static str] {
    match provider_kind {
        "openai_compatible" | "xiaomi_mimo" => &[
            "reasoningEffort",
            "enableThinking",
            "preserveThinking",
            "maxOutputTokens",
        ],
        "gemini" => &["reasoningEffort", "enableThinking", "maxOutputTokens"],
        _ => &[],
    }
}

/// Effort values a model advertised during discovery, if it advertised any.
fn advertised_efforts(model: &ModelConfigurationV2) -> Vec<&str> {
    model
        .capabilities
        .iter()
        .filter_map(|capability| capability.strip_prefix("reasoning_effort:"))
        .collect()
}

/// Drops every reasoning control the resolved binding cannot honour and returns
/// one warning per drop, naming the node or model and the adapter that ignored
/// it.
///
/// The document is the snapshot one pass runs, never the saved workflow, so a
/// later tier remap sees the user's original intent again.
pub(super) fn reconcile(
    document: &mut Value,
    model_parameters: &mut BTreeMap<String, Value>,
    provider: &ProviderConfigurationV2,
    model: &ModelConfigurationV2,
) -> Vec<String> {
    let consumed = consumed_controls(&provider.kind);
    let mut warnings = Vec::new();
    // Model-level Settings parameters apply wherever the concrete model runs, so
    // a key this adapter cannot consume is dropped for this pass and reported.
    let unsupported: Vec<String> = model_parameters
        .keys()
        .filter(|key| !consumed.contains(&key.as_str()))
        .cloned()
        .collect();
    for key in unsupported {
        model_parameters.remove(&key);
        warnings.push(format!(
            "model '{}' sets '{key}', but the '{}' adapter has no consumer for it; this pass ignores it",
            model.name, provider.kind
        ));
    }

    let advertised = advertised_efforts(model);
    let Some(nodes) = document.get_mut("nodes").and_then(Value::as_array_mut) else {
        return warnings;
    };
    let mut omitted = 0_usize;
    for node in nodes {
        if !matches!(
            node.get("type").and_then(Value::as_str),
            Some("agent" | "model_call")
        ) {
            continue;
        }
        let node_id = node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let Some(configuration) = node.get_mut("configuration").and_then(Value::as_object_mut)
        else {
            continue;
        };
        let controls: Vec<&str> = ["reasoningEffort", "enableThinking", "preserveThinking"]
            .into_iter()
            .filter(|key| {
                configuration
                    .get(*key)
                    .is_some_and(|value| !value.is_null())
            })
            .collect();
        if controls.is_empty() {
            continue;
        }
        let effort = configuration
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut messages = Vec::new();
        let mut dropped: Vec<&str> = controls
            .iter()
            .copied()
            .filter(|key| !consumed.contains(key))
            .collect();
        for key in &dropped {
            messages.push(format!(
                "workflow node '{node_id}' sets {key}, but the '{}' adapter has no consumer for it; this pass ignores it",
                provider.kind
            ));
        }
        // Discovery is the model's own word about which efforts it accepts. An
        // effort outside that list would be refused by the provider, so the pass
        // falls back to the model default and says so.
        if !dropped.contains(&"reasoningEffort")
            && provider.kind == "openai_compatible"
            && let Some(effort) = effort.as_deref()
            && !advertised.is_empty()
            && !advertised.iter().any(|value| *value == effort)
        {
            dropped.push("reasoningEffort");
            messages.push(format!(
                "workflow node '{node_id}' selects reasoningEffort '{effort}', but model '{}' advertises only {}; this pass uses the model default",
                model.name,
                advertised.join(", ")
            ));
        }
        for key in dropped {
            configuration.insert(key.to_owned(), Value::Null);
        }
        if warnings.len().saturating_add(messages.len()) > MAXIMUM_WARNINGS {
            omitted = omitted.saturating_add(messages.len());
        } else {
            warnings.extend(messages);
        }
    }
    if omitted > 0 {
        warnings.push(format!(
            "and {omitted} more node reasoning-control notice(s) omitted from this report"
        ));
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: &str) -> ProviderConfigurationV2 {
        ProviderConfigurationV2 {
            id: "provider.fixture".into(),
            name: "Fixture".into(),
            kind: kind.into(),
            base_url: "https://example.com/v1".into(),
            enabled: true,
            credential_ref: None,
            models: Vec::new(),
            configuration: BTreeMap::new(),
        }
    }

    fn model(capabilities: &[&str]) -> ModelConfigurationV2 {
        ModelConfigurationV2 {
            id: "model.fixture".into(),
            name: "Fixture model".into(),
            remote_id: "fixture-model".into(),
            enabled: true,
            context_window: None,
            max_output_tokens: None,
            compaction: None,
            capabilities: capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            parameters: BTreeMap::new(),
        }
    }

    fn document(configuration: Value) -> Value {
        json!({"nodes":[{"id":"agent.1","type":"agent","configuration":configuration}]})
    }

    #[test]
    fn a_consuming_adapter_keeps_every_control() {
        for (kind, configuration) in [
            (
                "openai_compatible",
                json!({"reasoningEffort":"high","enableThinking":false}),
            ),
            ("xiaomi_mimo", json!({"enableThinking":true})),
            (
                "gemini",
                json!({"reasoningEffort":"high","enableThinking":true}),
            ),
        ] {
            let mut document = document(configuration.clone());
            let mut parameters = BTreeMap::new();
            let warnings = reconcile(
                &mut document,
                &mut parameters,
                &provider(kind),
                &model(&["text", "reasoning_effort:low", "reasoning_effort:high"]),
            );
            assert!(warnings.is_empty(), "{kind} warned: {warnings:?}");
            assert_eq!(document["nodes"][0]["configuration"], configuration);
        }
    }

    #[test]
    fn an_adapter_without_a_consumer_drops_the_controls_and_says_so() {
        let mut document = document(json!({"reasoningEffort":"high","enableThinking":false}));
        let mut parameters = BTreeMap::new();
        let warnings = reconcile(
            &mut document,
            &mut parameters,
            &provider("anthropic"),
            &model(&["text"]),
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("agent.1") && warnings[0].contains("anthropic"));
        assert_eq!(
            document["nodes"][0]["configuration"]["reasoningEffort"],
            Value::Null
        );
        assert_eq!(
            document["nodes"][0]["configuration"]["enableThinking"],
            Value::Null
        );
    }

    #[test]
    fn an_unadvertised_effort_falls_back_to_the_model_default() {
        let mut document = document(json!({"reasoningEffort":"max","enableThinking":true}));
        let mut parameters = BTreeMap::new();
        let warnings = reconcile(
            &mut document,
            &mut parameters,
            &provider("openai_compatible"),
            &model(&["text", "reasoning_effort:low", "reasoning_effort:high"]),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("advertises only low, high"),
            "{warnings:?}"
        );
        assert_eq!(
            document["nodes"][0]["configuration"]["reasoningEffort"],
            Value::Null
        );
        assert_eq!(
            document["nodes"][0]["configuration"]["enableThinking"],
            json!(true),
            "a control the adapter consumes is never dropped"
        );
    }

    #[test]
    fn a_model_setting_the_adapter_cannot_consume_is_dropped() {
        let mut document = document(json!({}));
        let mut parameters = BTreeMap::from([("preserveThinking".to_owned(), json!(true))]);
        let warnings = reconcile(
            &mut document,
            &mut parameters,
            &provider("gemini"),
            &model(&["text"]),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("preserveThinking"), "{warnings:?}");
        assert!(parameters.is_empty());
    }

    #[test]
    fn the_report_is_bounded_and_counts_what_it_omitted() {
        let nodes = (0..20)
            .map(|index| {
                json!({
                    "id": format!("agent.{index}"),
                    "type": "agent",
                    "configuration": {"enableThinking": false}
                })
            })
            .collect::<Vec<_>>();
        let mut document = json!({"nodes": nodes});
        let mut parameters = BTreeMap::new();
        let warnings = reconcile(
            &mut document,
            &mut parameters,
            &provider("anthropic"),
            &model(&["text"]),
        );
        assert!(warnings.len() <= MAXIMUM_WARNINGS + 1, "{warnings:?}");
        assert!(
            warnings
                .last()
                .is_some_and(|warning| warning.contains("more node")),
            "the bound names what it omitted: {warnings:?}"
        );
        // Every node is still reconciled, not only the reported ones.
        assert!(
            document["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|node| node["configuration"]["enableThinking"].is_null())
        );
    }
}
