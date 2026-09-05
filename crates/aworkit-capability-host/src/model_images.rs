//! Bounded image references and provider-only materialization. Durable requests
//! contain hashes, never image bytes or ambient filesystem paths.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ProviderError;

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_IMAGE_CONTEXT_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_IMAGES: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageAttachmentV1 {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub byte_length: usize,
}

impl ImageAttachmentV1 {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.id.len() != 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.name.trim().is_empty()
            || self.name.len() > 255
            || self.name.chars().any(char::is_control)
            || !matches!(
                self.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || self.byte_length == 0
            || self.byte_length > MAX_IMAGE_BYTES
        {
            return Err(ProviderError::Failed(
                "Invalid image attachment (PNG, JPEG or WebP, up to 5 MiB each)".into(),
            ));
        }
        Ok(())
    }

    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<(), ProviderError> {
        self.validate()?;
        let format_matches = match self.mime_type.as_str() {
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !format_matches
            || bytes.len() != self.byte_length
            || format!("{:x}", Sha256::digest(bytes)) != self.id
        {
            return Err(ProviderError::Failed(
                "Stored image is missing or has changed; attach it again".into(),
            ));
        }
        Ok(())
    }
}

/// Only the trusted desktop composition supplies this resolver. The caller
/// validates the compact request against its frozen authority before resolving.
pub trait ModelImageResolver: Send + Sync {
    fn read(&self, image: &ImageAttachmentV1) -> Result<Vec<u8>, ProviderError>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelImageV1 {
    #[serde(flatten)]
    pub attachment: ImageAttachmentV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

pub fn validate_image_attachments(images: &[ImageAttachmentV1]) -> Result<(), ProviderError> {
    for image in images {
        image.validate()?;
    }
    if images.len() > MAX_IMAGES
        || images.iter().map(|image| image.byte_length).sum::<usize>() > MAX_IMAGE_CONTEXT_BYTES
    {
        return Err(ProviderError::Failed(
            "Image context is limited to 20 images and 12 MiB; remove images or start a new Chat"
                .into(),
        ));
    }
    Ok(())
}

/// Resolve references immediately before provider dispatch, after observers and
/// durable authority checks have seen the original compact request.
pub(crate) fn materialize_images(
    input: &Value,
    resolver: Option<&dyn ModelImageResolver>,
) -> Result<Value, ProviderError> {
    let mut result = input.clone();
    let entries: &mut [Value] = match &mut result {
        Value::Array(entries) => entries,
        Value::Object(object) if object.contains_key("messages") => object
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or(ProviderError::InvalidPlan)?,
        Value::Object(_) => std::slice::from_mut(&mut result),
        _ => return Ok(result),
    };
    let mut all = Vec::new();
    for entry in entries {
        let Some(images) = entry.get_mut("images") else {
            continue;
        };
        let references: Vec<ImageAttachmentV1> =
            serde_json::from_value(images.clone()).map_err(|_| ProviderError::InvalidPlan)?;
        all.extend(references.clone());
        validate_image_attachments(&all)?;
        let mut resolved = Vec::new();
        for attachment in references {
            let bytes = resolver
                .ok_or_else(|| {
                    ProviderError::Failed(
                        "Image storage is unavailable for this model request".into(),
                    )
                })?
                .read(&attachment)?;
            attachment.verify_bytes(&bytes)?;
            resolved.push(ModelImageV1 {
                attachment,
                data: Some(STANDARD.encode(bytes)),
            });
        }
        *images = serde_json::to_value(resolved).map_err(|_| ProviderError::InvalidPlan)?;
    }
    Ok(result)
}

/// Protocol mapping is shared by plain completion and tool-aware requests.
pub(crate) fn image_content(
    text: &str,
    images: &[ModelImageV1],
    protocol: &str,
) -> Result<Value, ProviderError> {
    if images.is_empty() && protocol != "gemini" {
        return Ok(Value::String(text.into()));
    }
    let mut parts = Vec::new();
    for image in images {
        let data = image
            .data
            .as_deref()
            .ok_or_else(|| ProviderError::Failed("Image reference was not materialized".into()))?;
        let mime = &image.attachment.mime_type;
        if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(ProviderError::Failed(
                "Materialized image exceeds its size bound".into(),
            ));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| ProviderError::Failed("Invalid materialized image encoding".into()))?;
        image.attachment.verify_bytes(&bytes)?;
        parts.push(match protocol {
            "openai" => json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}"),"detail":"auto"}}),
            "anthropic" => json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}}),
            "gemini" => json!({"inlineData":{"mimeType":mime,"data":data}}),
            _ => return Err(ProviderError::InvalidPlan),
        });
    }
    if !text.is_empty() {
        parts.push(if protocol == "gemini" {
            json!({"text":text})
        } else {
            json!({"type":"text","text":text})
        });
    }
    Ok(Value::Array(parts))
}
