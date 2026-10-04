//! ComfyUI Settings commands.
//!
//! These are thin, data-in/data-out commands on [`DesktopRuntime`]: a
//! reachability probe, an explicit local start, a workflow inspection, and the
//! model-assisted parameter authoring call. Nothing here saves Settings; an
//! authored tool is returned to the Settings draft for the user to review and
//! save.

use std::{collections::BTreeMap, path::Path, time::Duration};

use serde_json::Value;

use aworkit_capability_host::{ComfyUiClient, inspect_api_workflow_with_object_info};

use super::{DesktopRuntime, ResolvedWorkflowModel, resolve_workflow_model};
use crate::runtime::comfyui::{
    ComfyUiAutocreateRequestV2, ComfyUiAutocreateResultV2, ComfyUiInspectRequestV2,
    ComfyUiInspectResultV2, ComfyUiProbeRequestV2, ComfyUiProbeResultV2, ComfyUiStartRequestV2,
    ComfyUiStartResultV2,
};

/// Largest workflow JSON read for inspection or authoring.
const MAXIMUM_WORKFLOW_BYTES: u64 = 2 * 1024 * 1024;
/// Startup wait for one explicit local start.
const LOCAL_START_WAIT: Duration = Duration::from_secs(90);
impl DesktopRuntime {
    /// Probes a ComfyUI endpoint. This never starts anything.
    pub fn settings_v2_comfyui_probe(&mut self, request: ComfyUiProbeRequestV2) -> ComfyUiProbeResultV2 {
        let probe = crate::runtime::comfyui::server::probe(&request.endpoint);
        ComfyUiProbeResultV2 {
            ok: probe.reachable,
            message: probe.message,
            endpoint: normalized_endpoint(&request.endpoint),
            version: probe.version,
            latency_millis: probe.latency_millis,
            draft_fingerprint: request.draft_fingerprint,
        }
    }

    /// Starts the configured local ComfyUI server and waits for readiness.
    pub fn settings_v2_comfyui_start(&mut self, request: ComfyUiStartRequestV2) -> ComfyUiStartResultV2 {
        let endpoint = normalized_endpoint(&request.endpoint);
        let log_directory = self
            .documents
            .data_root()
            .join(crate::runtime::comfyui::COMFYUI_DIRECTORY)
            .join("logs");
        match crate::runtime::comfyui::server::start_and_wait(
            &endpoint,
            Path::new(&request.install_path),
            &request.launch_arguments,
            &log_directory,
            LOCAL_START_WAIT,
        ) {
            Ok(outcome) => ComfyUiStartResultV2 {
                ok: outcome.ready,
                message: outcome.message,
                endpoint,
                process_id: outcome.process_id,
                log_path: outcome.log_path,
                output_tail: outcome.output_tail,
            },
            Err(message) => ComfyUiStartResultV2 {
                ok: false,
                message,
                endpoint,
                process_id: None,
                log_path: String::new(),
                output_tail: String::new(),
            },
        }
    }

    /// Inspects one API workflow and lists its editable inputs.
    pub fn settings_v2_comfyui_inspect(
        &mut self,
        request: ComfyUiInspectRequestV2,
    ) -> ComfyUiInspectResultV2 {
        let workflow_path = request.workflow_path.clone();
        let graph = match read_workflow_json(Path::new(&workflow_path)) {
            Ok(graph) => graph,
            Err(message) => {
                return ComfyUiInspectResultV2 {
                    ok: false,
                    message,
                    workflow_path,
                    inspection: Default::default(),
                };
            }
        };
        let object_info = request
            .endpoint
            .as_deref()
            .and_then(|endpoint| ComfyUiClient::new(endpoint).ok())
            .and_then(|client| client.object_info().ok());
        match inspect_api_workflow_with_object_info(&graph, object_info.as_ref()) {
            Ok(inspection) => ComfyUiInspectResultV2 {
                ok: true,
                message: format!(
                    "Found {} editable inputs across {} nodes.",
                    inspection.inputs.len(),
                    inspection.node_count
                ),
                workflow_path,
                inspection,
            },
            Err(error) => ComfyUiInspectResultV2 {
                ok: false,
                message: error.to_string(),
                workflow_path,
                inspection: Default::default(),
            },
        }
    }

    /// Authors a workflow tool from one API workflow with the model mapped to
    /// `tier:balanced`. It proposes; it never saves.
    ///
    /// # Errors
    ///
    /// Returns a message when the workflow cannot be read, the tier is not an
    /// exact configured provider/model, the provider call fails, or the model's
    /// answer binds an input the workflow does not contain.
    pub fn settings_v2_comfyui_autocreate(
        &mut self,
        request: ComfyUiAutocreateRequestV2,
    ) -> Result<ComfyUiAutocreateResultV2, String> {
        let graph = read_workflow_json(Path::new(&request.workflow_path))?;
        let object_info = ComfyUiClient::new(&request.endpoint)
            .ok()
            .and_then(|client| client.object_info().ok());
        let inspection = inspect_api_workflow_with_object_info(&graph, object_info.as_ref())
            .map_err(|error| error.to_string())?;
        if inspection.inputs.is_empty() {
            return Err(
                "this workflow exposes no editable scalar inputs, so no usable tool parameters can be authored"
                    .into(),
            );
        }
        let settings = self.documents.settings().clone();
        let resolved = resolve_workflow_model(&settings, "tier:balanced")?;
        let use_stored_credential = resolved.provider.credential_ref.is_some();
        let limits = resolved.provider.runtime_limits()?;
        let messages = vec![
            (
                "system".to_owned(),
                crate::runtime::comfyui::authoring::authoring_instructions(
                    &request.workflow_path,
                    &inspection,
                ),
            ),
            ("user".to_owned(), render_workflow_message(&graph)?),
        ];
        let authoring_parameters = authoring_parameters(&resolved.model.parameters);
        let (completion, parsed) = self.author_or_retry(
            &resolved,
            use_stored_credential,
            Duration::from_secs(limits.request_timeout_seconds),
            messages,
            authoring_parameters,
        )?;
        let accepted = crate::runtime::comfyui::authoring::accept_authoring_output(
            parsed,
            &inspection,
            &fallback_tool_id(&request.workflow_path),
            &request.workflow_path,
        )?;
        Ok(ComfyUiAutocreateResultV2 {
            ok: true,
            message: format!(
                "Proposed {} parameter(s) with {}. Review them, then save Settings.",
                accepted.tool.parameters.len(),
                completion.model
            ),
            tool: accepted.tool,
            summary: accepted.summary,
            provider_id: resolved.provider.id.clone(),
            model_id: resolved.model.id.clone(),
            input_tokens: completion.input_units,
            output_tokens: completion.output_units,
        })
    }

    /// Issues the authoring completion, then one bounded correction retry when
    /// the answer is not the required JSON object.
    fn author_or_retry(
        &mut self,
        resolved: &ResolvedWorkflowModel,
        use_stored_credential: bool,
        request_timeout: Duration,
        messages: Vec<(String, String)>,
        parameters: BTreeMap<String, Value>,
    ) -> Result<
        (
            super::super::provider::ProviderCompletion,
            crate::runtime::comfyui::authoring::ComfyUiAuthoringOutputV1,
        ),
        String,
    > {
        let credential = self.provider_credential_for(resolved, use_stored_credential)?;
        let first = self
            .provider
            .complete_text(
                &resolved.provider.kind,
                &resolved.provider.base_url,
                &resolved.model.remote_id,
                credential,
                request_timeout,
                &messages,
                parameters.clone(),
            )
            .map_err(|error| {
                format!("the ComfyUI parameter authoring call failed: {error}")
            })?;
        match crate::runtime::comfyui::authoring::parse_authoring_output(&first.text) {
            Ok(parsed) => Ok((first, parsed)),
            Err(reason) => {
                let mut retry_messages = messages;
                retry_messages.push(("assistant".to_owned(), first.text));
                retry_messages.push((
                    "user".to_owned(),
                    crate::runtime::comfyui::authoring::authoring_correction_notice(&reason),
                ));
                let credential = self.provider_credential_for(resolved, use_stored_credential)?;
                let retry = self
                    .provider
                    .complete_text(
                        &resolved.provider.kind,
                        &resolved.provider.base_url,
                        &resolved.model.remote_id,
                        credential,
                        request_timeout,
                        &retry_messages,
                        parameters,
                    )
                    .map_err(|error| {
                        format!("the ComfyUI parameter authoring retry failed: {error}")
                    })?;
                let parsed =
                    crate::runtime::comfyui::authoring::parse_authoring_output(&retry.text)
                        .map_err(|second| {
                            format!("{reason}; the correction retry was also rejected: {second}")
                        })?;
                Ok((retry, parsed))
            }
        }
    }

    fn provider_credential_for(
        &mut self,
        resolved: &ResolvedWorkflowModel,
        use_stored_credential: bool,
    ) -> Result<Option<String>, String> {
        self.provider_operation_credential(&resolved.provider, None, use_stored_credential)
    }
}

/// Normalizes an endpoint for reporting, falling back to the raw value.
fn normalized_endpoint(endpoint: &str) -> String {
    aworkit_capability_host::validate_comfyui_endpoint(endpoint)
        .unwrap_or_else(|_| endpoint.to_owned())
}

/// Reads one workflow JSON under a byte bound.
fn read_workflow_json(path: &Path) -> Result<Value, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot read the workflow {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("the workflow path is not a file: {}", path.display()));
    }
    if metadata.len() > MAXIMUM_WORKFLOW_BYTES {
        return Err(format!(
            "the workflow {} is larger than {} MiB",
            path.display(),
            MAXIMUM_WORKFLOW_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read the workflow {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("the workflow {} is not valid JSON: {error}", path.display()))
}

/// Renders the workflow for the authoring user message, bounded.
fn render_workflow_message(graph: &Value) -> Result<String, String> {
    let rendered = serde_json::to_string_pretty(graph).map_err(|error| error.to_string())?;
    if rendered.len() > crate::runtime::comfyui::authoring::MAXIMUM_AUTHORING_WORKFLOW_BYTES {
        return Err("the workflow is too large to send for authoring".into());
    }
    Ok(format!("ComfyUI API workflow:\n{rendered}"))
}

/// Model parameters for the authoring call.
///
/// The authoring call deliberately adds no output-token cap and overrides no
/// model setting: the configured model's own declared output is the only bound,
/// so a complex workflow gets as much room as the user configured for that
/// model. A transport byte guard still protects the process, and it is reported
/// rather than converted into a rewritten request.
fn authoring_parameters(model_parameters: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    model_parameters.clone()
}

/// Derives a stable identifier from a workflow file name.
fn fallback_tool_id(workflow_path: &str) -> String {
    let stem = Path::new(workflow_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("comfyui-workflow");
    let mut id = String::new();
    for character in stem.chars() {
        if character.is_ascii_alphanumeric() {
            id.push(character.to_ascii_lowercase());
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-');
    let id = if id.is_empty() { "comfyui-workflow" } else { id };
    id.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fallback_ids_are_identifier_safe_and_bounded() {
        assert_eq!(
            fallback_tool_id("D:\\wf\\Krea-2_Turbo_T2I_Upscale_API.json"),
            "krea-2-turbo-t2i-upscale-api"
        );
        assert_eq!(fallback_tool_id(":::"), "comfyui-workflow");
        assert!(fallback_tool_id(&"a".repeat(200)).len() <= 64);
    }

    #[test]
    fn authoring_forwards_the_configured_model_parameters_without_a_forced_cap() {
        let mut model = BTreeMap::new();
        model.insert("reasoningEffort".to_owned(), json!("high"));
        let parameters = authoring_parameters(&model);
        // The configured model settings pass through untouched...
        assert_eq!(parameters["reasoningEffort"], json!("high"));
        assert_eq!(parameters.len(), 1);
        // ...and Aworkit adds no output-token cap of its own.
        assert!(!parameters.contains_key("maxOutputTokens"));
    }

    #[test]
    fn reading_a_workflow_rejects_missing_files_and_bad_json() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_workflow_json(&root.path().join("absent.json")).is_err());
        let path = root.path().join("bad.json");
        std::fs::write(&path, b"{").unwrap();
        assert!(read_workflow_json(&path).is_err());
        let good = root.path().join("good.json");
        std::fs::write(&good, br#"{"1":{"class_type":"KSampler","inputs":{}}}"#).unwrap();
        assert!(read_workflow_json(&good).is_ok());
    }

    #[test]
    fn normalized_endpoints_report_the_configured_value_when_invalid() {
        assert_eq!(
            normalized_endpoint("http://127.0.0.1:8188"),
            "http://127.0.0.1:8188/"
        );
        assert_eq!(normalized_endpoint("nonsense"), "nonsense");
    }
}
