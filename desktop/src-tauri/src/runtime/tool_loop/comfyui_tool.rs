//! Native ComfyUI workflow execution.
//!
//! One frozen workflow tool runs entirely in this process: the API graph is read
//! from the path frozen with the binding, the model's arguments are written into
//! the exact `(node id, input name)` pairs the Settings tab validated, the prompt
//! is queued on the configured ComfyUI server, and the produced image is stored
//! as immutable image evidence. No script layer and no host interpreter are
//! involved.
use super::*;

/// Longest single wait for one queued prompt. This bounds one poll, not a Run:
/// on expiry the call is reported as failed and the model continues.
const COMFYUI_WAIT: Duration = Duration::from_secs(600);
/// Largest workflow JSON read for execution.
const MAXIMUM_WORKFLOW_BYTES: u64 = 2 * 1024 * 1024;
/// Stable client id, so a ComfyUI server can group Aworkit's queues.
const COMFYUI_CLIENT_ID: &str = "aworkit";

impl FileToolDispatcherV1 {
    /// Runs one frozen ComfyUI workflow tool and stores its image output.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the workflow cannot be read, the
    /// frozen graph does not carry a bound input, the server refuses the prompt,
    /// the wait expires, or the produced file cannot be stored.
    pub(super) fn run_comfyui_tool(
        &self,
        endpoint: &str,
        workflow_path: &str,
        parameters: &[Value],
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        validate_call_arguments(&self.record.binding, &self.record.call.arguments)
            .map_err(|error| error.to_string())?;
        if cancellation.is_cancelled() {
            return Err("ComfyUI workflow run cancelled".into());
        }
        let arguments = self
            .record
            .call
            .arguments
            .as_object()
            .ok_or("ComfyUI arguments must be an object")?;
        let path = Path::new(workflow_path);
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("Could not read the ComfyUI workflow: {error}"))?;
        if metadata.len() > MAXIMUM_WORKFLOW_BYTES {
            return Err("The ComfyUI workflow is larger than the 2 MiB bound".into());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("Could not read the ComfyUI workflow: {error}"))?;
        let mut graph: Value = serde_json::from_str(&text)
            .map_err(|error| format!("The ComfyUI workflow is not valid JSON: {error}"))?;
        for parameter in parameters {
            let name = parameter
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // An omitted optional parameter keeps the value the workflow ships
            // with; only what the model actually asked for is written.
            let Some(value) = arguments.get(name) else {
                continue;
            };
            let node_id = parameter
                .get("nodeId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input_name = parameter
                .get("inputName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let node = graph
                .get_mut(node_id)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| format!("The frozen workflow has no node '{node_id}'"))?;
            let inputs = node
                .get_mut("inputs")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| format!("Workflow node '{node_id}' has no inputs"))?;
            inputs.insert(input_name.to_owned(), value.clone());
        }
        if cancellation.is_cancelled() {
            return Err("ComfyUI workflow run cancelled".into());
        }
        let client = aworkit_capability_host::ComfyUiClient::new(endpoint)
            .map_err(|error| format!("The ComfyUI endpoint is unusable: {error}"))?;
        let prompt_id = client
            .queue_prompt(&graph, COMFYUI_CLIENT_ID)
            .map_err(|error| format!("ComfyUI refused the workflow: {error}"))?;
        let entry = client
            .wait_for_history(&prompt_id, COMFYUI_WAIT)
            .map_err(|error| {
                format!("ComfyUI did not finish prompt {prompt_id} within {}s: {error}", COMFYUI_WAIT.as_secs())
            })?;
        let (filename, subfolder, kind) = first_output_image(&entry).ok_or_else(|| {
            format!("ComfyUI finished prompt {prompt_id} without an image output")
        })?;
        if cancellation.is_cancelled() {
            return Err("ComfyUI workflow run cancelled".into());
        }
        let bytes = client
            .view(&filename, &subfolder, &kind)
            .map_err(|error| format!("Could not read the produced image: {error}"))?;
        let image = self
            .runtime
            .images
            .import_bytes(filename.clone(), &bytes)
            .map_err(|error| format!("Could not store the produced image: {error}"))?;
        // Two paths are reported: the named media file a person or a media tool
        // can use directly, and the immutable evidence blob it links to.
        let stored_path = self.runtime.images.blob_path(&image);
        let (output_path, export_error) = match self
            .runtime
            .images
            .publish_media("comfyui", &subfolder, &filename, &image)
        {
            Ok(path) => (path, None),
            Err(error) => (stored_path.clone(), Some(error)),
        };
        let summary = format!(
            "Ran ComfyUI workflow {prompt_id}; produced {} ({} bytes) at {}.",
            image.name,
            image.byte_length,
            output_path.display()
        );
        let mut source = json!({
            "endpoint": endpoint,
            "workflowPath": workflow_path,
            "promptId": prompt_id,
            "filename": filename,
            "subfolder": subfolder,
            "outputPath": output_path.to_string_lossy(),
            "storedPath": stored_path.to_string_lossy(),
        });
        if let Some(error) = export_error {
            // The blob already exists, so a failed named export is reported to
            // the model instead of failing the call.
            if let Some(object) = source.as_object_mut() {
                object.insert("outputPathError".into(), Value::from(error));
            }
        }
        Ok((json!({ "image": image, "source": source }), summary))
    }

    /// Runs one read-only ComfyUI authoring helper.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when the endpoint is unusable, the
    /// server refuses the catalog read, or the requested workflow id is not one
    /// of the tools frozen with the Chat.
    pub(super) fn run_comfyui_authoring(
        &self,
        capability_id: &str,
        endpoint: &str,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        validate_call_arguments(&self.record.binding, &self.record.call.arguments)
            .map_err(|error| error.to_string())?;
        if cancellation.is_cancelled() {
            return Err("ComfyUI authoring call cancelled".into());
        }
        let arguments = self
            .record
            .call
            .arguments
            .as_object()
            .ok_or("ComfyUI authoring arguments must be an object")?;
        match capability_id {
            crate::runtime::comfyui::COMFYUI_NODE_TYPES_CAPABILITY_ID => {
                let client = aworkit_capability_host::ComfyUiClient::new(endpoint)
                    .map_err(|error| format!("The ComfyUI endpoint is unusable: {error}"))?;
                let catalog = client
                    .object_info()
                    .map_err(|error| format!("Could not read the ComfyUI node catalog: {error}"))?;
                let search = arguments
                    .get("search")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(50, |value| value.clamp(1, 200)) as usize;
                let (nodes, matched) = node_catalog_page(&catalog, &search, limit);
                Ok((
                    json!({
                        "endpoint": endpoint,
                        "search": search,
                        "limit": limit,
                        "matched": matched,
                        "nodes": nodes,
                    }),
                    format!("Returned {matched} matching node type(s); at most {limit} shown."),
                ))
            }
            crate::runtime::comfyui::COMFYUI_GET_WORKFLOW_CAPABILITY_ID => {
                let workflow_id = arguments
                    .get("workflow")
                    .and_then(Value::as_str)
                    .ok_or("ComfyUI workflow id must be a string")?;
                let path = match &self.record.binding.limit {
                    StoredFileToolLimitV1::ComfyUiAuthoring { workflows, .. } => {
                        workflows.get(workflow_id).ok_or_else(|| {
                            format!("'{workflow_id}' is not a configured ComfyUI workflow tool")
                        })?
                    }
                    _ => return Err("ComfyUI authoring binding has no frozen workflow table".into()),
                };
                let graph = read_bounded_workflow(Path::new(path))?;
                Ok((
                    json!({ "workflow": workflow_id, "workflowPath": path, "graph": graph }),
                    format!("Read the API graph of ComfyUI workflow '{workflow_id}'."),
                ))
            }
            _ => Err("Unknown ComfyUI authoring capability".into()),
        }
    }
}

/// One bounded page of the node-type catalog: compact entries whose type,
/// category or input names contain `search`, plus the total number of matches.
///
/// The iteration order is the catalog's own object order, so the same request
/// returns the same page for a given ComfyUI server.
#[must_use]
fn node_catalog_page(catalog: &Value, search: &str, limit: usize) -> (Vec<Value>, usize) {
    let Some(object) = catalog.as_object() else {
        return (Vec::new(), 0);
    };
    let mut matched = Vec::new();
    for (class_type, definition) in object {
        let category = definition
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let inputs = definition
            .get("input")
            .and_then(|input| input.get("required"))
            .and_then(Value::as_object)
            .map(|required| required.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if !search.is_empty()
            && !class_type.to_lowercase().contains(search)
            && !category.to_lowercase().contains(search)
            && !inputs.iter().any(|name| name.to_lowercase().contains(search))
        {
            continue;
        }
        matched.push(json!({
            "type": class_type,
            "category": category,
            "inputs": inputs,
        }));
    }
    let total = matched.len();
    (matched.into_iter().take(limit).collect(), total)
}

/// Reads one API workflow JSON under the same 2 MiB bound execution uses.
fn read_bounded_workflow(path: &Path) -> Result<Value, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not read the ComfyUI workflow: {error}"))?;
    if metadata.len() > MAXIMUM_WORKFLOW_BYTES {
        return Err("The ComfyUI workflow is larger than the 2 MiB bound".into());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("Could not read the ComfyUI workflow: {error}"))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("The ComfyUI workflow is not valid JSON: {error}"))
}

/// The image a ComfyUI history entry produced.
///
/// A saved `output` is preferred over a `temp` preview, because a preview is
/// regenerated and may not exist once the prompt settles. Node order is not
/// meaningful to ComfyUI, so the first reported candidate of the best kind wins;
/// an entry without any image returns `None` instead of inventing a filename.
#[must_use]
fn first_output_image(entry: &Value) -> Option<(String, String, String)> {
    let outputs = entry.get("outputs")?.as_object()?;
    let mut fallback = None;
    for output in outputs.values() {
        let Some(image) = output
            .get("images")
            .and_then(Value::as_array)
            .and_then(|images| images.first())
        else {
            continue;
        };
        let Some(filename) = image.get("filename").and_then(Value::as_str) else {
            continue;
        };
        let kind = image
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("output")
            .to_owned();
        let candidate = (
            filename.to_owned(),
            image
                .get("subfolder")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            kind.clone(),
        );
        if kind == "output" {
            return Some(candidate);
        }
        if fallback.is_none() {
            fallback = Some(candidate);
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reported_image_is_the_produced_file() {
        let entry = json!({
            "outputs": {
                "9": {"images": [{"filename": "krea_00001_.png", "subfolder": "", "type": "output"}]},
                "12": {"images": [{"filename": "other.png", "subfolder": "sub", "type": "temp"}]},
            }
        });
        let (filename, subfolder, kind) = first_output_image(&entry).expect("an image");
        assert_eq!(filename, "krea_00001_.png");
        assert_eq!(subfolder, "");
        assert_eq!(kind, "output");
    }

    #[test]
    fn an_entry_without_images_or_a_filename_reports_nothing() {
        assert!(first_output_image(&json!({"outputs": {}})).is_none());
        assert!(first_output_image(&json!({"outputs": {"9": {"text": []}}})).is_none());
        assert!(first_output_image(&json!({"outputs": {"9": {"images": [{}]}}})).is_none());
        assert!(first_output_image(&json!({})).is_none());
    }

    #[test]
    fn the_node_catalog_page_is_bounded_and_searchable() {
        let catalog = json!({
            "KSampler": {"category": "sampling", "input": {"required": {"seed": [], "steps": []}}},
            "CLIPTextEncode": {"category": "conditioning", "input": {"required": {"text": []}}},
            "SaveImage": {"category": "image", "input": {"required": {"filename_prefix": []}}}
        });
        let (page, total) = node_catalog_page(&catalog, "", 10);
        assert_eq!(total, 3);
        assert_eq!(page.len(), 3);
        // A page smaller than the match count reports the true total.
        let (page, total) = node_catalog_page(&catalog, "", 2);
        assert_eq!(total, 3);
        assert_eq!(page.len(), 2);
        // Type, category and input names all participate in the match.
        let (page, total) = node_catalog_page(&catalog, "text", 50);
        assert_eq!(total, 1);
        assert_eq!(page[0]["type"], json!("CLIPTextEncode"));
        let (page, _) = node_catalog_page(&catalog, "sampling", 50);
        assert_eq!(page[0]["type"], json!("KSampler"));
        let (_, total) = node_catalog_page(&catalog, "nothing-matches", 50);
        assert_eq!(total, 0);
        assert_eq!(node_catalog_page(&json!([1, 2]), "", 5).1, 0);
    }

    #[test]
    fn the_workflow_reader_bounds_and_validates_the_file() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_bounded_workflow(&root.path().join("absent.json")).is_err());
        let bad = root.path().join("bad.json");
        std::fs::write(&bad, b"{").unwrap();
        assert!(read_bounded_workflow(&bad).is_err());
        let good = root.path().join("good.json");
        std::fs::write(
            &good,
            br#"{"6":{"class_type":"CLIPTextEncode","inputs":{"text":"hi"}}}"#,
        )
        .unwrap();
        let graph = read_bounded_workflow(&good).unwrap();
        assert_eq!(graph["6"]["class_type"], json!("CLIPTextEncode"));
    }
}
