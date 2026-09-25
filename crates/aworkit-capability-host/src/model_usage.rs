//! Optional provider-reported accounting counters. The token counters partition
//! input usage; they are never added to the total input tokens or reconstructed
//! for old calls. The harness also records the size of the request it sent, so a
//! billed prompt count can always be compared against the bytes that produced it.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCacheUsageV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_input_tokens: Option<u64>,
    /// Provider-reported total, kept so its own prompt and completion split can
    /// be cross-checked against what it billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Size in bytes of the request body this call actually sent. A token covers
    /// at least one byte, so a prompt count above this number is an accounting
    /// fault rather than a large payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_bytes: Option<u64>,
    /// Leading bytes this request shares with the previous one on the same
    /// binding: the largest prefix a provider-side prefix cache could have
    /// reused. A cache hit far below it means the provider dropped a prefix we
    /// kept identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_prefix_bytes: Option<u64>,
}

impl ModelCacheUsageV1 {
    pub fn is_empty(&self) -> bool {
        self.cached_input_tokens.is_none() && self.cache_miss_input_tokens.is_none()
    }

    /// DeepSeek's two hit fields are aliases, not separate charges. Prefer its
    /// explicit counter and accept the standard OpenAI detail as a fallback.
    /// Missing, null or non-integer optional values remain unknown, not zero.
    pub(crate) fn from_openai(usage: &Map<String, Value>) -> Self {
        Self {
            cached_input_tokens: usage
                .get("prompt_cache_hit_tokens")
                .and_then(Value::as_u64)
                .or_else(|| {
                    usage
                        .get("prompt_tokens_details")?
                        .get("cached_tokens")?
                        .as_u64()
                }),
            cache_miss_input_tokens: usage
                .get("prompt_cache_miss_tokens")
                .and_then(Value::as_u64),
            total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
            sent_bytes: None,
            common_prefix_bytes: None,
        }
    }
}

/// Whole-Run accumulation of the optional cache counters above. A figure stays
/// absent until at least one call reports it, so an unreported counter is never
/// published as a fabricated zero. Reported values sum with saturation because a
/// Run can bill more tokens than any single counter can hold.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModelCacheTotalsV1 {
    pub cached_input_tokens: Option<u64>,
    pub cache_miss_input_tokens: Option<u64>,
}

impl ModelCacheTotalsV1 {
    /// Adds one call's reported counters; a missing value leaves the running
    /// total unchanged rather than resetting it.
    pub fn add(&mut self, usage: ModelCacheUsageV1) {
        self.merge(ModelCacheTotalsV1 {
            cached_input_tokens: usage.cached_input_tokens,
            cache_miss_input_tokens: usage.cache_miss_input_tokens,
        });
    }

    /// Merges a nested total, keeping an unreported counter absent instead of
    /// treating it as zero.
    pub fn merge(&mut self, totals: ModelCacheTotalsV1) {
        self.cached_input_tokens = add_units(self.cached_input_tokens, totals.cached_input_tokens);
        self.cache_miss_input_tokens =
            add_units(self.cache_miss_input_tokens, totals.cache_miss_input_tokens);
    }

    /// True when no call reported either counter.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cached_input_tokens.is_none() && self.cache_miss_input_tokens.is_none()
    }
}

fn add_units(total: Option<u64>, next: Option<u64>) -> Option<u64> {
    match (total, next) {
        (Some(total), Some(next)) => Some(total.saturating_add(next)),
        (Some(total), None) => Some(total),
        (None, next) => next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn totals_sum_reported_calls_and_keep_unreported_counters_absent() {
        let mut totals = ModelCacheTotalsV1::default();
        assert!(totals.is_empty());
        // One call reports only a hit count; the miss counter stays unknown.
        totals.add(ModelCacheUsageV1 {
            cached_input_tokens: Some(80),
            ..ModelCacheUsageV1::default()
        });
        assert_eq!(totals.cached_input_tokens, Some(80));
        assert_eq!(totals.cache_miss_input_tokens, None);
        // Later calls add to the known counter and introduce the missing one.
        totals.add(ModelCacheUsageV1 {
            cached_input_tokens: Some(20),
            cache_miss_input_tokens: Some(5),
            ..ModelCacheUsageV1::default()
        });
        assert_eq!(totals.cached_input_tokens, Some(100));
        assert_eq!(totals.cache_miss_input_tokens, Some(5));
        // A call that reports neither never resets the running totals.
        totals.add(ModelCacheUsageV1::default());
        assert_eq!(totals.cached_input_tokens, Some(100));
        assert_eq!(totals.cache_miss_input_tokens, Some(5));
    }

    #[test]
    fn totals_reproduce_the_recorded_test1_3_run_aggregate() {
        // Per-turn (cached, miss) counters recorded for test1_3
        // (chat.ef71761303be6ac41947a5a7954783d4ade0b614), read from the same
        // store `qa/cache-usage-report.mjs` queries. Summing them must reproduce
        // that report's Run total exactly, so the panel shows the Run figures
        // rather than the last loaded page.
        const TURNS: &[(u64, u64)] = &[
            (256, 2537),
            (2688, 238),
            (2432, 8915),
            (11264, 1025),
            (12032, 532),
            (46720, 204),
            (46848, 6016),
            (52736, 489),
            (59008, 215),
            (59520, 558),
            (59776, 530),
            (68864, 324),
            (77312, 150),
            (77440, 697),
            (77824, 1689),
            (79232, 2098),
            (81024, 560),
            (81664, 312),
            (82048, 302),
            (82304, 680),
            (82688, 6761),
            (89088, 2691),
            (92416, 717),
            (93184, 400),
            (93312, 426),
            (99328, 193),
            (102400, 241),
            (108416, 204),
            (111104, 220),
            (114560, 250),
            (116480, 175),
            (117376, 568),
            (118656, 450),
            (118784, 522),
            (122624, 646),
            (123008, 1610),
            (124288, 1107),
            (127488, 669),
            (127872, 631),
            (128896, 338),
            (129664, 313),
            (142720, 288),
            (143360, 378),
            (143360, 608),
            (144000, 1020),
            (150528, 363),
            (150528, 560),
            (151040, 1240),
            (154240, 374),
            (154752, 326),
            (155136, 267),
            (155520, 378),
            (155520, 595),
            (156160, 1045),
            (157440, 752),
            (162560, 157),
            (163200, 2451),
            (167808, 488),
            (167936, 680),
            (169728, 306),
            (170496, 466),
            (170624, 686),
            (171264, 864),
            (172416, 1717),
            (174592, 980),
        ];
        let mut totals = ModelCacheTotalsV1::default();
        for (cached, miss) in TURNS {
            totals.add(ModelCacheUsageV1 {
                cached_input_tokens: Some(*cached),
                cache_miss_input_tokens: Some(*miss),
                ..ModelCacheUsageV1::default()
            });
        }
        assert_eq!(TURNS.len(), 65);
        assert_eq!(totals.cached_input_tokens, Some(7_207_552));
        assert_eq!(totals.cache_miss_input_tokens, Some(64_192));
        let cached = totals.cached_input_tokens.unwrap();
        let uncached = totals.cache_miss_input_tokens.unwrap();
        assert_eq!(cached + uncached, 7_271_744, "partitions the Run input");
        assert_eq!(
            format!("{:.1}%", 100.0 * cached as f64 / (cached + uncached) as f64),
            "99.1%"
        );
    }

    #[test]
    fn historical_usage_deserializes_and_reported_cache_survives_projection() {
        let old = json!({"kind":"usage","input_tokens":100,"output_tokens":4});
        let event: crate::ModelToolEventV1 = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), old);
        assert!(crate::project_model_tool_events(&[event]).cache.is_empty());
        let new = json!({"kind":"usage","input_tokens":100,"output_tokens":4,
            "cache":{"cachedInputTokens":80,"cacheMissInputTokens":20}});
        let event: crate::ModelToolEventV1 = serde_json::from_value(new.clone()).unwrap();
        let projection = crate::project_model_tool_events(&[event]);
        assert_eq!(projection.cache.cached_input_tokens, Some(80));
        assert_eq!(projection.output_value(), json!([new]));
    }

    #[test]
    fn aliases_are_not_added_and_zero_is_not_unknown() {
        let read = |v: Value| ModelCacheUsageV1::from_openai(v.as_object().unwrap());
        let cache = read(
            json!({"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20,
            "prompt_tokens_details":{"cached_tokens":80}}),
        );
        assert_eq!(cache.cached_input_tokens, Some(80));
        assert_eq!(cache.cache_miss_input_tokens, Some(20));
        assert_eq!(
            read(json!({"prompt_cache_hit_tokens":0})).cached_input_tokens,
            Some(0)
        );
        assert!(read(json!({})).is_empty());
        assert!(
            read(json!({"prompt_cache_hit_tokens":null,"prompt_cache_miss_tokens":-1})).is_empty()
        );
        assert_eq!(
            read(json!({"prompt_tokens_details":{"cached_tokens":60}})).cached_input_tokens,
            Some(60)
        );
        assert_eq!(
            serde_json::to_value(ModelCacheUsageV1::default()).unwrap(),
            json!({})
        );
    }
}
