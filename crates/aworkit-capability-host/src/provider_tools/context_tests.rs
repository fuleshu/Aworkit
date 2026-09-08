//! Context must stay between complete exchanges on every provider wire.
use super::{
    anthropic::anthropic_tool_request,
    gemini::gemini_tool_request,
    openai::{OpenAiRequestParametersV1, openai_tool_request},
};
use crate::{
    ModelAssistantContentV1, ModelToolCallV1, ModelToolContextV1, ModelToolDefinitionV1,
    ModelToolExchangeV1, ModelToolRequestV1, ModelToolResultV1,
};
use serde_json::json;

#[test]
fn historical_instruction_references_keep_position_without_callable_schemas() {
    let request = ModelToolRequestV1 {
        input: json!({"messages":[
            {"role":"user","content":"first"},{"role":"assistant","content":"answer"},{"role":"user","content":"second"}
        ]}),
        parameters: Default::default(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        retry_notice: None,
        context_messages: vec![ModelToolContextV1 {
            after_input_messages: Some(1),
            instruction_event_id: Some("event.instructions".into()),
            content: "<system-reminder>\nOriginal guidance\n</system-reminder>".into(),
            ..Default::default()
        }],
    };
    request.validate().unwrap();
    crate::model_tools::validate_tool_request(&request).unwrap();
    for body in [
        openai_tool_request("fixture", &request, &OpenAiRequestParametersV1::default()).unwrap(),
        anthropic_tool_request("fixture", 100, &request).unwrap(),
    ] {
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert_eq!(
            body["messages"][1]["content"],
            request.context_messages[0].content
        );
        assert_eq!(body["messages"][2]["content"], "answer");
        assert_eq!(body["messages"].as_array().unwrap().len(), 4);
        assert!(!body.to_string().contains("event.instructions"));
    }
    let gemini = gemini_tool_request(&request).unwrap();
    assert!(gemini.get("tools").is_none());
    assert_eq!(
        gemini["contents"][1]["parts"][0]["text"],
        request.context_messages[0].content
    );
    assert_eq!(gemini["contents"][2]["role"], "model");
}

#[test]
fn durable_catalog_and_invocation_context_keep_append_only_provider_order() {
    let call = ModelToolCallV1 {
        call_id: "call.1".into(),
        provider_call_id: Some("call.1".into()),
        capability_id: "tool.skill".into(),
        name: "skill".into(),
        arguments: json!({"name":"test"}),
        provider_context: None,
    };
    let request = ModelToolRequestV1 {
        input: json!({"messages":[{"role":"user","content":"direct input"}]}),
        parameters: Default::default(),
        retry_notice: None,
        tools: vec![ModelToolDefinitionV1 {
            capability_id: "tool.skill".into(),
            name: "skill".into(),
            description: "Load skill".into(),
            input_schema: json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
        }],
        exchanges: vec![ModelToolExchangeV1 {
            assistant_content: vec![ModelAssistantContentV1::ToolCall { call }],
            results: vec![ModelToolResultV1 {
                call_id: "call.1".into(),
                content: json!("loaded body"),
                is_error: false,
            }],
        }],
        context_messages: vec![
            ModelToolContextV1 {
                after_exchanges: 0,
                content: "initial catalog".into(),
                ..Default::default()
            },
            ModelToolContextV1 {
                after_exchanges: 0,
                content: "explicit invocation".into(),
                ..Default::default()
            },
            ModelToolContextV1 {
                after_exchanges: 1,
                content: "replacement catalog".into(),
                ..Default::default()
            },
        ],
    };
    crate::model_tools::validate_tool_request(&request).unwrap();
    let openai =
        openai_tool_request("fixture", &request, &OpenAiRequestParametersV1::default()).unwrap();
    let anthropic = anthropic_tool_request("fixture", 100, &request).unwrap();
    let gemini = gemini_tool_request(&request).unwrap();
    for body in [openai, anthropic] {
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[1]["content"], "initial catalog");
        assert_eq!(messages[2]["content"], "explicit invocation");
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[5]["content"], "replacement catalog");
    }
    assert_eq!(gemini["contents"][1]["parts"][0]["text"], "initial catalog");
    assert_eq!(
        gemini["contents"][2]["parts"][0]["text"],
        "explicit invocation"
    );
    assert_eq!(gemini["contents"][3]["role"], "model");
    assert_eq!(
        gemini["contents"][5]["parts"][0]["text"],
        "replacement catalog"
    );
    let mut error_request = request.clone();
    error_request.exchanges[0].results[0].is_error = true;
    error_request.exchanges[0].results[0].content =
        json!("Error: skill \"missing\" is unknown or no longer available");
    let openai = openai_tool_request(
        "fixture",
        &error_request,
        &OpenAiRequestParametersV1::default(),
    )
    .unwrap();
    assert_eq!(
        openai["messages"][4]["content"],
        error_request.exchanges[0].results[0].content
    );
    let anthropic = anthropic_tool_request("fixture", 100, &error_request).unwrap();
    assert_eq!(
        anthropic["messages"][4]["content"][0]["content"],
        error_request.exchanges[0].results[0].content
    );
    assert_eq!(anthropic["messages"][4]["content"][0]["is_error"], true);
    let mut invalid = request;
    invalid.context_messages[0].after_exchanges = 2;
    assert!(crate::model_tools::validate_tool_request(&invalid).is_err());
}

#[test]
fn edited_context_keeps_assistant_user_and_image_order_for_every_provider() {
    use base64::Engine;
    use sha2::Digest;
    let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap();
    let request: crate::ModelToolRequestV1 = serde_json::from_value(json!({
        "input":{"messages":[{"role":"user","content":"Original question"}]},
        "parameters":{}, "tools":[], "exchanges":[],
        "contextMessages":[
            {"afterExchanges":0,"role":"assistant","content":"Edited answer"},
            {"afterExchanges":0,"role":"user","content":"New image question","images":[{
                "id":format!("{:x}",sha2::Sha256::digest(&bytes)),"name":"test.png","mimeType":"image/png","byteLength":bytes.len(),"data":data
            }]}
        ]
    })).unwrap();
    request.validate().unwrap();
    let openai =
        openai_tool_request("fixture", &request, &OpenAiRequestParametersV1::default()).unwrap();
    assert_eq!(openai["messages"][1]["role"], "assistant");
    assert_eq!(openai["messages"][1]["content"], "Edited answer");
    assert_eq!(openai["messages"][2]["role"], "user");
    assert!(
        openai["messages"][2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image_url")
    );
    let anthropic = anthropic_tool_request("fixture", 100, &request).unwrap();
    assert_eq!(anthropic["messages"][1]["role"], "assistant");
    assert_eq!(anthropic["messages"][2]["role"], "user");
    assert!(
        anthropic["messages"][2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "image")
    );
    let gemini = gemini_tool_request(&request).unwrap();
    assert_eq!(gemini["contents"][1]["role"], "model");
    assert_eq!(gemini["contents"][1]["parts"][0]["text"], "Edited answer");
    assert_eq!(gemini["contents"][2]["role"], "user");
    assert!(
        gemini["contents"][2]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part.get("inlineData").is_some())
    );
}
