//! Image acquisition uses existing project authority, immutable image storage,
//! tool settlement and vision dispatch. No image bytes enter durable JSON.
use super::*;
use aworkit_capability_host::model_images::ImageAttachmentV1;

pub(super) const READ: &str = "tool.image.read";
pub(super) const SCREENSHOT: &str = "tool.screenshot";

pub(super) fn schema(id: &str) -> Value {
    super::super::tool_registry::native_tool(id)
        .expect("image tool manifest")
        .input_schema
        .clone()
}

pub(super) fn freeze(
    id: &str,
    configuration: &Value,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let tool = super::super::tool_registry::native_tool(id)
        .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
    if *configuration != json!(tool.configuration) {
        return Err(invalid_tool("invalid image tool configuration"));
    }
    Ok((
        tool.provider_name.clone(),
        tool.description.clone(),
        tool.input_schema.clone(),
        if id == READ {
            StoredFileToolLimitV1::ImageRead
        } else {
            StoredFileToolLimitV1::Screenshot
        },
    ))
}

/// Recover image metadata only from these trusted, settled native capabilities.
pub(super) fn result_images(
    id: &str,
    content: &Value,
    failed: bool,
) -> Result<Vec<ImageAttachmentV1>, WorkflowPipelineError> {
    if failed || !matches!(id, READ | SCREENSHOT) {
        return Ok(Vec::new());
    }
    let Some(value) = content.get("image") else {
        return Ok(Vec::new());
    };
    let image: ImageAttachmentV1 =
        serde_json::from_value(value.clone()).map_err(|e| invalid_tool(&e.to_string()))?;
    image.validate().map_err(|e| invalid_tool(&e.to_string()))?;
    Ok(vec![image])
}

impl FileToolDispatcherV1 {
    pub(super) fn acquire_image(
        &self,
        files: &ProjectFiles,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let args = &self.record.call.arguments;
        if cancellation.is_cancelled() {
            return Err("Image acquisition cancelled".into());
        }
        if self.record.call.capability_id == SCREENSHOT && args["operation"] == "list" {
            return super::super::screen_capture::list()
                .map(|value| (value, "Listed screenshot targets.".into()));
        }
        if self.context.model_context["imageInput"] != true {
            return Err("This Chat's model is not configured for vision. Enable Vision for a compatible model and start a new Chat.".into());
        }
        let (name, bytes, source) = if self.record.call.capability_id == READ {
            let path = Path::new(args["path"].as_str().ok_or("Image path is required")?);
            let read = files
                .read_image_v1(path, cancellation)
                .map_err(|e| e.to_string())?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("Invalid image name")?
                .to_owned();
            (name, read.bytes, json!({"path":path}))
        } else {
            let target = args["target"]
                .as_str()
                .ok_or("Select a screenshot target from operation=list")?;
            let capture = super::super::screen_capture::capture(target)?;
            ("screenshot.png".into(), capture.bytes, capture.source)
        };
        if cancellation.is_cancelled() {
            return Err("Image acquisition cancelled".into());
        }
        let image = self.runtime.images.import_bytes(name, &bytes)?;
        let summary = format!("Read image {} ({} bytes).", image.name, image.byte_length);
        Ok((json!({"image":image,"source":source}), summary))
    }
}
