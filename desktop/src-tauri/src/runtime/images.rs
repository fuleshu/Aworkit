//! Profile-local content-addressed Chat images. Durable records contain only
//! hashes and metadata. Renderer-supplied filesystem paths are never accepted.
use aworkit_capability_host::{
    ProviderError,
    model_images::{
        ImageAttachmentV1, MAX_IMAGE_BYTES, MAX_IMAGE_SOURCE_BYTES, ModelImageCopyV1,
        ModelImageResolver, validate_image_attachments,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
mod source;
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

    /// Imports one local image file the operating system's chooser already
    /// resolved to an absolute path.
    ///
    /// The path never comes from the renderer: only the native dialog supplies
    /// it, which is why this is separate from the base64 import the webview uses.
    pub fn import_path(&self, path: &Path) -> Result<ImageAttachmentV1, String> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Picked image".to_owned());
        let bytes = fs::read(path)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        self.import_bytes(name, &bytes)
    }

    /// Validates actual image bytes before atomically publishing a local blob.
    ///
    /// The stored blob is the original the user chose, byte for byte; a picture
    /// larger than a vision request should carry is reduced for the model when
    /// it is sent, never on the way in.
    pub fn import(&self, name: String, data: String) -> Result<ImageAttachmentV1, String> {
        if data.len() > MAX_IMAGE_SOURCE_BYTES.div_ceil(3) * 4 {
            return Err("Images must be 32 MiB or smaller".into());
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| "Invalid image encoding")?;
        self.import_bytes(name, &bytes)
    }

    /// Import a bounded native file/capture without a base64 round trip.
    pub(crate) fn import_bytes(&self, name: String, bytes: &[u8]) -> Result<ImageAttachmentV1, String> {
        if bytes.len() > MAX_IMAGE_SOURCE_BYTES { return Err("Images must be 32 MiB or smaller".into()); }
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

    /// The bytes a model request should carry for one attachment.
    ///
    /// The stored original is the linked image: the thumbnail, the preview and
    /// the durable reference all keep it. A picture larger than a vision request
    /// should carry is reduced once into a copy cached beside its blob, and that
    /// copy is what the model receives.
    fn model_request_bytes(
        &self,
        image: &ImageAttachmentV1,
    ) -> Result<(Vec<u8>, Option<ModelImageCopyV1>), String> {
        let original = self.read(image).map_err(|error| error.to_string())?;
        if original.len() <= MAX_IMAGE_BYTES {
            return Ok((original, None));
        }
        let copy = self.vision_copy(image, &original)?;
        Ok((copy.bytes, Some(copy.record)))
    }

    /// Loads the vision copy of one oversized stored image, producing it once.
    ///
    /// The cache is a plain file beside the blob (`vision-<id>.<ext>`): it holds
    /// no authority of its own — the copy is verified against the record that
    /// describes it — and a lost race to write it is not a failure, because both
    /// writers produce identical bytes.
    fn vision_copy(
        &self,
        image: &ImageAttachmentV1,
        original: &[u8],
    ) -> Result<StoredVisionCopy, String> {
        let original_dimensions = image_dimensions(&self.root.join(&image.id))?;
        for (extension, mime_type) in [("png", "image/png"), ("jpg", "image/jpeg")] {
            let path = self
                .root
                .join(format!("vision-{}.{extension}", image.id));
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let (width, height) = image_dimensions(&path)?;
            return Ok(StoredVisionCopy {
                record: ModelImageCopyV1 {
                    mime_type: mime_type.to_owned(),
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    byte_length: bytes.len(),
                    width,
                    height,
                    original_byte_length: original.len(),
                    original_width: original_dimensions.0,
                    original_height: original_dimensions.1,
                },
                bytes,
            });
        }
        let copy = source::model_copy(original)?;
        let path = self
            .root
            .join(format!("vision-{}.{}", image.id, copy.extension));
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        temporary
            .write_all(&copy.bytes)
            .map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        let _ = temporary.persist_noclobber(&path);
        Ok(StoredVisionCopy {
            record: ModelImageCopyV1 {
                mime_type: copy.mime_type.to_owned(),
                sha256: format!("{:x}", Sha256::digest(&copy.bytes)),
                byte_length: copy.bytes.len(),
                width: copy.width,
                height: copy.height,
                original_byte_length: original.len(),
                original_width: copy.original_width,
                original_height: copy.original_height,
            },
            bytes: copy.bytes,
        })
    }
}

/// One derived vision copy together with the record that describes it.
struct StoredVisionCopy {
    bytes: Vec<u8>,
    record: ModelImageCopyV1,
}

/// The pixel dimensions of one stored or derived image file.
///
/// A stored blob has no file extension — its name is its hash — so the format is
/// sniffed from the contents instead of the path.
fn image_dimensions(path: &Path) -> Result<(u32, u32), String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())
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
            .take((MAX_IMAGE_SOURCE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        image.verify_bytes(&bytes)?;
        Ok(bytes)
    }

    /// The desktop store owns the reduction: the durable attachment keeps the
    /// original, and only the dispatch copy carries the vision-sized one.
    fn read_for_model(
        &self,
        image: &ImageAttachmentV1,
    ) -> Result<(Vec<u8>, Option<ModelImageCopyV1>), ProviderError> {
        self.model_request_bytes(image)
            .map_err(ProviderError::Failed)
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
