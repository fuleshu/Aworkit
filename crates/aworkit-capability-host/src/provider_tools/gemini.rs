//! Gemini `generateContent` function-call wire mapping.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::{
    ModelAssistantContentV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
    ProviderError,
    model_tools::{ModelInputRoleV1, normalize_model_input, normalize_tool_call},
};

/// Closed, non-secret subset of model Settings and workflow-node overrides the
/// Gemini `generateContent` adapter consumes.
///
/// `reasoningEffort` and `enableThinking` are provider-neutral reasoning
/// controls; this adapter translates them into `generationConfig.thinkingConfig`
/// instead of the OpenAI-compatible `reasoning_effort` fields.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GeminiRequestParametersV1 {
    max_output_tokens: Option<u64>,
    reasoning_effort: Option<ReasoningEffortV1>,
    enable_thinking: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReasoningEffortV1 {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffortV1 {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            _ => return Option::None,
        })
    }
}

impl GeminiRequestParametersV1 {
    pub(crate) fn from_parameters(parameters: &BTreeMap<String, Value>) -> Result<Self, ()> {
        if parameters.keys().any(|key| {
            !matches!(
                key.as_str(),
                "reasoningEffort" | "enableThinking" | "maxOutputTokens"
            )
        }) {
            return Err(());
        }
        let reasoning_effort = parameters
            .get("reasoningEffort")
            .map(|value| value.as_str().and_then(ReasoningEffortV1::parse).ok_or(()))
            .transpose()?;
        let enable_thinking = parameters
            .get("enableThinking")
            .map(|value| value.as_bool().ok_or(()))
            .transpose()?;
        let max_output_tokens = parameters
            .get("maxOutputTokens")
            .map(|value| {
                value
                    .as_u64()
                    .filter(|tokens| *tokens > 0 && *tokens <= 1_048_576)
                    .ok_or(())
            })
            .transpose()?;
        Ok(Self {
            max_output_tokens,
            reasoning_effort,
            enable_thinking,
        })
    }

    pub(crate) fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// The thinking control this request asks for, or `None` when it leaves the
    /// concrete model default in place.
    ///
    /// An explicit `enableThinking: false` wins over a requested effort, because
    /// it is the only setting that asks for less reasoning than the default.
    /// `enableThinking: true` without an effort asks for a deliberate but not
    /// maximal level, so a model that thinks nothing by default still thinks.
    pub(crate) fn thinking_config(&self, model: &str) -> Option<Value> {
        let profile = GeminiThinkingProfileV1::for_model(model);
        if self.enable_thinking == Some(false) {
            return Some(profile.lowest());
        }
        let effort = match self.reasoning_effort {
            Some(effort) => effort,
            None if self.enable_thinking == Some(true) => ReasoningEffortV1::Medium,
            None => return Option::None,
        };
        Some(profile.for_effort(effort))
    }

    /// Applies the consumed fields to a `generationConfig` object.
    pub(crate) fn apply(&self, generation_config: &mut Value, model: &str) {
        let Some(generation_config) = generation_config.as_object_mut() else {
            return;
        };
        if let Some(cap) = self.max_output_tokens {
            generation_config.insert("maxOutputTokens".into(), json!(cap));
        }
        if let Some(thinking) = self.thinking_config(model) {
            generation_config.insert("thinkingConfig".into(), thinking);
        }
    }
}

/// How one Gemini model family expresses reasoning depth.
///
/// Gemini 3 and later reject `thinkingBudget` and take a coarse `thinkingLevel`;
/// earlier models take an explicit token budget. A model name outside the
/// `gemini-<major>` shape (a proxy-served alias) is treated as budget-based,
/// which is the form every pre-3 model accepted.
enum GeminiThinkingProfileV1 {
    /// `medium` is withheld from `pro` models: the first Gemini 3 Pro accepts
    /// only `low` and `high`, so an unsupported level would fail the request.
    Levels {
        supports_xhigh: bool,
        supports_medium: bool,
    },
    Budget {
        pro: bool,
    },
}

impl GeminiThinkingProfileV1 {
    fn for_model(model: &str) -> Self {
        let name = model.rsplit('/').next().unwrap_or(model);
        let major = name.strip_prefix("gemini-").and_then(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .ok()
        });
        match major {
            Some(major) if major >= 3 => Self::Levels {
                supports_xhigh: major >= 4,
                supports_medium: !name.contains("pro"),
            },
            _ => Self::Budget {
                pro: name.contains("pro"),
            },
        }
    }

    /// The least reasoning the model can be asked for. `MINIMAL` is not offered
    /// by every level model, and a 2.5 Pro cannot switch thinking off entirely,
    /// so each family gets the lowest setting it documents.
    fn lowest(&self) -> Value {
        match self {
            Self::Levels { .. } => json!({"thinkingLevel":"LOW"}),
            Self::Budget { pro: true } => json!({"thinkingBudget":128}),
            Self::Budget { pro: false } => json!({"thinkingBudget":0}),
        }
    }

    fn for_effort(&self, effort: ReasoningEffortV1) -> Value {
        match self {
            Self::Levels {
                supports_xhigh,
                supports_medium,
            } => {
                let level = match effort {
                    ReasoningEffortV1::None
                    | ReasoningEffortV1::Minimal
                    | ReasoningEffortV1::Low => "LOW",
                    ReasoningEffortV1::Medium if *supports_medium => "MEDIUM",
                    ReasoningEffortV1::Medium | ReasoningEffortV1::High => "HIGH",
                    ReasoningEffortV1::XHigh | ReasoningEffortV1::Max if *supports_xhigh => "XHIGH",
                    ReasoningEffortV1::XHigh | ReasoningEffortV1::Max => "HIGH",
                };
                json!({"thinkingLevel": level})
            }
            Self::Budget { pro } => {
                // Documented ranges: Pro 128..=32768, Flash and Flash-Lite
                // 0..=24576 (Lite from 512). `-1` asks the model to decide
                // dynamically and is accepted by every budget model.
                let budget = match effort {
                    ReasoningEffortV1::None if *pro => 128,
                    ReasoningEffortV1::None => 0,
                    ReasoningEffortV1::Minimal => 512,
                    ReasoningEffortV1::Low => 1_024,
                    ReasoningEffortV1::Medium => 8_192,
                    ReasoningEffortV1::High => 16_384,
                    ReasoningEffortV1::XHigh => 24_576,
                    ReasoningEffortV1::Max => -1,
                };
                json!({"thinkingBudget": budget})
            }
        }
    }
}

pub(crate) fn gemini_tool_request(
    request: &ModelToolRequestV1,
    model: &str,
) -> Result<Value, ProviderError> {
    let base = normalize_model_input(&request.projected_input()?)?;
    let system_parts = base
        .iter()
        .filter(|message| message.role == ModelInputRoleV1::System)
        .map(|message| json!({"text":message.content}))
        .collect::<Vec<_>>();
    let mut contents = base
        .into_iter()
        .filter(|message| message.role != ModelInputRoleV1::System)
        .map(|message| {
            Ok(json!({
                "role": match message.role {
                    ModelInputRoleV1::User => "user",
                    ModelInputRoleV1::Assistant => "model",
                    ModelInputRoleV1::System => unreachable!("system messages were filtered"),
                },
                "parts": crate::model_images::image_content(&message.content, &message.images, "gemini")?,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;

    for (index, exchange) in request.exchanges.iter().enumerate() {
        for context in request
            .context_messages
            .iter()
            .filter(|c| c.after_input_messages.is_none() && c.after_exchanges == index)
        {
            contents.push(super::context_message(context, "gemini")?);
        }
        let model_parts = exchange
            .assistant_content
            .iter()
            .map(|content| match content {
                ModelAssistantContentV1::Text { text } => Ok(Some(json!({"text":text}))),
                ModelAssistantContentV1::ToolCall { call } => {
                    if call
                        .provider_call_id
                        .as_deref()
                        .is_some_and(|id| id != call.call_id)
                    {
                        return Err(invalid_request());
                    }
                    let mut function = Map::new();
                    if let Some(id) = &call.provider_call_id {
                        function.insert("id".to_owned(), Value::String(id.clone()));
                    }
                    function.insert("name".to_owned(), Value::String(call.name.clone()));
                    function.insert("args".to_owned(), call.arguments.clone());
                    let mut part = Map::new();
                    part.insert("functionCall".to_owned(), Value::Object(function));
                    if let Some(context) = &call.provider_context {
                        part.insert(
                            "thoughtSignature".to_owned(),
                            Value::String(context.as_str().to_owned()),
                        );
                    }
                    Ok(Some(Value::Object(part)))
                }
                // Retained chain of thought is DeepSeek-specific text. This
                // protocol carries thoughts only as signature-bearing parts,
                // which a replay cannot forge, so the part is not sent.
                ModelAssistantContentV1::Reasoning { .. } => Ok(None),
            })
            .collect::<Result<Vec<_>, ProviderError>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        contents.push(json!({"role":"model","parts":model_parts}));

        let result_parts = exchange
            .results
            .iter()
            .map(|result| {
                let call = exchange
                    .assistant_content
                    .iter()
                    .find_map(|content| match content {
                        ModelAssistantContentV1::ToolCall { call }
                            if call.call_id == result.call_id =>
                        {
                            Some(call)
                        }
                        _ => None,
                    })
                    .ok_or_else(invalid_request)?;
                let mut function = Map::new();
                if let Some(id) = &call.provider_call_id {
                    function.insert("id".to_owned(), Value::String(id.clone()));
                }
                function.insert("name".to_owned(), Value::String(call.name.clone()));
                function.insert(
                    "response".to_owned(),
                    if result.is_error {
                        json!({"error":result.content})
                    } else {
                        json!({"output":result.content})
                    },
                );
                Ok(json!({"functionResponse":Value::Object(function)}))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        if !result_parts.is_empty() {
            contents.push(json!({"role":"user","parts":result_parts}));
        }
    }
    for context in request.context_messages.iter().filter(|c| {
        c.after_input_messages.is_none() && c.after_exchanges == request.exchanges.len()
    }) {
        contents.push(super::context_message(context, "gemini")?);
    }
    if let Some(notice) = &request.retry_notice {
        contents.push(json!({"role":"user","parts":[{"text":notice}]}));
    }

    let declarations = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name":tool.name,
                "description":tool.description,
                "parametersJsonSchema":tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::new();
    body.insert("contents".to_owned(), Value::Array(contents));
    body.insert(
        "tools".to_owned(),
        json!([{"functionDeclarations":declarations}]),
    );
    body.insert(
        "toolConfig".to_owned(),
        json!({"functionCallingConfig":{"mode":"AUTO"}}),
    );
    let parameters = GeminiRequestParametersV1::from_parameters(&request.parameters)
        .map_err(|()| invalid_parameters())?;
    let mut generation_config = json!({"candidateCount":1});
    parameters.apply(&mut generation_config, model);
    body.insert("generationConfig".to_owned(), generation_config);
    if request.tools.is_empty() {
        body.remove("tools");
        body.remove("toolConfig");
    }
    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_owned(),
            json!({"parts":system_parts}),
        );
    }
    Ok(Value::Object(body))
}

pub(crate) fn normalize_gemini_tool_response(
    response: Value,
    tools: &[ModelToolDefinitionV1],
    provider_namespace: &str,
) -> Result<Vec<ModelToolEventV1>, ProviderError> {
    let object = response.as_object().ok_or_else(invalid_response)?;
    let candidates = object
        .get("candidates")
        .and_then(Value::as_array)
        .filter(|candidates| candidates.len() == 1)
        .ok_or_else(invalid_response)?;
    let candidate = candidates[0].as_object().ok_or_else(invalid_response)?;
    if candidate.get("finishReason").and_then(Value::as_str) != Some("STOP") {
        return Err(invalid_response());
    }
    let content = candidate
        .get("content")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    if content.get("role").and_then(Value::as_str) != Some("model") {
        return Err(invalid_response());
    }
    let parts = content
        .get("parts")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(invalid_response)?;
    let mut events = Vec::new();
    let mut call_count = 0_usize;
    for part in parts {
        let part = part.as_object().ok_or_else(invalid_response)?;
        match (part.get("text"), part.get("functionCall")) {
            (Some(Value::String(text)), None) if !text.is_empty() => {
                if part.contains_key("thoughtSignature") {
                    return Err(invalid_response());
                }
                events.push(ModelToolEventV1::AssistantOutput { text: text.clone() });
            }
            (None, Some(Value::Object(function))) => {
                let provider_call_id = function
                    .get("id")
                    .map(|id| {
                        id.as_str()
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(invalid_response)
                    })
                    .transpose()?;
                let name = required_string(function, "name")?;
                let arguments = function.get("args").cloned().unwrap_or_else(|| json!({}));
                let context = part
                    .get("thoughtSignature")
                    .map(|context| {
                        context
                            .as_str()
                            .filter(|context| !context.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(invalid_response)
                    })
                    .transpose()?;
                let call = normalize_tool_call(
                    tools,
                    provider_call_id,
                    name,
                    arguments,
                    context,
                    call_count,
                    provider_namespace,
                )
                .map_err(|_| invalid_response())?;
                call_count = call_count.saturating_add(1);
                events.push(ModelToolEventV1::ToolCall { call });
            }
            _ => return Err(invalid_response()),
        }
    }

    let usage = object
        .get("usageMetadata")
        .and_then(Value::as_object)
        .ok_or_else(invalid_response)?;
    let output_tokens = required_u64(usage, "candidatesTokenCount")?
        .checked_add(optional_u64(usage, "thoughtsTokenCount")?)
        .ok_or_else(invalid_response)?;
    events.push(ModelToolEventV1::Usage {
        input_tokens: required_u64(usage, "promptTokenCount")?,
        output_tokens,
        cache: Default::default(),
    });
    Ok(events)
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, ProviderError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(invalid_response)
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    object
        .get(key)
        .map_or(Ok(0), |value| value.as_u64().ok_or_else(invalid_response))
}

fn invalid_request() -> ProviderError {
    ProviderError::Failed("Gemini tool request is invalid".to_owned())
}

fn invalid_parameters() -> ProviderError {
    ProviderError::Failed("Gemini request parameters are unsupported or invalid".to_owned())
}

fn invalid_response() -> ProviderError {
    ProviderError::Failed("Gemini tool response is incomplete or unsupported".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(values: &[(&str, Value)]) -> BTreeMap<String, Value> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn thinking(values: &[(&str, Value)], model: &str) -> Option<Value> {
        GeminiRequestParametersV1::from_parameters(&parameters(values))
            .expect("valid Gemini parameters")
            .thinking_config(model)
    }

    #[test]
    fn reasoning_effort_maps_to_the_model_family_knob() {
        assert_eq!(
            thinking(&[("reasoningEffort", json!("high"))], "gemini-2.5-flash"),
            Some(json!({"thinkingBudget":16384}))
        );
        assert_eq!(
            thinking(&[("reasoningEffort", json!("none"))], "gemini-2.5-pro"),
            Some(json!({"thinkingBudget":128}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("none"))],
                "gemini-2.5-flash-lite"
            ),
            Some(json!({"thinkingBudget":0}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("max"))],
                "models/gemini-2.5-pro"
            ),
            Some(json!({"thinkingBudget":-1}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("high"))],
                "gemini-3-pro-preview"
            ),
            Some(json!({"thinkingLevel":"HIGH"}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("none"))],
                "gemini-3-pro-preview"
            ),
            Some(json!({"thinkingLevel":"LOW"}))
        );
        // The first Gemini 3 Pro has no medium level; it must not be sent one.
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("medium"))],
                "gemini-3-pro-preview"
            ),
            Some(json!({"thinkingLevel":"HIGH"}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("medium"))],
                "gemini-3-flash-preview"
            ),
            Some(json!({"thinkingLevel":"MEDIUM"}))
        );
        assert_eq!(
            thinking(
                &[("reasoningEffort", json!("xhigh"))],
                "gemini-3-flash-preview"
            ),
            Some(json!({"thinkingLevel":"HIGH"}))
        );
        assert_eq!(
            thinking(&[("reasoningEffort", json!("xhigh"))], "gemini-4-hydrogen"),
            Some(json!({"thinkingLevel":"XHIGH"}))
        );
        // A proxy alias outside the `gemini-<major>` shape keeps the budget form.
        assert_eq!(
            thinking(&[("reasoningEffort", json!("low"))], "local-reasoner"),
            Some(json!({"thinkingBudget":1024}))
        );
    }

    #[test]
    fn thinking_toggle_is_honoured_without_an_effort() {
        assert_eq!(
            thinking(&[("enableThinking", json!(false))], "gemini-2.5-flash"),
            Some(json!({"thinkingBudget":0}))
        );
        assert_eq!(
            thinking(&[("enableThinking", json!(false))], "gemini-3-pro-preview"),
            Some(json!({"thinkingLevel":"LOW"}))
        );
        assert_eq!(
            thinking(&[("enableThinking", json!(true))], "gemini-2.5-flash"),
            Some(json!({"thinkingBudget":8192}))
        );
        // No control at all leaves the model default alone.
        assert_eq!(thinking(&[], "gemini-2.5-flash"), None);
    }

    #[test]
    fn openai_only_or_malformed_parameters_are_rejected() {
        for values in [
            vec![("temperature", json!(0.5))],
            vec![("preserveThinking", json!(true))],
            vec![("reasoningEffort", json!("turbo"))],
            vec![("reasoningEffort", json!(3))],
            vec![("enableThinking", json!("yes"))],
            vec![("maxOutputTokens", json!(0))],
        ] {
            assert!(
                GeminiRequestParametersV1::from_parameters(&parameters(&values)).is_err(),
                "{values:?} must be rejected"
            );
        }
    }

    #[test]
    fn tool_request_applies_thinking_and_the_output_cap() {
        let request = ModelToolRequestV1 {
            context_messages: Vec::new(),
            input: json!({"messages":[{"role":"user","content":"hi"}]}),
            parameters: parameters(&[
                ("reasoningEffort", json!("medium")),
                ("maxOutputTokens", json!(4096)),
            ]),
            tools: Vec::new(),
            exchanges: Vec::new(),
            retry_notice: None,
        };
        let body = gemini_tool_request(&request, "gemini-2.5-flash").expect("gemini body");
        assert_eq!(body["generationConfig"]["candidateCount"], 1);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 4096);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget":8192})
        );

        let mut unsupported = request;
        unsupported
            .parameters
            .insert("temperature".into(), json!(0.2));
        assert!(
            gemini_tool_request(&unsupported, "gemini-2.5-flash")
                .unwrap_err()
                .to_string()
                .contains("parameters are unsupported")
        );
    }
}
