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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
