mod provider_tool_support;

use aworkit_capability_host::{
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolExchangeV1,
    ModelToolRequestV1, ModelToolResultV1, OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider,
    OpenAiCompatibleProviderConfig, ProviderAcceptanceV1, ProviderEnginePortV1, ProviderError,
};
use provider_tool_support::{FixtureResponse, start_fixture};
use serde_json::{Value, json};

fn tool() -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "tool.files.read".to_owned(),
        name: "files_read".to_owned(),
        description: "Read one project-relative text file.".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{"path":{"type":"string"}},
            "required":["path"],
            "additionalProperties":false
        }),
    }
}

fn request(exchanges: Vec<ModelToolExchangeV1>) -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        input: json!({"messages":[
            {"role":"system","content":"Use the provided project tools."},
            {"role":"user","content":"Read the README."}
        ]}),
        tools: vec![tool()],
        exchanges,
    }
}

fn provider(origin: &str, maximum_response_bytes: usize) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        OpenAiCompatibleProviderConfig::new(
            "binding.openai.tools",
            "wire-v1",
            format!("{origin}/v1"),
            "gpt-fixture",
            Some("openai-tool-secret".to_owned()),
            OpenAiCompatibleLimitsV1 {
                maximum_response_bytes,
                ..OpenAiCompatibleLimitsV1::default()
            },
        )
        .expect("OpenAI tool config"),
    )
    .expect("OpenAI tool provider")
}

fn execute(
    provider: &OpenAiCompatibleProvider,
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
            content: json!({"text":"Aworkit README"}),
            is_error: false,
        }],
    }
}

#[test]
fn openai_tool_call_and_result_round_trip_exact_wire_and_usage() {
    let (origin, server) = start_fixture(2, |index, request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer openai-tool-secret")
        );
        let body: Value = serde_json::from_slice(&request.body).expect("OpenAI request JSON");
        assert_eq!(body["model"], "gpt-fixture");
        assert_eq!(body["stream"], false);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "files_read");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
        if index == 0 {
            assert_eq!(body["messages"].as_array().expect("messages").len(), 2);
            return FixtureResponse::json(json!({
                "choices":[{
                    "finish_reason":"tool_calls",
                    "message":{
                        "role":"assistant",
                        "content":"I will inspect it.",
                        "tool_calls":[{
                            "id":"call_read_1",
                            "type":"function",
                            "function":{"name":"files_read","arguments":"{\"path\":\"README.md\"}"}
                        }]
                    }
                }],
                "usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19}
            }));
        }
        assert_eq!(body["messages"].as_array().expect("messages").len(), 4);
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call_read_1");
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "call_read_1");
        assert_eq!(
            body["messages"][3]["content"],
            "{\"text\":\"Aworkit README\"}"
        );
        FixtureResponse::json(json!({
            "choices":[{
                "finish_reason":"stop",
                "message":{
                    "role":"assistant",
                    "content":"The README describes Aworkit.",
                    "tool_calls":null
                }
            }],
            "usage":{"prompt_tokens":24,"completion_tokens":6,"total_tokens":30}
        }))
    });
    let provider = provider(&origin, 1024 * 1024);

    let first = execute(&provider, &request(Vec::new())).expect("first tool turn");
    assert_eq!(
        first,
        vec![
            ModelToolEventV1::AssistantOutput {
                text: "I will inspect it.".to_owned()
            },
            ModelToolEventV1::ToolCall {
                call: match &first[1] {
                    ModelToolEventV1::ToolCall { call } => {
                        assert_eq!(call.call_id, "call_read_1");
                        assert_eq!(call.provider_call_id.as_deref(), Some("call_read_1"));
                        assert_eq!(call.capability_id, "tool.files.read");
                        assert_eq!(call.arguments, json!({"path":"README.md"}));
                        call.clone()
                    }
                    _ => panic!("expected OpenAI tool call"),
                }
            },
            ModelToolEventV1::Usage {
                input_tokens: 12,
                output_tokens: 7
            }
        ]
    );
    let second = execute(&provider, &request(vec![exchange(&first)])).expect("result turn");
    assert_eq!(
        second,
        vec![
            ModelToolEventV1::AssistantOutput {
                text: "The README describes Aworkit.".to_owned()
            },
            ModelToolEventV1::Usage {
                input_tokens: 24,
                output_tokens: 6
            }
        ]
    );
    server.join().expect("OpenAI fixture");
}

#[test]
fn openai_rejects_malformed_calls_and_oversized_tool_responses_without_events() {
    let (origin, malformed_server) = start_fixture(1, |_index, _request| {
        FixtureResponse::json(json!({
            "choices":[{
                "finish_reason":"tool_calls",
                "message":{"content":null,"tool_calls":[{
                    "id":"call_bad",
                    "type":"function",
                    "function":{"name":"files_read","arguments":"not-json"}
                }]}
            }],
            "usage":{"prompt_tokens":1,"completion_tokens":1}
        }))
    });
    let malformed_provider = provider(&origin, 1024 * 1024);
    let mut events = Vec::new();
    let error = malformed_provider
        .execute_tool_turn(&request(Vec::new()), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect_err("malformed tool call");
    assert!(error.to_string().contains("OpenAI tool response"));
    assert!(events.is_empty());
    malformed_server.join().expect("malformed fixture");

    let (origin, oversized_server) =
        start_fixture(1, |_index, _request| FixtureResponse::raw(vec![b'x'; 257]));
    let oversized_provider = provider(&origin, 256);
    let error = execute(&oversized_provider, &request(Vec::new())).expect_err("oversized response");
    assert!(error.to_string().contains("size bound"));
    oversized_server.join().expect("oversized fixture");
}
