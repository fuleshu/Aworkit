use aworkit_capability_host::{
    FrozenModelGateway, ModelCandidateV1, ModelEventV1, ModelRequestV1, ModelResolutionPlanV1,
    ModelToolCallV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
};
use serde_json::json;

struct ContractProvider {
    events: Vec<ModelToolEventV1>,
}

impl ProviderEnginePortV1 for ContractProvider {
    fn binding_id(&self) -> &str {
        "binding.contract"
    }

    fn version_hash(&self) -> &str {
        "wire-v1"
    }

    fn execute(
        &self,
        _request: &ModelRequestV1,
        _emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        panic!("text execution is outside this contract test")
    }

    fn execute_tool_turn_cancellable(
        &self,
        _request: &ModelToolRequestV1,
        _cancellation: &aworkit_capability_host::CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        for event in self.events.clone() {
            emit(event)?;
        }
        Ok(ProviderAcceptanceV1::Accepted)
    }
}

fn tool(schema: serde_json::Value) -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "tool.files.read".to_owned(),
        name: "files_read".to_owned(),
        description: "Read one project-relative file.".to_owned(),
        input_schema: schema,
    }
}

fn request(schema: serde_json::Value) -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        input: json!("read the README"),
        parameters: Default::default(),
        tools: vec![tool(schema)],
        exchanges: Vec::new(),
    }
}

fn plan(maximum_output_bytes: usize) -> ModelResolutionPlanV1 {
    ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: "binding.contract".to_owned(),
            version_hash: "wire-v1".to_owned(),
        }],
        maximum_input_bytes: 1024 * 1024,
        maximum_output_bytes,
    }
}

#[test]
fn frozen_gateway_rejects_oversized_tool_schema_before_provider_dispatch() {
    let gateway = FrozenModelGateway::new(vec![Box::new(ContractProvider {
        events: vec![ModelToolEventV1::AssistantOutput {
            text: "must not execute".to_owned(),
        }],
    })]);
    let error = gateway
        .execute_tool_turn(
            &plan(1024),
            &request(json!({
                "type":"object",
                "description":"x".repeat(65 * 1024)
            })),
        )
        .expect_err("oversized schema");
    assert_eq!(
        error.to_string(),
        "provider failed: provider tool request is invalid"
    );
}

#[test]
fn frozen_gateway_bounds_normalized_tool_call_payloads() {
    let gateway = FrozenModelGateway::new(vec![Box::new(ContractProvider {
        events: vec![
            ModelToolEventV1::ToolCall {
                call: ModelToolCallV1 {
                    call_id: "call-1".to_owned(),
                    provider_call_id: Some("call-1".to_owned()),
                    capability_id: "tool.files.read".to_owned(),
                    name: "files_read".to_owned(),
                    arguments: json!({"path":"x".repeat(256)}),
                    provider_context: None,
                },
            },
            ModelToolEventV1::Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        ],
    })]);
    assert_eq!(
        gateway.execute_tool_turn(
            &plan(64),
            &request(json!({"type":"object","properties":{}})),
        ),
        Err(ProviderError::OutputBound)
    );
}

#[test]
fn accepted_tool_turn_requires_exact_usage() {
    let gateway = FrozenModelGateway::new(vec![Box::new(ContractProvider {
        events: vec![ModelToolEventV1::AssistantOutput {
            text: "answer".to_owned(),
        }],
    })]);
    assert_eq!(
        gateway.execute_tool_turn(
            &plan(1024),
            &request(json!({"type":"object","properties":{}})),
        ),
        Err(ProviderError::MissingOrDuplicateUsage)
    );
}
