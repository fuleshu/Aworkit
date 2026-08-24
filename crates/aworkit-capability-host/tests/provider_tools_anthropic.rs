mod provider_tool_support;

use aworkit_capability_host::{
    AnthropicMessagesLimitsV1, AnthropicMessagesProvider, AnthropicMessagesProviderConfig,
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolExchangeV1,
    ModelToolRequestV1, ModelToolResultV1, ProviderAcceptanceV1, ProviderEnginePortV1,
    ProviderError,
};
use provider_tool_support::{FixtureResponse, start_fixture};
use serde_json::{Value, json};

fn tool() -> ModelToolDefinitionV1 {
    ModelToolDefinitionV1 {
        capability_id: "tool.files.search".to_owned(),
        name: "files_search".to_owned(),
        description: "Search project files for a bounded text pattern.".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{"query":{"type":"string"}},
            "required":["query"],
            "additionalProperties":false
        }),
    }
}

fn request(exchanges: Vec<ModelToolExchangeV1>) -> ModelToolRequestV1 {
    ModelToolRequestV1 {
        input: json!({"messages":[
            {"role":"system","content":"Use tools when evidence is needed."},
            {"role":"user","content":"Find the settings implementation."}
        ]}),
        tools: vec![tool()],
        exchanges,
    }
}

fn provider(origin: &str) -> AnthropicMessagesProvider {
    AnthropicMessagesProvider::new(
        AnthropicMessagesProviderConfig::new(
            "binding.anthropic.tools",
            "wire-v1",
            origin,
            "claude-fixture",
            Some("anthropic-tool-secret".to_owned()),
            AnthropicMessagesLimitsV1::default(),
        )
        .expect("Anthropic tool config"),
    )
    .expect("Anthropic tool provider")
}

fn execute(
    provider: &AnthropicMessagesProvider,
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
            content: Value::String("project scope denied the requested path".to_owned()),
            is_error: true,
        }],
    }
}

#[test]
fn anthropic_tool_use_and_result_round_trip_exact_wire_and_usage() {
    let (origin, server) = start_fixture(2, |index, request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(
            request.headers.get("x-api-key").map(String::as_str),
            Some("anthropic-tool-secret")
        );
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
        let body: Value = serde_json::from_slice(&request.body).expect("Anthropic request JSON");
        assert_eq!(body["model"], "claude-fixture");
        assert_eq!(body["system"], "Use tools when evidence is needed.");
        assert_eq!(body["stream"], false);
        assert_eq!(body["tool_choice"]["type"], "auto");
        assert_eq!(body["tools"][0]["name"], "files_search");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        if index == 0 {
            assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
            return FixtureResponse::json(json!({
                "role":"assistant",
                "content":[
                    {"type":"text","text":"I will search."},
                    {"type":"tool_use","id":"toolu_search_1","name":"files_search","input":{"query":"SettingsScreen"}}
                ],
                "stop_reason":"tool_use",
                "usage":{
                    "input_tokens":11,
                    "cache_creation_input_tokens":2,
                    "cache_read_input_tokens":3,
                    "output_tokens":5
                }
            }));
        }
        assert_eq!(body["messages"].as_array().expect("messages").len(), 3);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(body["messages"][1]["content"][1]["id"], "toolu_search_1");
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            "toolu_search_1"
        );
        assert_eq!(body["messages"][2]["content"][0]["is_error"], true);
        FixtureResponse::json(json!({
            "role":"assistant",
            "content":[{"type":"text","text":"That path is outside the approved scope."}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":21,"output_tokens":8}
        }))
    });
    let provider = provider(&origin);

    let first = execute(&provider, &request(Vec::new())).expect("first Anthropic turn");
    assert_eq!(
        first[0],
        ModelToolEventV1::AssistantOutput {
            text: "I will search.".to_owned()
        }
    );
    let call = match &first[1] {
        ModelToolEventV1::ToolCall { call } => call,
        _ => panic!("expected Anthropic tool call"),
    };
    assert_eq!(call.call_id, "toolu_search_1");
    assert_eq!(call.capability_id, "tool.files.search");
    assert_eq!(call.arguments, json!({"query":"SettingsScreen"}));
    assert_eq!(
        first[2],
        ModelToolEventV1::Usage {
            input_tokens: 16,
            output_tokens: 5
        }
    );

    let second = execute(&provider, &request(vec![exchange(&first)])).expect("result turn");
    assert_eq!(
        second,
        vec![
            ModelToolEventV1::AssistantOutput {
                text: "That path is outside the approved scope.".to_owned()
            },
            ModelToolEventV1::Usage {
                input_tokens: 21,
                output_tokens: 8
            }
        ]
    );
    server.join().expect("Anthropic fixture");
}

#[test]
fn anthropic_rejects_unknown_and_malformed_tool_use_without_partial_events() {
    let (origin, server) = start_fixture(1, |_index, _request| {
        FixtureResponse::json(json!({
            "role":"assistant",
            "content":[{"type":"tool_use","id":"toolu_bad","name":"shell_host","input":{}}],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":1,"output_tokens":1}
        }))
    });
    let provider = provider(&origin);
    let mut events = Vec::new();
    let error = provider
        .execute_tool_turn(&request(Vec::new()), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect_err("unfrozen tool name");
    assert!(error.to_string().contains("Anthropic tool response"));
    assert!(events.is_empty());
    server.join().expect("Anthropic malformed fixture");
}
