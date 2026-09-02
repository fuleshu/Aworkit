#[cfg(test)]
use std::collections::BTreeMap;
use std::{sync::Arc, time::Duration};

use aworkit_capability_host::{
    AnthropicMessagesLimitsV1, AnthropicMessagesProvider, AnthropicMessagesProviderConfig,
    GoogleGeminiLimitsV1, GoogleGeminiProvider, GoogleGeminiProviderConfig,
    OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider, OpenAiCompatibleProviderConfig,
};
#[cfg(test)]
use aworkit_capability_host::{
    FrozenModelGateway, ModelCandidateV1, ModelRequestV1, ModelResolutionPlanV1,
    project_model_events,
};
#[cfg(test)]
use serde_json::{Value, json};

use super::dto::ProviderTestResult;
#[cfg(test)]
use super::history::ConversationMessage;

const OPENAI_BINDING_ID: &str = "provider.openai-compatible.primary";
const OPENAI_VERSION_HASH: &str = "openai-compatible.v1";
const ANTHROPIC_BINDING_ID: &str = "provider.anthropic.primary";
const ANTHROPIC_VERSION_HASH: &str = "anthropic-messages.v1";
const GEMINI_BINDING_ID: &str = "provider.gemini.primary";
const GEMINI_VERSION_HASH: &str = "google-gemini.v1";

#[derive(Clone, Debug)]
#[cfg(test)]
pub(crate) struct ProviderCompletion {
    pub text: String,
    pub input_units: u64,
    pub output_units: u64,
    pub model: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredProviderModel {
    pub remote_id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub capabilities: Vec<String>,
}

/// Returns the model-facing features implemented by an installed provider
/// adapter. Provider catalog APIs generally do not expose per-model tool-call
/// metadata, so discovery reports the transport contract Aworkit can actually
/// offer instead of incorrectly labelling every discovered model text-only.
pub(crate) fn installed_model_capabilities(kind: &str) -> Vec<String> {
    let mut capabilities = vec!["text".to_owned()];
    if provider_supports_tool_calls(kind) {
        capabilities.push("tools".to_owned());
    }
    capabilities
}

pub(crate) fn provider_supports_tool_calls(kind: &str) -> bool {
    matches!(kind, "openai_compatible" | "anthropic" | "gemini")
}

pub(crate) trait ProviderPort: Send + Sync {
    fn validate(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
        request_timeout: Duration,
    ) -> Result<(), String>;

    fn test_connection(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        request_timeout: Duration,
    ) -> ProviderTestResult;

    #[cfg(test)]
    fn complete(
        &self,
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        messages: &[ConversationMessage],
    ) -> Result<ProviderCompletion, String>;

    fn discover_models(
        &self,
        _kind: &str,
        _base_url: &str,
        _api_key: Option<String>,
        _request_timeout: Duration,
    ) -> Result<Vec<DiscoveredProviderModel>, String> {
        Err("this provider adapter does not implement model discovery".into())
    }
}

#[derive(Default)]
pub(crate) struct BuiltInProviderPort;

impl ProviderPort for BuiltInProviderPort {
    fn validate(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
        request_timeout: Duration,
    ) -> Result<(), String> {
        match kind {
            "openai_compatible" => {
                openai_provider(base_url, model, None, request_timeout).map(|_| ())
            }
            "anthropic" => anthropic_provider(base_url, model, None, request_timeout).map(|_| ()),
            "gemini" => gemini_provider(base_url, model, None, request_timeout).map(|_| ()),
            _ => Err(format!(
                "provider protocol '{kind}' has no installed native adapter"
            )),
        }
    }

    fn test_connection(
        &self,
        kind: &str,
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        request_timeout: Duration,
    ) -> ProviderTestResult {
        let result = match kind {
            "openai_compatible" => openai_provider(base_url, model, api_key, request_timeout)
                .and_then(|provider| {
                    provider
                        .test_connection()
                        .map(|test| (test.models, test.configured_model_available))
                        .map_err(|error| error.to_string())
                }),
            "anthropic" => {
                anthropic_provider(base_url, model, api_key, request_timeout).and_then(|provider| {
                    provider
                        .test_connection()
                        .map(|test| {
                            (
                                test.models.into_iter().map(|model| model.id).collect(),
                                test.configured_model_available,
                            )
                        })
                        .map_err(|error| error.to_string())
                })
            }
            "gemini" => {
                gemini_provider(base_url, model, api_key, request_timeout).and_then(|provider| {
                    provider
                        .test_connection()
                        .map(|test| {
                            (
                                test.models.into_iter().map(|model| model.id).collect(),
                                test.configured_model_available,
                            )
                        })
                        .map_err(|error| error.to_string())
                })
            }
            _ => Err(format!(
                "provider protocol '{kind}' has no installed native adapter"
            )),
        };
        match result {
            Ok((_models, true)) => ProviderTestResult {
                ok: true,
                message: format!("Connected. Model '{model}' is available."),
                model: Some(model.to_owned()),
            },
            Ok((models, false)) => ProviderTestResult {
                ok: false,
                message: if models.is_empty() {
                    "Connected, but the endpoint returned no usable models.".into()
                } else {
                    format!(
                        "Connected, but model '{model}' was not advertised. Available: {}",
                        models.join(", ")
                    )
                },
                model: None,
            },
            Err(error) => ProviderTestResult {
                ok: false,
                message: format!("Connection test failed: {error}"),
                model: None,
            },
        }
    }

    #[cfg(test)]
    fn complete(
        &self,
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        messages: &[ConversationMessage],
    ) -> Result<ProviderCompletion, String> {
        if messages.is_empty() {
            return Err("Chat requires at least one message".into());
        }
        let provider = openai_provider(
            base_url,
            model,
            api_key,
            OpenAiCompatibleLimitsV1::default().request_timeout,
        )?;
        let gateway = FrozenModelGateway::new(vec![Box::new(provider)]);
        let input = Value::Array(
            messages
                .iter()
                .map(|message| json!({"role":message.role,"content":message.content}))
                .collect(),
        );
        let evidence = gateway
            .execute(
                &ModelResolutionPlanV1 {
                    candidates: vec![ModelCandidateV1 {
                        binding_id: OPENAI_BINDING_ID.into(),
                        version_hash: OPENAI_VERSION_HASH.into(),
                    }],
                    maximum_input_bytes: 1024 * 1024,
                    maximum_output_bytes: 1024 * 1024,
                },
                &ModelRequestV1 {
                    input,
                    parameters: BTreeMap::new(),
                },
            )
            .map_err(|error| format!("provider completion failed: {error}"))?;
        let turn = project_model_events(&evidence.events);
        let text = turn.assistant_text;
        if text.trim().is_empty() {
            return Err("provider returned an empty assistant response".into());
        }
        let input_units = turn.input_tokens;
        let output_units = turn.output_tokens;
        Ok(ProviderCompletion {
            text,
            input_units,
            output_units,
            model: model.to_owned(),
        })
    }

    fn discover_models(
        &self,
        kind: &str,
        base_url: &str,
        api_key: Option<String>,
        request_timeout: Duration,
    ) -> Result<Vec<DiscoveredProviderModel>, String> {
        match kind {
            "openai_compatible" => {
                let connection =
                    openai_provider(base_url, "aworkit-discovery", api_key, request_timeout)?
                        .test_connection()
                        .map_err(|error| format!("model discovery failed: {error}"))?;
                Ok(connection
                    .model_details
                    .into_iter()
                    .map(|model| {
                        let mut capabilities = installed_model_capabilities(kind);
                        capabilities.extend(model.capabilities);
                        capabilities.sort();
                        capabilities.dedup();
                        DiscoveredProviderModel {
                            name: model.id.clone(),
                            remote_id: model.id,
                            context_window: model.context_window,
                            max_output_tokens: model.max_output_tokens,
                            capabilities,
                        }
                    })
                    .collect())
            }
            "anthropic" => {
                let connection =
                    anthropic_provider(base_url, "aworkit-discovery", api_key, request_timeout)?
                        .test_connection()
                        .map_err(|error| format!("model discovery failed: {error}"))?;
                Ok(connection
                    .models
                    .into_iter()
                    .map(|model| DiscoveredProviderModel {
                        remote_id: model.id,
                        name: model.name,
                        context_window: None,
                        max_output_tokens: None,
                        capabilities: installed_model_capabilities(kind),
                    })
                    .collect())
            }
            "gemini" => {
                let connection =
                    gemini_provider(base_url, "aworkit-discovery", api_key, request_timeout)?
                        .test_connection()
                        .map_err(|error| format!("model discovery failed: {error}"))?;
                Ok(connection
                    .models
                    .into_iter()
                    .map(|model| DiscoveredProviderModel {
                        remote_id: model.id,
                        name: model.name,
                        context_window: model.input_token_limit,
                        max_output_tokens: model.output_token_limit,
                        capabilities: installed_model_capabilities(kind),
                    })
                    .collect())
            }
            _ => Err(format!(
                "provider protocol '{kind}' has no installed native adapter"
            )),
        }
    }
}

pub(crate) fn production_provider() -> Arc<dyn ProviderPort> {
    Arc::new(BuiltInProviderPort)
}

fn openai_provider(
    base_url: &str,
    model: &str,
    api_key: Option<String>,
    request_timeout: Duration,
) -> Result<OpenAiCompatibleProvider, String> {
    let config = OpenAiCompatibleProviderConfig::new(
        OPENAI_BINDING_ID,
        OPENAI_VERSION_HASH,
        base_url,
        model,
        api_key,
        OpenAiCompatibleLimitsV1 {
            request_timeout,
            ..OpenAiCompatibleLimitsV1::default()
        },
    )
    .map_err(|error| error.to_string())?;
    OpenAiCompatibleProvider::new(config).map_err(|error| error.to_string())
}

fn anthropic_provider(
    base_url: &str,
    model: &str,
    api_key: Option<String>,
    request_timeout: Duration,
) -> Result<AnthropicMessagesProvider, String> {
    let config = AnthropicMessagesProviderConfig::new(
        ANTHROPIC_BINDING_ID,
        ANTHROPIC_VERSION_HASH,
        base_url,
        model,
        api_key,
        AnthropicMessagesLimitsV1 {
            request_timeout,
            ..AnthropicMessagesLimitsV1::default()
        },
    )
    .map_err(|error| error.to_string())?;
    AnthropicMessagesProvider::new(config).map_err(|error| error.to_string())
}

fn gemini_provider(
    base_url: &str,
    model: &str,
    api_key: Option<String>,
    request_timeout: Duration,
) -> Result<GoogleGeminiProvider, String> {
    let config = GoogleGeminiProviderConfig::new(
        GEMINI_BINDING_ID,
        GEMINI_VERSION_HASH,
        base_url,
        model,
        api_key,
        GoogleGeminiLimitsV1 {
            request_timeout,
            ..GoogleGeminiLimitsV1::default()
        },
    )
    .map_err(|error| error.to_string())?;
    GoogleGeminiProvider::new(config).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        thread::{self, JoinHandle},
        time::Duration,
    };

    use serde_json::{Value, json};

    use super::*;

    fn catalog_fixture(
        expected_path: &'static str,
        expected_authorization: &'static str,
        response: Value,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider fixture");
        let address = listener.local_addr().expect("provider fixture address");
        let body = serde_json::to_vec(&response).expect("provider fixture JSON");
        let handle = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("provider fixture connection");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("provider fixture timeout");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).expect("provider fixture request");
                    assert_ne!(read, 0, "request ended before headers");
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).expect("provider fixture headers");
                let request_line = request.lines().next().expect("provider request line");
                assert_eq!(request_line, format!("GET {expected_path} HTTP/1.1"));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains(&expected_authorization.to_ascii_lowercase()),
                    "provider request did not use the protocol-specific API-key header"
                );
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .expect("provider fixture response headers");
                stream
                    .write_all(&body)
                    .expect("provider fixture response body");
            }
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn provider_probe_and_discovery_dispatch_all_installed_protocols() {
        let cases = [
            (
                "openai_compatible",
                "/v1/models",
                "/v1",
                "authorization: bearer desktop-secret",
                json!({"data":[{
                    "id":"fixture-model",
                    "max_model_len":65536,
                    "supported_parameters":["reasoning_effort","enable_thinking"],
                    "reasoning_efforts":["low","medium","high"]
                }]}),
                "fixture-model",
            ),
            (
                "anthropic",
                "/v1/models?limit=100",
                "",
                "x-api-key: desktop-secret",
                json!({"data":[{"id":"fixture-model","display_name":"Fixture Anthropic"}]}),
                "Fixture Anthropic",
            ),
            (
                "gemini",
                "/v1beta/models?pageSize=100",
                "",
                "x-goog-api-key: desktop-secret",
                json!({"models":[{
                    "name":"models/fixture-model",
                    "displayName":"Fixture Gemini",
                    "inputTokenLimit":32768,
                    "outputTokenLimit":4096,
                    "supportedGenerationMethods":["generateContent"]
                }]}),
                "Fixture Gemini",
            ),
        ];

        for (kind, path, base_suffix, authorization, response, expected_name) in cases {
            let (origin, server) = catalog_fixture(path, authorization, response);
            let base_url = format!("{origin}{base_suffix}");
            let port = BuiltInProviderPort;
            port.validate(kind, &base_url, "fixture-model", Duration::from_secs(42))
                .expect("valid provider draft");
            let probe = port.test_connection(
                kind,
                &base_url,
                "fixture-model",
                Some("desktop-secret".to_owned()),
                Duration::from_secs(42),
            );
            assert!(probe.ok, "{kind} probe failed: {}", probe.message);
            let discovered = port
                .discover_models(
                    kind,
                    &base_url,
                    Some("desktop-secret".to_owned()),
                    Duration::from_secs(42),
                )
                .expect("provider discovery");
            assert_eq!(discovered.len(), 1);
            assert_eq!(discovered[0].remote_id, "fixture-model");
            assert_eq!(discovered[0].name, expected_name);
            if kind == "openai_compatible" {
                assert_eq!(discovered[0].context_window, Some(65_536));
                assert_eq!(
                    discovered[0].capabilities,
                    vec![
                        "reasoning",
                        "reasoning_effort:high",
                        "reasoning_effort:low",
                        "reasoning_effort:medium",
                        "text",
                        "thinking_toggle",
                        "tools",
                    ]
                );
            } else {
                assert_eq!(discovered[0].capabilities, vec!["text", "tools"]);
            }
            if kind == "gemini" {
                assert_eq!(discovered[0].context_window, Some(32_768));
                assert_eq!(discovered[0].max_output_tokens, Some(4_096));
            }
            server.join().expect("provider fixture");
        }
        assert!(!provider_supports_tool_calls("uninstalled"));
    }
}
