mod provider_tool_support;

use aworkit_capability_host::{
    GoogleGeminiLimitsV1, GoogleGeminiProvider, GoogleGeminiProviderConfig,
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolExchangeV1,
    ModelToolRequestV1, ModelToolResultV1, ProviderAcceptanceV1, ProviderEnginePortV1,
    ProviderError,
};
use provider_tool_support::{FixtureResponse, start_fixture};
use serde_json::{Value, json};

fn tool() -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "tool.python.host".to_owned(),
        name: "python_host".to_owned(),
        description: "Run bounded Python in the approved project context.".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{"code":{"type":"string"}},
            "required":["code"],
            "additionalProperties":false
        }),
    }
}

fn request(exchanges: Vec<ModelToolExchangeV1>) -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        input: json!({"messages":[
            {"role":"system","content":"Use approved tools for calculations."},
            {"role":"user","content":"Calculate six times seven."}
        ]}),
        tools: vec![tool()],
        exchanges,
    }
}

fn provider(origin: &str) -> GoogleGeminiProvider {
    GoogleGeminiProvider::new(
        GoogleGeminiProviderConfig::new(
            "binding.gemini.tools",
            "wire-v1",
            origin,
            "gemini-fixture",
            Some("gemini-tool-secret".to_owned()),
            GoogleGeminiLimitsV1::default(),
        )
        .expect("Gemini tool config"),
    )
    .expect("Gemini tool provider")
}

fn execute(
    provider: &GoogleGeminiProvider,
    request: &ModelToolRequestV1,
) -> Result<Vec<ModelToolEventV1>, ProviderError> {
    let mut events = Vec::new();
    let acceptance = provider.execute_tool_turn(request, &mut |event| {
        events.push(event);
        Ok(())
    })?;
    assert_eq!(acceptance, ProviderAcceptanceV1::Accepted);
    Ok(events)
}

fn exchange(events: &[ModelToolEventV1]) -> ModelToolExchangeV1 {
    let assistant_content = events
        .iter()
        .filter_map(|event| match event {
            ModelToolEventV1::AssistantOutput { text } => {
                Some(ModelAssistantContentV1::Text { text: text.clone() })
            }
            ModelToolEventV1::ToolCall { call } => {
                Some(ModelAssistantContentV1::ToolCall { call: call.clone() })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let call_id = assistant_content
        .iter()
        .find_map(|content| match content {
            ModelAssistantContentV1::ToolCall { call } => Some(call.call_id.clone()),
            ModelAssistantContentV1::Text { .. } => None,
        })
        .expect("tool call");
    ModelToolExchangeV1 {
        assistant_content,
        results: vec![ModelToolResultV1 {
            call_id,
            content: json!({"stdout":"42\n","exitCode":0}),
            is_error: false,
        }],
    }
}

#[test]
fn gemini_function_call_and_response_preserve_signature_and_exact_usage() {
    let (origin, server) = start_fixture(2, |index, request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-fixture:generateContent"
        );
        assert_eq!(
            request.headers.get("x-goog-api-key").map(String::as_str),
            Some("gemini-tool-secret")
        );
        let body: Value = serde_json::from_slice(&request.body).expect("Gemini request JSON");
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "Use approved tools for calculations."
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "python_host"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "object"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(body["generationConfig"]["candidateCount"], 1);
        if index == 0 {
            assert_eq!(body["contents"].as_array().expect("contents").len(), 1);
            return FixtureResponse::json(json!({
                "candidates":[{
                    "finishReason":"STOP",
                    "content":{"role":"model","parts":[{
                        "functionCall":{"id":"gemini-call-1","name":"python_host","args":{"code":"print(6 * 7)"}},
                        "thoughtSignature":"opaque-thought-signature"
                    }]}
                }],
                "usageMetadata":{
                    "promptTokenCount":13,
                    "candidatesTokenCount":4,
                    "thoughtsTokenCount":6,
                    "totalTokenCount":23
                }
            }));
        }
        assert_eq!(body["contents"].as_array().expect("contents").len(), 3);
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            "opaque-thought-signature"
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["id"],
            "gemini-call-1"
        );
        assert_eq!(body["contents"][2]["role"], "user");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["id"],
            "gemini-call-1"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["output"]["stdout"],
            "42\n"
        );
        FixtureResponse::json(json!({
            "candidates":[{
                "finishReason":"STOP",
                "content":{"role":"model","parts":[{"text":"Six times seven is 42."}]}
            }],
            "usageMetadata":{"promptTokenCount":27,"candidatesTokenCount":7,"totalTokenCount":34}
        }))
    });
    let provider = provider(&origin);

    let first = execute(&provider, &request(Vec::new())).expect("first Gemini turn");
    let call = match &first[0] {
        ModelToolEventV1::ToolCall { call } => call,
        _ => panic!("expected Gemini function call"),
    };
    assert_eq!(call.call_id, "gemini-call-1");
    assert_eq!(call.provider_call_id.as_deref(), Some("gemini-call-1"));
    assert_eq!(call.capability_id, "tool.python.host");
    assert_eq!(call.arguments, json!({"code":"print(6 * 7)"}));
    assert_eq!(
        format!("{:?}", call.provider_context.as_ref().expect("signature")),
        "ModelProviderContextV1([OPAQUE])"
    );
    assert_eq!(
        first[1],
        ModelToolEventV1::Usage {
            input_tokens: 13,
            output_tokens: 10
        }
    );

    let second = execute(&provider, &request(vec![exchange(&first)])).expect("result turn");
    assert_eq!(
        second,
        vec![
            ModelToolEventV1::AssistantOutput {
                text: "Six times seven is 42.".to_owned()
            },
            ModelToolEventV1::Usage {
                input_tokens: 27,
                output_tokens: 7
            }
        ]
    );
    server.join().expect("Gemini fixture");
}

#[test]
fn gemini_rejects_malformed_function_arguments_without_partial_events() {
    let (origin, server) = start_fixture(1, |_index, _request| {
        FixtureResponse::json(json!({
            "candidates":[{
                "finishReason":"STOP",
                "content":{"role":"model","parts":[{
                    "functionCall":{"id":"bad-call","name":"python_host","args":["not","an","object"]}
                }]}
            }],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1}
        }))
    });
    let provider = provider(&origin);
    let mut events = Vec::new();
    let error = provider
        .execute_tool_turn(&request(Vec::new()), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect_err("malformed Gemini call");
    assert!(error.to_string().contains("Gemini tool response"));
    assert!(events.is_empty());
    server.join().expect("Gemini malformed fixture");
}
