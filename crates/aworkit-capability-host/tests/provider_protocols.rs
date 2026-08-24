use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use aworkit_capability_host::{
    AnthropicMessagesLimitsV1, AnthropicMessagesProvider, AnthropicMessagesProviderConfig,
    AnthropicMessagesProviderError, FrozenModelGateway, GoogleGeminiLimitsV1, GoogleGeminiProvider,
    GoogleGeminiProviderConfig, GoogleGeminiProviderError, ModelCandidateV1, ModelEventV1,
    ModelRequestV1, ModelResolutionPlanV1, OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider,
    OpenAiCompatibleProviderConfig, ProviderEnginePortV1,
};
use serde_json::{Value, json};

struct FixtureRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct FixtureResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
}

fn start_fixture(
    requests: usize,
    handler: impl Fn(FixtureRequest) -> FixtureResponse + Send + Sync + 'static,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let handler = Arc::new(handler);
    let handle = thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            let request = read_request(&mut stream);
            let response = handler(request);
            if !response.delay.is_zero() {
                thread::sleep(response.delay);
            }
            let reason = if response.status < 300 {
                "OK"
            } else {
                "Response"
            };
            let mut header = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                response.status,
                reason,
                response.body.len()
            );
            for (name, value) in response.headers {
                header.push_str(&format!("{name}: {value}\r\n"));
            }
            header.push_str("\r\n");
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&response.body);
        }
    });
    (format!("http://{address}"), handle)
}

fn read_request(stream: &mut TcpStream) -> FixtureRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("fixture request");
        assert_ne!(read, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).expect("request headers");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("request line");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().expect("method").to_owned();
    let path = request_parts.next().expect("path").to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let content_length = headers
        .get("content-length")
        .map_or(0, |value| value.parse::<usize>().expect("content length"));
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).expect("fixture body");
        assert_ne!(read, 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    FixtureRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn response(status: u16, body: Value) -> FixtureResponse {
    FixtureResponse {
        status,
        headers: Vec::new(),
        body: serde_json::to_vec(&body).expect("fixture JSON"),
        delay: Duration::ZERO,
    }
}

fn execute(
    provider: Box<dyn ProviderEnginePortV1>,
    binding_id: &str,
    version_hash: &str,
) -> Vec<ModelEventV1> {
    FrozenModelGateway::new(vec![provider])
        .execute(
            &ModelResolutionPlanV1 {
                candidates: vec![ModelCandidateV1 {
                    binding_id: binding_id.to_owned(),
                    version_hash: version_hash.to_owned(),
                }],
                maximum_input_bytes: 1024 * 1024,
                maximum_output_bytes: 1024 * 1024,
            },
            &ModelRequestV1 {
                input: json!({
                    "messages": [
                        {"role":"system","content":"be concise"},
                        {"role":"user","content":"hello"}
                    ]
                }),
            },
        )
        .expect("provider completion")
        .events
}

#[test]
fn openai_anthropic_and_gemini_discover_and_complete_with_exact_usage() {
    let (openai_origin, openai_server) = start_fixture(2, |request| {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer openai-secret")
        );
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/v1/models") => response(200, json!({"data":[{"id":"gpt-fixture"}]})),
            ("POST", "/v1/chat/completions") => {
                let body: Value = serde_json::from_slice(&request.body).expect("OpenAI body");
                assert_eq!(body["model"], "gpt-fixture");
                assert_eq!(body["stream"], false);
                response(
                    200,
                    json!({
                        "choices":[{"message":{"content":"hello openai"}}],
                        "usage":{"prompt_tokens":7,"completion_tokens":3}
                    }),
                )
            }
            other => panic!("unexpected OpenAI request: {other:?}"),
        }
    });
    let openai_config = OpenAiCompatibleProviderConfig::new(
        "binding.openai",
        "version.openai",
        format!("{openai_origin}/v1"),
        "gpt-fixture",
        Some("openai-secret".to_owned()),
        OpenAiCompatibleLimitsV1::default(),
    )
    .expect("OpenAI config");
    let openai = OpenAiCompatibleProvider::new(openai_config).expect("OpenAI provider");
    assert!(
        openai
            .test_connection()
            .expect("OpenAI discovery")
            .configured_model_available
    );
    assert_eq!(
        execute(Box::new(openai), "binding.openai", "version.openai"),
        vec![
            ModelEventV1::AssistantOutput("hello openai".to_owned()),
            ModelEventV1::Usage {
                input_tokens: 7,
                output_tokens: 3
            }
        ]
    );
    openai_server.join().expect("OpenAI fixture");

    let (anthropic_origin, anthropic_server) = start_fixture(2, |request| {
        assert_eq!(
            request.headers.get("x-api-key").map(String::as_str),
            Some("anthropic-secret")
        );
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
        if request.method == "GET" {
            assert_eq!(request.path, "/v1/models?limit=100");
            return response(
                200,
                json!({"data":[{"id":"claude-fixture","display_name":"Claude Fixture"}]}),
            );
        }
        assert_eq!(request.path, "/v1/messages");
        let body: Value = serde_json::from_slice(&request.body).expect("Anthropic body");
        assert_eq!(body["model"], "claude-fixture");
        assert_eq!(body["system"], "be concise");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["stream"], false);
        response(
            200,
            json!({
                "content":[{"type":"text","text":"hello anthropic"}],
                "usage":{
                    "input_tokens":11,
                    "cache_creation_input_tokens":2,
                    "cache_read_input_tokens":3,
                    "output_tokens":4
                }
            }),
        )
    });
    let anthropic_config = AnthropicMessagesProviderConfig::new(
        "binding.anthropic",
        "version.anthropic",
        &anthropic_origin,
        "claude-fixture",
        Some("anthropic-secret".to_owned()),
        AnthropicMessagesLimitsV1::default(),
    )
    .expect("Anthropic config");
    let anthropic = AnthropicMessagesProvider::new(anthropic_config).expect("Anthropic provider");
    let discovery = anthropic.test_connection().expect("Anthropic discovery");
    assert!(discovery.configured_model_available);
    assert_eq!(discovery.models[0].name, "Claude Fixture");
    assert_eq!(
        execute(
            Box::new(anthropic),
            "binding.anthropic",
            "version.anthropic"
        ),
        vec![
            ModelEventV1::AssistantOutput("hello anthropic".to_owned()),
            ModelEventV1::Usage {
                input_tokens: 16,
                output_tokens: 4
            }
        ]
    );
    anthropic_server.join().expect("Anthropic fixture");

    let (gemini_origin, gemini_server) = start_fixture(2, |request| {
        assert_eq!(
            request.headers.get("x-goog-api-key").map(String::as_str),
            Some("gemini-secret")
        );
        if request.method == "GET" {
            assert_eq!(request.path, "/v1beta/models?pageSize=100");
            return response(
                200,
                json!({"models":[{
                    "name":"models/gemini-fixture",
                    "displayName":"Gemini Fixture",
                    "inputTokenLimit":32768,
                    "outputTokenLimit":4096,
                    "supportedGenerationMethods":["generateContent"]
                }]}),
            );
        }
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-fixture:generateContent"
        );
        let body: Value = serde_json::from_slice(&request.body).expect("Gemini body");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be concise");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["generationConfig"]["candidateCount"], 1);
        response(
            200,
            json!({
                "candidates":[{"content":{"role":"model","parts":[{"text":"hello gemini"}]}}],
                "usageMetadata":{
                    "promptTokenCount":12,
                    "candidatesTokenCount":5,
                    "thoughtsTokenCount":7,
                    "totalTokenCount":24
                }
            }),
        )
    });
    let gemini_config = GoogleGeminiProviderConfig::new(
        "binding.gemini",
        "version.gemini",
        &gemini_origin,
        "gemini-fixture",
        Some("gemini-secret".to_owned()),
        GoogleGeminiLimitsV1::default(),
    )
    .expect("Gemini config");
    let gemini = GoogleGeminiProvider::new(gemini_config).expect("Gemini provider");
    let discovery = gemini.test_connection().expect("Gemini discovery");
    assert!(discovery.configured_model_available);
    assert_eq!(discovery.models[0].input_token_limit, Some(32768));
    assert_eq!(
        execute(Box::new(gemini), "binding.gemini", "version.gemini"),
        vec![
            ModelEventV1::AssistantOutput("hello gemini".to_owned()),
            ModelEventV1::Usage {
                input_tokens: 12,
                output_tokens: 12
            }
        ]
    );
    gemini_server.join().expect("Gemini fixture");
}

#[test]
fn new_provider_adapters_redact_keys_deny_redirects_and_enforce_bounds() {
    let anthropic_secret = "anthropic-never-log";
    let (anthropic_origin, anthropic_server) = start_fixture(1, |_request| FixtureResponse {
        status: 302,
        headers: vec![("Location".to_owned(), "/v1/elsewhere".to_owned())],
        body: Vec::new(),
        delay: Duration::ZERO,
    });
    let anthropic_config = AnthropicMessagesProviderConfig::new(
        "binding.anthropic",
        "version.anthropic",
        format!("{anthropic_origin}/v1"),
        "claude-fixture",
        Some(anthropic_secret.to_owned()),
        AnthropicMessagesLimitsV1::default(),
    )
    .expect("Anthropic config");
    assert!(!format!("{anthropic_config:?}").contains(anthropic_secret));
    let anthropic = AnthropicMessagesProvider::new(anthropic_config).expect("Anthropic provider");
    assert_eq!(
        anthropic.test_connection(),
        Err(AnthropicMessagesProviderError::HttpStatus(302))
    );
    anthropic_server.join().expect("redirect fixture");

    let gemini_secret = "gemini-never-log";
    let (gemini_origin, gemini_server) = start_fixture(1, |_request| FixtureResponse {
        status: 200,
        headers: Vec::new(),
        body: vec![b'x'; 65],
        delay: Duration::ZERO,
    });
    let gemini_config = GoogleGeminiProviderConfig::new(
        "binding.gemini",
        "version.gemini",
        format!("{gemini_origin}/v1beta"),
        "gemini-fixture",
        Some(gemini_secret.to_owned()),
        GoogleGeminiLimitsV1 {
            maximum_response_bytes: 64,
            ..GoogleGeminiLimitsV1::default()
        },
    )
    .expect("Gemini config");
    assert!(!format!("{gemini_config:?}").contains(gemini_secret));
    let gemini = GoogleGeminiProvider::new(gemini_config).expect("Gemini provider");
    assert_eq!(
        gemini.test_connection(),
        Err(GoogleGeminiProviderError::ResponseTooLarge)
    );
    gemini_server.join().expect("bound fixture");
}
