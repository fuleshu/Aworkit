//! Profile-local content-addressed Chat images. Durable records contain only
//! hashes and metadata. Renderer-supplied filesystem paths are never accepted.
use aworkit_capability_host::{
    ProviderError,
    model_images::{
        ImageAttachmentV1, MAX_IMAGE_BYTES, ModelImageResolver, validate_image_attachments,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

// Serialize image decoding so opening a long transcript cannot allocate twenty
// full-resolution bitmaps at once. I/O stays off the WebView/runtime thread.
static IMAGE_DECODE: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct ChatImageStore {
    root: PathBuf,
}

impl ChatImageStore {
    pub fn new(profile: &Path) -> Self {
        Self {
            root: profile.join("images"),
        }
    }

    /// Validates actual image bytes before atomically publishing a local blob.
    pub fn import(&self, name: String, data: String) -> Result<ImageAttachmentV1, String> {
        if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("Images must be 5 MiB or smaller".into());
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| "Invalid image encoding")?;
        let format = image::guess_format(&bytes).map_err(|_| "Choose a PNG, JPEG or WebP image")?;
        let mime_type = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            image::ImageFormat::WebP => "image/webp",
            _ => return Err("Choose a PNG, JPEG or WebP image".into()),
        };
        let attachment = ImageAttachmentV1 {
            id: format!("{:x}", Sha256::digest(&bytes)),
            name,
            mime_type: mime_type.into(),
            byte_length: bytes.len(),
        };
        attachment.validate().map_err(|e| e.to_string())?;
        let decode_guard = IMAGE_DECODE
            .lock()
            .map_err(|_| "Image decoder is unavailable")?;
        decode_image(&bytes)?;
        drop(decode_guard);
        fs::create_dir_all(&self.root).map_err(|e| format!("Cannot create image storage: {e}"))?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        temporary.write_all(&bytes).map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        match temporary.persist_noclobber(self.root.join(&attachment.id)) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.read(&attachment).map_err(|e| e.to_string())?;
            }
            Err(error) => return Err(format!("Cannot save image: {}", error.error)),
        }
        Ok(attachment)
    }

    pub fn preview(&self, image: &ImageAttachmentV1) -> Result<String, String> {
        let bytes = self.read(image).map_err(|e| e.to_string())?;
        Ok(format!(
            "data:{};base64,{}",
            image.mime_type,
            STANDARD.encode(bytes)
        ))
    }

    pub fn thumbnail(&self, image: &ImageAttachmentV1) -> Result<String, String> {
        let _decode = IMAGE_DECODE
            .lock()
            .map_err(|_| "Image decoder is unavailable")?;
        let bytes = self.read(image).map_err(|e| e.to_string())?;
        let thumbnail = decode_image(&bytes)?.thumbnail(256, 192);
        let mut encoded = Cursor::new(Vec::new());
        thumbnail
            .write_to(&mut encoded, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "data:image/png;base64,{}",
            STANDARD.encode(encoded.into_inner())
        ))
    }
}

fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8000);
    limits.max_image_height = Some(8000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| "Image is corrupt or exceeds 8000 pixels per side".into())
}

impl ModelImageResolver for ChatImageStore {
    fn read(&self, image: &ImageAttachmentV1) -> Result<Vec<u8>, ProviderError> {
        image.validate()?;
        let path = self.root.join(&image.id);
        let unavailable = || {
            ProviderError::Failed(format!(
                "Image '{}' is missing or unreadable; attach it again",
                image.name
            ))
        };
        let metadata = fs::symlink_metadata(&path).map_err(|_| unavailable())?;
        if !metadata.is_file() || metadata.len() != image.byte_length as u64 {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|_| unavailable())?
            .take((MAX_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        image.verify_bytes(&bytes)?;
        Ok(bytes)
    }
}

/// Shared live-command and recovery validation, including image-only inputs.
pub(crate) fn command_images(payload: &Value) -> Result<Vec<ImageAttachmentV1>, String> {
    let images = payload
        .get("attachments")
        .map(|value| serde_json::from_value::<Vec<ImageAttachmentV1>>(value.clone()))
        .transpose()
        .map_err(|_| "Invalid image attachment references")?
        .unwrap_or_default();
    validate_image_attachments(&images).map_err(|e| e.to_string())?;
    Ok(images)
}

pub(crate) fn command_text(payload: &Value) -> Result<String, String> {
    let images = command_images(payload)?;
    let text = payload
        .get("input")
        .and_then(Value::as_str)
        .ok_or("Chat input must be text")?;
    if (text.trim().is_empty() && images.is_empty())
        || text.len() > 128 * 1024
        || text.contains('\0')
    {
        return Err("Enter a message or add an image (text limit: 128 KiB)".into());
    }
    Ok(text.into())
}
