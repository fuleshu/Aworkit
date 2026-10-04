//! Bounded ComfyUI HTTP client and API-workflow inspection.
//!
//! ComfyUI is a local HTTP server (by default `http://127.0.0.1:8188`). This
//! module owns everything Aworkit needs to talk to it: endpoint validation, the
//! JSON and binary endpoints, and the analysis of an API-format workflow graph
//! that the Settings authoring flow and the bundled bridge build on.
//!
//! Every response is read through an explicit byte bound and a bounded timeout,
//! so an unresponsive or hostile local endpoint can shape a tool result but
//! cannot exhaust the process. A failure is always a typed, named error that
//! includes the endpoint; no method falls back silently to another host.

use std::io::Read as _;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Endpoint Aworkit assumes when the user has not configured one.
pub const DEFAULT_COMFYUI_ENDPOINT: &str = "http://127.0.0.1:8188";

/// Largest accepted JSON response. `/object_info` for a large node pack is the
/// biggest legitimate payload; it stays well below this bound.
const MAXIMUM_JSON_BYTES: usize = 8 * 1024 * 1024;
/// Largest accepted `GET /view` image.
pub const MAXIMUM_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Bound for the small control endpoints (`/system_stats`, `/prompt`).
const MAXIMUM_CONTROL_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const IMAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAXIMUM_ENDPOINT_BYTES: usize = 2_048;

/// Deterministic ComfyUI client or workflow-inspection failure.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ComfyUiError {
    /// The configured endpoint is not a usable ComfyUI base URL.
    #[error("invalid ComfyUI endpoint: {0}")]
    InvalidEndpoint(String),
    /// The server could not be reached at all.
    #[error("ComfyUI is not reachable at {endpoint}. Start ComfyUI, or set the endpoint in ComfyUI settings. ({cause})")]
    Unreachable {
        /// Normalized endpoint that was attempted.
        endpoint: String,
        /// Underlying transport cause.
        cause: String,
    },
    /// The server answered with a non-success status.
    #[error("ComfyUI answered HTTP {status} at {endpoint}.")]
    Status {
        /// Normalized endpoint that was attempted.
        endpoint: String,
        /// HTTP status code.
        status: u16,
    },
    /// The response exceeded the byte bound for that endpoint.
    #[error("ComfyUI response from {endpoint} exceeded {maximum_bytes} bytes.")]
    ResponseTooLarge {
        /// Normalized endpoint that was attempted.
        endpoint: String,
        /// Byte bound that was exceeded.
        maximum_bytes: usize,
    },
    /// The response body was not the expected JSON shape.
    #[error("ComfyUI returned an unexpected payload at {endpoint}: {cause}")]
    Malformed {
        /// Normalized endpoint that was attempted.
        endpoint: String,
        /// What was wrong with the payload.
        cause: String,
    },
    /// The workflow is not a ComfyUI API-format graph.
    #[error("the workflow is not a ComfyUI API-format graph: {0}")]
    InvalidWorkflow(String),
}

/// Value kind of one editable workflow input, derived from its current value.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComfyUiValueKindV1 {
    /// A free-form text input.
    String,
    /// A whole-number input.
    Integer,
    /// A fractional-number input.
    Number,
    /// A boolean switch.
    Boolean,
    /// A list or otherwise composite input that is not a plain scalar.
    Array,
}

impl ComfyUiValueKindV1 {
    /// Classifies a current ComfyUI input value. A two-element array whose
    /// first member is a node id is a graph link, not an editable value, and is
    /// handled by the caller through [`is_link_value`].
    #[must_use]
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Bool(_) => Self::Boolean,
            Value::Number(number) if number.is_i64() || number.is_u64() => Self::Integer,
            Value::Number(_) => Self::Number,
            Value::String(_) => Self::String,
            _ => Self::Array,
        }
    }

    /// The JSON-schema type a tool parameter of this kind declares.
    #[must_use]
    pub fn json_schema_type(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
        }
    }
}

/// One editable input of one node in a ComfyUI API workflow.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiWorkflowInputV1 {
    /// Node id as it appears in the API graph.
    pub node_id: String,
    /// ComfyUI class type of the node.
    pub class_type: String,
    /// Node title when the graph carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Input name inside the node.
    pub input_name: String,
    /// Classified value kind of the current value.
    pub value_kind: ComfyUiValueKindV1,
    /// Current value, bounded to a short scalar preview.
    pub current_value: Value,
    /// Allowed values when `/object_info` reports a closed choice set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Value>,
}

impl ComfyUiWorkflowInputV1 {
    /// Whether this input is exactly the `(node id, input name)` a parameter
    /// binding references.
    #[must_use]
    pub fn matches(&self, node_id: &str, input_name: &str) -> bool {
        self.node_id == node_id && self.input_name == input_name
    }
}

/// Bounded analysis of one ComfyUI API workflow.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiWorkflowInspectionV1 {
    /// Number of nodes in the graph.
    pub node_count: usize,
    /// Every editable input, ordered by node id then input name.
    pub inputs: Vec<ComfyUiWorkflowInputV1>,
}

impl ComfyUiWorkflowInspectionV1 {
    /// Finds the editable input a parameter binding must reference.
    #[must_use]
    pub fn find(&self, node_id: &str, input_name: &str) -> Option<&ComfyUiWorkflowInputV1> {
        self.inputs
            .iter()
            .find(|input| input.matches(node_id, input_name))
    }
}

/// One produced file reference returned by a queued workflow.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiImageRefV1 {
    /// Node that emitted the file.
    pub node_id: String,
    /// File name inside ComfyUI's output tree.
    pub filename: String,
    /// Subfolder inside ComfyUI's output tree.
    pub subfolder: String,
    /// ComfyUI file type (`output`, `temp` or `input`).
    pub kind: String,
    /// Absolute `GET /view` URL for the file.
    pub url: String,
}

/// Result of one queued ComfyUI workflow.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComfyUiRunResultV1 {
    /// ComfyUI prompt id.
    pub prompt_id: String,
    /// Produced image and video files.
    pub images: Vec<ComfyUiImageRefV1>,
}

/// Normalizes and validates a ComfyUI base URL.
///
/// The rule mirrors provider base URLs: `http` or `https`, a host, and no
/// user-info, query or fragment. A missing trailing slash is added so path
/// joins are unambiguous.
///
/// # Errors
///
/// Returns [`ComfyUiError::InvalidEndpoint`] for anything else.
pub fn validate_comfyui_endpoint(raw: &str) -> Result<String, ComfyUiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAXIMUM_ENDPOINT_BYTES || trimmed.contains('\0') {
        return Err(ComfyUiError::InvalidEndpoint(
            "the endpoint must be a bounded, non-empty URL".into(),
        ));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|error| ComfyUiError::InvalidEndpoint(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ComfyUiError::InvalidEndpoint(
            "the endpoint must use http or https".into(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(ComfyUiError::InvalidEndpoint(
            "the endpoint must name a host".into(),
        ));
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ComfyUiError::InvalidEndpoint(
            "the endpoint must not carry credentials, a query or a fragment".into(),
        ));
    }
    if parsed.cannot_be_a_base() {
        return Err(ComfyUiError::InvalidEndpoint(
            "the endpoint cannot be used as a base URL".into(),
        ));
    }
    let mut normalized = parsed;
    if !normalized.path().ends_with('/') {
        let path = format!("{}/", normalized.path());
        normalized.set_path(&path);
    }
    Ok(normalized.to_string())
}

/// Blocking, bounded ComfyUI client for one normalized endpoint.
#[derive(Clone, Debug)]
pub struct ComfyUiClient {
    endpoint: String,
    client: Client,
}

impl ComfyUiClient {
    /// Builds a client for a validated endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ComfyUiError`] when the endpoint or HTTP client is invalid.
    pub fn new(endpoint: &str) -> Result<Self, ComfyUiError> {
        let endpoint = validate_comfyui_endpoint(endpoint)?;
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .map_err(|error| ComfyUiError::InvalidEndpoint(error.to_string()))?;
        Ok(Self { endpoint, client })
    }

    /// The normalized endpoint this client talks to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn url(&self, path: &str) -> Result<Url, ComfyUiError> {
        Url::parse(&format!("{}{}", self.endpoint, path.trim_start_matches('/')))
            .map_err(|error| ComfyUiError::InvalidEndpoint(error.to_string()))
    }

    fn get_json(&self, path: &str, maximum_bytes: usize) -> Result<Value, ComfyUiError> {
        let url = self.url(path)?;
        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .map_err(|error| self.transport(&error))?;
        self.decode_json(response, maximum_bytes)
    }

    fn post_json(
        &self,
        path: &str,
        body: &Value,
        maximum_bytes: usize,
    ) -> Result<Value, ComfyUiError> {
        let url = self.url(path)?;
        let response = self
            .client
            .post(url)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .map_err(|error| self.transport(&error))?;
        self.decode_json(response, maximum_bytes)
    }

    fn decode_json(
        &self,
        response: reqwest::blocking::Response,
        maximum_bytes: usize,
    ) -> Result<Value, ComfyUiError> {
        let status = response.status();
        if !status.is_success() {
            return Err(ComfyUiError::Status {
                endpoint: self.endpoint.clone(),
                status: status.as_u16(),
            });
        }
        let bytes = read_bounded(response, maximum_bytes).map_err(|cause| {
            if cause == "too_large" {
                ComfyUiError::ResponseTooLarge {
                    endpoint: self.endpoint.clone(),
                    maximum_bytes,
                }
            } else {
                ComfyUiError::Malformed {
                    endpoint: self.endpoint.clone(),
                    cause,
                }
            }
        })?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|error| ComfyUiError::Malformed {
            endpoint: self.endpoint.clone(),
            cause: error.to_string(),
        })
    }

    fn transport(&self, error: &reqwest::Error) -> ComfyUiError {
        ComfyUiError::Unreachable {
            endpoint: self.endpoint.clone(),
            cause: error.to_string(),
        }
    }

    /// `GET /system_stats`. Also the reachability probe.
    ///
    /// # Errors
    ///
    /// Returns a named transport, status or payload error.
    pub fn system_stats(&self) -> Result<Value, ComfyUiError> {
        self.get_json("/system_stats", MAXIMUM_CONTROL_BYTES)
    }

    /// `GET /object_info`, the live node and input schema catalog.
    ///
    /// # Errors
    ///
    /// Returns a named transport, status, bound or payload error.
    pub fn object_info(&self) -> Result<Value, ComfyUiError> {
        self.get_json("/object_info", MAXIMUM_JSON_BYTES)
    }

    /// `POST /prompt`, queueing one API-format workflow.
    ///
    /// # Errors
    ///
    /// Returns a named transport, status or payload error.
    pub fn queue_prompt(&self, graph: &Value, client_id: &str) -> Result<String, ComfyUiError> {
        let body = serde_json::json!({ "prompt": graph, "client_id": client_id });
        let response = self.post_json("/prompt", &body, MAXIMUM_CONTROL_BYTES)?;
        response
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ComfyUiError::Malformed {
                endpoint: self.endpoint.clone(),
                cause: "the queue response carried no prompt id".into(),
            })
    }

    /// `GET /history/{prompt_id}`.
    ///
    /// # Errors
    ///
    /// Returns a named transport, status, bound or payload error.
    pub fn history(&self, prompt_id: &str) -> Result<Value, ComfyUiError> {
        let encoded: String = url_encode(prompt_id);
        self.get_json(&format!("/history/{encoded}"), MAXIMUM_JSON_BYTES)
    }

    /// Polls `/history/{prompt_id}` until the entry carries outputs or the
    /// deadline passes. The deadline bounds one wait, not a Run.
    ///
    /// # Errors
    ///
    /// Returns [`ComfyUiError::Malformed`] on deadline expiry or a named
    /// transport, status or payload error.
    pub fn wait_for_history(
        &self,
        prompt_id: &str,
        timeout: Duration,
    ) -> Result<Value, ComfyUiError> {
        let deadline = Instant::now() + timeout;
        loop {
            let history = self.history(prompt_id)?;
            if let Some(entry) = history.get(prompt_id) {
                if entry.get("outputs").is_some() {
                    return Ok(entry.clone());
                }
            }
            if Instant::now() >= deadline {
                return Err(ComfyUiError::Malformed {
                    endpoint: self.endpoint.clone(),
                    cause: format!(
                        "prompt {prompt_id} did not finish within {} seconds",
                        timeout.as_secs()
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(750));
        }
    }

    /// `GET /view`, returning one produced file's bytes under a byte bound.
    ///
    /// # Errors
    ///
    /// Returns a named transport, status or bound error.
    pub fn view(&self, filename: &str, subfolder: &str, kind: &str) -> Result<Vec<u8>, ComfyUiError> {
        let url = self.url("/view")?;
        let mut request = self.client.get(url).query(&[
            ("filename", filename.to_owned()),
            ("subfolder", subfolder.to_owned()),
            ("type", kind.to_owned()),
        ]);
        request = request.timeout(IMAGE_REQUEST_TIMEOUT);
        let response = request.send().map_err(|error| self.transport(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ComfyUiError::Status {
                endpoint: self.endpoint.clone(),
                status: status.as_u16(),
            });
        }
        read_bounded(response, MAXIMUM_IMAGE_BYTES).map_err(|cause| {
            if cause == "too_large" {
                ComfyUiError::ResponseTooLarge {
                    endpoint: self.endpoint.clone(),
                    maximum_bytes: MAXIMUM_IMAGE_BYTES,
                }
            } else {
                ComfyUiError::Malformed {
                    endpoint: self.endpoint.clone(),
                    cause,
                }
            }
        })
    }
}

/// Reads a response body up to `maximum_bytes`, refusing a larger body instead
/// of buffering it.
fn read_bounded(mut response: reqwest::blocking::Response, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    if let Some(length) = response.content_length() {
        if length > maximum_bytes as u64 {
            return Err("too_large".into());
        }
    }
    let mut buffer = Vec::new();
    let limit = u64::try_from(maximum_bytes).unwrap_or(u64::MAX).saturating_add(1);
    response
        .by_ref()
        .take(limit)
        .read_to_end(&mut buffer)
        .map_err(|error| error.to_string())?;
    if buffer.len() > maximum_bytes {
        return Err("too_large".into());
    }
    Ok(buffer)
}

/// Percent-encodes a path segment without pulling in a URL-encoding crate.
fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Whether a raw input value is a graph link rather than an editable value.
///
/// ComfyUI's API format encodes a connection as a two-element array holding the
/// source node id and the output index.
#[must_use]
pub fn is_link_value(value: &Value) -> bool {
    value.as_array().is_some_and(|array| {
        array.len() == 2 && array[0].is_string() && array[1].as_u64().is_some()
    })
}

/// Analyzes an API-format workflow and lists the inputs a parameter could bind.
///
/// Model filenames, sampler internals and node plumbing stay visible as
/// candidates; choosing which of them a user should control is the authoring
/// step's job, not this analysis.
///
/// # Errors
///
/// Returns [`ComfyUiError::InvalidWorkflow`] when the value is not an API graph.
pub fn inspect_api_workflow(graph: &Value) -> Result<ComfyUiWorkflowInspectionV1, ComfyUiError> {
    inspect_api_workflow_with_object_info(graph, None)
}

/// As [`inspect_api_workflow`], attaching closed choice sets reported by
/// `GET /object_info` to matching combo inputs.
///
/// # Errors
///
/// Returns [`ComfyUiError::InvalidWorkflow`] when the value is not an API graph.
pub fn inspect_api_workflow_with_object_info(
    graph: &Value,
    object_info: Option<&Value>,
) -> Result<ComfyUiWorkflowInspectionV1, ComfyUiError> {
    let nodes = graph.as_object().ok_or_else(|| {
        ComfyUiError::InvalidWorkflow("the top level must be an object of node id -> node".into())
    })?;
    let mut inputs = Vec::new();
    for (node_id, node) in nodes {
        let Some(node) = node.as_object() else {
            continue;
        };
        let Some(class_type) = node.get("class_type").and_then(Value::as_str) else {
            continue;
        };
        let title = node
            .get("_meta")
            .and_then(Value::as_object)
            .and_then(|meta| meta.get("title"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(node_inputs) = node.get("inputs").and_then(Value::as_object) else {
            continue;
        };
        for (input_name, value) in node_inputs {
            if is_link_value(value) {
                continue;
            }
            if value.is_object() || value.is_null() {
                continue;
            }
            let choices = choices_for(object_info, class_type, input_name);
            inputs.push(ComfyUiWorkflowInputV1 {
                node_id: node_id.clone(),
                class_type: class_type.to_owned(),
                title: title.clone(),
                input_name: input_name.clone(),
                value_kind: ComfyUiValueKindV1::of(value),
                current_value: preview_value(value),
                choices,
            });
        }
    }
    inputs.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then_with(|| left.input_name.cmp(&right.input_name))
    });
    Ok(ComfyUiWorkflowInspectionV1 {
        node_count: nodes.len(),
        inputs,
    })
}

/// Extracts the closed choice list for one `(class_type, input)` pair when the
/// object-info catalog reports one.
fn choices_for(object_info: Option<&Value>, class_type: &str, input_name: &str) -> Vec<Value> {
    let Some(catalog) = object_info.and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(definition) = catalog.get(class_type).and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(required) = definition
        .get("input")
        .and_then(Value::as_object)
        .and_then(|input| input.get("required"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    match required.get(input_name) {
        Some(Value::Array(values)) => match values.first() {
            Some(Value::Array(choices)) => choices
                .iter()
                .filter(|value| value.is_string() || value.is_number() || value.is_boolean())
                .take(512)
                .cloned()
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Bounds one current value to a short scalar preview suitable for a prompt or
/// a Settings table.
fn preview_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.chars().take(512).collect()),
        Value::Array(items) => Value::Array(items.iter().take(32).map(preview_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .take(32)
                .map(|(key, value)| (key.clone(), preview_value(value)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::Write as _,
        net::TcpListener,
        thread::{self, JoinHandle},
    };

    #[test]
    fn endpoint_validation_matches_provider_rules() {
        assert_eq!(
            validate_comfyui_endpoint("http://127.0.0.1:8188").unwrap(),
            "http://127.0.0.1:8188/"
        );
        assert_eq!(
            validate_comfyui_endpoint(" http://localhost:8188/ ").unwrap(),
            "http://localhost:8188/"
        );
        for invalid in [
            "",
            "ftp://127.0.0.1:8188",
            "http://user:pass@127.0.0.1:8188",
            "http://127.0.0.1:8188/?a=1",
            "http://127.0.0.1:8188/#frag",
            "not-a-url",
        ] {
            assert!(
                validate_comfyui_endpoint(invalid).is_err(),
                "{invalid} should be rejected"
            );
        }
    }

    #[test]
    fn inspection_lists_editable_inputs_and_skips_links() {
        let graph = json!({
            "3": {"class_type": "KSampler", "inputs": {"seed": 42, "steps": 20, "cfg": 7.5, "model": ["4", 0]}, "_meta": {"title": "Sampler"}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a quiet harbour", "clip": ["5", 1]}, "_meta": {"title": "Positive prompt"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "Krea"}},
            "10": {"class_type": "Primitive", "inputs": {"enabled": true}}
        });
        let inspection = inspect_api_workflow(&graph).unwrap();
        assert_eq!(inspection.node_count, 4);
        assert!(inspection.find("3", "seed").is_some());
        assert!(inspection.find("6", "text").is_some());
        assert!(inspection.find("9", "filename_prefix").is_some());
        assert!(inspection.find("10", "enabled").is_some());
        // A two-element `[node id, output index]` value is a connection.
        assert!(inspection.find("3", "model").is_none());
        assert!(inspection.find("6", "clip").is_none());
        assert_eq!(
            inspection.find("3", "seed").unwrap().value_kind,
            ComfyUiValueKindV1::Integer
        );
        assert_eq!(
            inspection.find("3", "cfg").unwrap().value_kind,
            ComfyUiValueKindV1::Number
        );
        assert_eq!(
            inspection.find("10", "enabled").unwrap().value_kind,
            ComfyUiValueKindV1::Boolean
        );
        assert_eq!(
            inspection.find("6", "text").unwrap().title.as_deref(),
            Some("Positive prompt")
        );
    }

    #[test]
    fn inspection_rejects_a_non_api_document() {
        assert!(inspect_api_workflow(&json!({"nodes": []})).is_ok());
        assert!(inspect_api_workflow(&json!([1, 2, 3])).is_err());
        assert!(inspect_api_workflow(&json!("text")).is_err());
    }

    #[test]
    fn object_info_choices_attach_to_matching_combo_inputs() {
        let graph = json!({
            "3": {"class_type": "KSampler", "inputs": {"sampler_name": "euler"}}
        });
        let object_info = json!({
            "KSampler": {"input": {"required": {"sampler_name": [["euler", "dpmpp_2m"], {"default": "euler"}]}}}
        });
        let inspection =
            inspect_api_workflow_with_object_info(&graph, Some(&object_info)).unwrap();
        let input = inspection.find("3", "sampler_name").unwrap();
        assert_eq!(input.choices, vec![json!("euler"), json!("dpmpp_2m")]);
    }

    #[test]
    fn client_reads_system_stats_and_queues_a_prompt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server: JoinHandle<()> = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 4096];
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]);
                let body = if request.starts_with("POST /prompt") {
                    json!({"prompt_id": "fixture-prompt"}).to_string()
                } else {
                    json!({"system": {"comfyui_version": "0.3.0"}, "devices": []}).to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let client = ComfyUiClient::new(&format!("http://{address}")).unwrap();
        let stats = client.system_stats().unwrap();
        assert_eq!(stats["system"]["comfyui_version"], "0.3.0");
        let prompt = client
            .queue_prompt(&json!({"3": {"class_type": "KSampler"}}), "aworkit")
            .unwrap();
        assert_eq!(prompt, "fixture-prompt");
        server.join().unwrap();
    }

    #[test]
    fn unreachable_endpoint_names_the_endpoint() {
        // Port 1 is not bound by any local service in practice.
        let client = ComfyUiClient::new("http://127.0.0.1:1").unwrap();
        let error = client.system_stats().unwrap_err();
        assert!(error.to_string().contains("http://127.0.0.1:1/"));
    }
}
