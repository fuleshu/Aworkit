use super::reviewer::*;
use aworkit_capability_host::*;
use serde_json::json;

struct Provider(bool);
impl ProviderEnginePortV1 for Provider {
    fn binding_id(&self) -> &str {
        "review"
    }
    fn version_hash(&self) -> &str {
        "v1"
    }
    fn execute(
        &self,
        _: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        // Even a malformed decision or a failure after usage must retain billing evidence.
        emit(ModelEventV1::AssistantOutput("invalid decision".into()))?;
        emit(ModelEventV1::Usage {
            input_tokens: 100,
            output_tokens: 4,
            cache: ModelCacheUsageV1 {
                cached_input_tokens: Some(80),
                cache_miss_input_tokens: Some(20),
                ..Default::default()
            },
        })?;
        if self.0 {
            Err(ProviderError::AcceptanceAmbiguous)
        } else {
            Ok(ProviderAcceptanceV1::Accepted)
        }
    }
}

#[test]
fn unsuccessful_reviews_retain_provider_usage_and_old_decisions_remain_readable() {
    let old: ReviewDecision =
        serde_json::from_value(json!({"decision":"approve","reason":"Requested"})).unwrap();
    assert!(old.cache.is_empty());
    for fail in [false, true] {
        let gateway = FrozenModelGateway::new(vec![Box::new(Provider(fail))]);
        let decision = review_action(
            &gateway,
            "review",
            "v1",
            &[],
            &[],
            &ModelToolCallV1 {
                call_id: "call.1".into(),
                provider_call_id: None,
                capability_id: "tool.shell.host".into(),
                name: "shell".into(),
                arguments: json!({"command":"echo hi"}),
                provider_context: None,
            },
            &json!({"root":"project"}),
            &CancellationToken::default(),
        );
        assert_eq!(decision.decision, ReviewOutcome::AskUser);
        assert_eq!((decision.input_tokens, decision.output_tokens), (100, 4));
        assert_eq!(decision.cache.cached_input_tokens, Some(80));
        assert_eq!(
            serde_json::to_value(&decision).unwrap()["cache"]["cacheMissInputTokens"],
            20
        );
    }
}
