//! Real HTTP wire tests for image-only, multiple-image and tool-aware turns.
mod provider_tool_support;
use aworkit_capability_host::model_images::{ImageAttachmentV1, ModelImageResolver};
use aworkit_capability_host::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use provider_tool_support::{FixtureResponse, start_fixture};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
struct Images;
impl ModelImageResolver for Images {
    fn read(&self, _: &ImageAttachmentV1) -> Result<Vec<u8>, ProviderError> {
        Ok(STANDARD.decode(PNG).unwrap())
    }
}
fn attachment() -> ImageAttachmentV1 {
    let bytes = STANDARD.decode(PNG).unwrap();
    ImageAttachmentV1 {
        id: format!("{:x}", Sha256::digest(&bytes)),
        name: "red.png".into(),
        mime_type: "image/png".into(),
        byte_length: bytes.len(),
    }
}

#[test]
fn images_reach_all_protocols_in_plain_and_tool_turns() {
    for kind in ["openai", "anthropic", "gemini"] {
        let (origin, server) = start_fixture(3, move |index, request| {
            assert_eq!(request.method, "POST");
            assert!(request.headers.contains_key("content-type"));
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let messages = if kind == "gemini" {
                &body["contents"]
            } else {
                &body["messages"]
            };
            if index < 2 {
                let first = if kind == "gemini" {
                    &messages[0]["parts"]
                } else {
                    &messages[0]["content"]
                };
                let last = if kind == "gemini" {
                    &messages[2]["parts"]
                } else {
                    &messages[2]["content"]
                };
                assert_eq!(first.as_array().unwrap().len(), 2);
                assert_eq!(
                    last.as_array().unwrap().len(),
                    2,
                    "image-only last turn has two images"
                );
                for part in [first[0].clone(), last[0].clone(), last[1].clone()] {
                    match kind {
                        "openai" => {
                            assert_eq!(part["type"], "image_url");
                            assert_eq!(
                                part["image_url"]["url"],
                                format!("data:image/png;base64,{PNG}")
                            );
                        }
                        "anthropic" => {
                            assert_eq!(part["source"]["type"], "base64");
                            assert_eq!(part["source"]["media_type"], "image/png");
                            assert_eq!(part["source"]["data"], PNG);
                        }
                        _ => {
                            assert_eq!(part["inlineData"]["mimeType"], "image/png");
                            assert_eq!(part["inlineData"]["data"], PNG);
                        }
                    }
                }
            } else {
                // Every protocol must keep the call/result adjacent, then send
                // the tool image as labeled user evidence with actual bytes.
                assert_eq!(messages.as_array().unwrap().len(), 4);
                let last = &messages[3];
                assert_eq!(last["role"], "user");
                let parts = if kind == "gemini" {
                    &last["parts"]
                } else {
                    &last["content"]
                };
                let data = match kind {
                    "openai" => {
                        assert_eq!(messages[2]["role"], "tool");
                        assert_eq!(messages[2]["tool_call_id"], "image.1");
                        parts[0]["image_url"]["url"]
                            .as_str()
                            .unwrap()
                            .strip_prefix("data:image/png;base64,")
                            .unwrap()
                    }
                    "anthropic" => {
                        assert_eq!(messages[2]["content"][0]["tool_use_id"], "image.1");
                        parts[0]["source"]["data"].as_str().unwrap()
                    }
                    _ => {
                        assert_eq!(messages[2]["parts"][0]["functionResponse"]["name"], "test");
                        parts[0]["inlineData"]["data"].as_str().unwrap()
                    }
                };
                assert_eq!(data, PNG);
                assert!(parts.to_string().contains("tool evidence"));
            }
            assert_eq!(body.get("tools").is_some(), index > 0);
            match kind {
                "openai" => {
                    assert_eq!(request.path, "/v1/chat/completions");
                    FixtureResponse::sse(vec![
                        json!({"choices":[{"index":0,"delta":{"content":"Red images"},"finish_reason":null}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":4}}),
                    ])
                }
                "anthropic" => FixtureResponse::json(
                    json!({"content":[{"type":"text","text":"Red images"}],"stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":4}}),
                ),
                _ => FixtureResponse::json(
                    json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Red images"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":30,"candidatesTokenCount":4}}),
                ),
            }
        });
        let provider: Box<dyn ProviderEnginePortV1> = match kind {
            "openai" => Box::new(
                OpenAiCompatibleProvider::new(
                    OpenAiCompatibleProviderConfig::new(
                        "vision",
                        "v1",
                        format!("{origin}/v1"),
                        "vision",
                        None,
                        OpenAiCompatibleLimitsV1::default(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
            "anthropic" => Box::new(
                AnthropicMessagesProvider::new(
                    AnthropicMessagesProviderConfig::new(
                        "vision",
                        "v1",
                        &origin,
                        "vision",
                        None,
                        AnthropicMessagesLimitsV1::default(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
            _ => Box::new(
                GoogleGeminiProvider::new(
                    GoogleGeminiProviderConfig::new(
                        "vision",
                        "v1",
                        &origin,
                        "vision",
                        None,
                        GoogleGeminiLimitsV1::default(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
        };
        let gateway = FrozenModelGateway::new(vec![provider]).with_image_resolver(Arc::new(Images));
        let plan = ModelResolutionPlanV1 {
            candidates: vec![ModelCandidateV1 {
                binding_id: "vision".into(),
                version_hash: "v1".into(),
            }],
            maximum_input_bytes: 16 * 1024,
            maximum_output_bytes: 4096,
        };
        let image = attachment();
        let input = json!({"messages":[{"role":"user","content":"Remember this","images":[image]}, {"role":"assistant","content":"Seen"}, {"role":"user","content":"","images":[image,image]}]});
        gateway
            .execute(
                &plan,
                &ModelRequestV1 {
                    input: input.clone(),
                    parameters: Default::default(),
                },
            )
            .unwrap();
        gateway
            .execute_tool_turn(
                &plan,
                &ModelToolRequestV1 {
                    input,
                    parameters: Default::default(),
                    tools: vec![ModelToolDefinitionV1 {
                        capability_id: "tool.test".into(),
                        name: "test".into(),
                        description: "Test".into(),
                        input_schema: json!({"type":"object"}),
                    }],
                    exchanges: Vec::new(),
                    retry_notice: None,
                    context_messages: Vec::new(),
                },
            )
            .unwrap();
        let request = ModelToolRequestV1 {
            input: json!({"messages":[{"role":"user","content":"Read the image"}]}),
            parameters: Default::default(),
            tools: vec![ModelToolDefinitionV1 {
                capability_id: "tool.test".into(),
                name: "test".into(),
                description: "Test".into(),
                input_schema: json!({"type":"object"}),
            }],
            exchanges: vec![ModelToolExchangeV1 {
                assistant_content: vec![ModelAssistantContentV1::ToolCall {
                    call: ModelToolCallV1 {
                        call_id: "image.1".into(),
                        provider_call_id: Some("image.1".into()),
                        capability_id: "tool.test".into(),
                        name: "test".into(),
                        arguments: json!({}),
                        provider_context: None,
                    },
                }],
                results: vec![ModelToolResultV1 {
                    call_id: "image.1".into(),
                    content: json!({"image":attachment()}),
                    is_error: false,
                    images: vec![attachment()],
                }],
            }],
            context_messages: Vec::new(),
            retry_notice: None,
        };
        let canonical = request.clone();
        gateway.execute_tool_turn(&plan, &request).unwrap();
        assert_eq!(
            request, canonical,
            "provider projection must not mutate durable history"
        );
        assert!(!serde_json::to_string(&request).unwrap().contains(PNG));
        server.join().unwrap();
    }
}

#[test]
fn image_reference_validation_rejects_paths_and_oversized_metadata() {
    let mut image = attachment();
    image.id = "../private".into();
    assert!(image.validate().is_err());
    image = attachment();
    image.byte_length = usize::MAX;
    assert!(model_images::validate_image_attachments(&[image.clone(), image]).is_err());
}

/// Attached requests are bounded: the newest images carry the bytes and every
/// older image still arrives as an explicit reference. Nothing is dropped
/// silently, and a long run cannot keep uploading an unbounded image history.
#[test]
fn a_large_chat_attaches_the_newest_images_and_references_the_rest() {
    const IMAGES: usize = model_images::MAX_REQUEST_IMAGES + 8;
    let (origin, server) = start_fixture(1, move |_index, request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        let parts = messages[0]["content"].as_array().unwrap();
        let attached = parts
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count();
        let referenced = parts
            .iter()
            .filter(|part| {
                part["type"] == "text"
                    && part["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("image not attached"))
            })
            .count();
        assert_eq!(
            attached,
            model_images::MAX_REQUEST_IMAGES,
            "the newest images hold the attached budget"
        );
        assert_eq!(
            referenced,
            IMAGES - model_images::MAX_REQUEST_IMAGES,
            "every older image keeps an explicit reference"
        );
        assert_eq!(
            attached + referenced,
            IMAGES,
            "every image is represented exactly once"
        );
        FixtureResponse::sse(vec![
            json!({"choices":[{"index":0,"delta":{"content":"Understood"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":4}}),
        ])
    });
    let provider = OpenAiCompatibleProvider::new(
        OpenAiCompatibleProviderConfig::new(
            "vision",
            "v1",
            format!("{origin}/v1"),
            "vision",
            None,
            OpenAiCompatibleLimitsV1::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let gateway =
        FrozenModelGateway::new(vec![Box::new(provider)]).with_image_resolver(Arc::new(Images));
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: "vision".into(),
            version_hash: "v1".into(),
        }],
        maximum_input_bytes: 16 * 1024,
        maximum_output_bytes: 4096,
    };
    let images = vec![attachment(); IMAGES];
    let request = ModelToolRequestV1 {
        input: json!({"messages":[{"role":"user","content":"Compare these","images":images}]}),
        parameters: Default::default(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        context_messages: Vec::new(),
        retry_notice: None,
    };
    let canonical = request.clone();
    gateway.execute_tool_turn(&plan, &request).unwrap();
    assert_eq!(
        request, canonical,
        "materializing a large image set never rewrites durable history"
    );
    server.join().unwrap();
}

/// A model with no image input receives a reference for every image and no
/// image bytes at all, so the request stays text-sized instead of carrying
/// megabytes the provider would discard.
#[test]
fn a_model_without_image_input_receives_references_and_never_bytes() {
    let (origin, server) = start_fixture(1, move |_index, request| {
        let wire = String::from_utf8_lossy(&request.body).to_string();
        assert!(
            !wire.contains("data:image/png;base64"),
            "a model with no image input is never sent image bytes"
        );
        assert!(
            !wire.contains(PNG),
            "the stored image never reaches the wire"
        );
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert!(
            parts.iter().all(|part| part["type"] == "text"),
            "every part is text when no bytes are attached"
        );
        assert_eq!(
            parts
                .iter()
                .filter(|part| part["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("red.png")))
                .count(),
            1,
            "the model still learns which image exists"
        );
        FixtureResponse::sse(vec![
            json!({"choices":[{"index":0,"delta":{"content":"Noted"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":4}}),
        ])
    });
    let provider = OpenAiCompatibleProvider::new(
        OpenAiCompatibleProviderConfig::new(
            "text-only",
            "v1",
            format!("{origin}/v1"),
            "text-only",
            None,
            OpenAiCompatibleLimitsV1::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let gateway = FrozenModelGateway::new(vec![Box::new(provider)])
        .with_image_resolver(Arc::new(Images))
        .with_image_dispatch(model_images::ImageDispatchV1::Reference);
    let plan = ModelResolutionPlanV1 {
        candidates: vec![ModelCandidateV1 {
            binding_id: "text-only".into(),
            version_hash: "v1".into(),
        }],
        maximum_input_bytes: 16 * 1024,
        maximum_output_bytes: 4096,
    };
    let request = ModelToolRequestV1 {
        input: json!({"messages":[{"role":"user","content":"What was on screen?","images":[attachment()]}]}),
        parameters: Default::default(),
        tools: Vec::new(),
        exchanges: Vec::new(),
        context_messages: Vec::new(),
        retry_notice: None,
    };
    let canonical = request.clone();
    gateway.execute_tool_turn(&plan, &request).unwrap();
    assert_eq!(
        request, canonical,
        "a reference dispatch never rewrites durable history"
    );
    server.join().unwrap();
}
