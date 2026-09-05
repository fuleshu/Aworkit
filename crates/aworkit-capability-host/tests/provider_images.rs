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
        let (origin, server) = start_fixture(2, move |index, request| {
            assert_eq!(request.method, "POST");
            assert!(request.headers.contains_key("content-type"));
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let messages = if kind == "gemini" {
                &body["contents"]
            } else {
                &body["messages"]
            };
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
            assert_eq!(body.get("tools").is_some(), index == 1);
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
                },
            )
            .unwrap();
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
