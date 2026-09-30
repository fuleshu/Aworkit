//! Oversized local images are converted into an explicitly described vision
//! copy. Small accepted images keep their original bytes; user files never change.
use super::*;
use aworkit_capability_host::model_images::MAX_IMAGE_SOURCE_BYTES;
use image::{GenericImageView, imageops::FilterType};
use serde_json::json;

/// A vision-sized copy of an image that is too large to send as it is.
pub(crate) struct ModelCopy {
    pub bytes: Vec<u8>,
    /// `image/png` for a transparent picture, `image/jpeg` for an opaque one.
    pub mime_type: &'static str,
    /// File extension matching [`ModelCopy::mime_type`].
    pub extension: &'static str,
    pub width: u32,
    pub height: u32,
    pub original_width: u32,
    pub original_height: u32,
}

/// Reduces one oversized image into a copy a vision request can carry.
///
/// The aspect ratio is preserved: every step shrinks both sides of the picture
/// by the same factor with a Lanczos filter, so the model sees the same image
/// with fewer pixels. An opaque image becomes JPEG — the smaller form for a
/// photograph — and a transparent one stays PNG. This is the rule the local
/// image reader has always used for its oversized sources, so a pasted
/// screenshot and a read file are prepared identically.
pub(crate) fn model_copy(bytes: &[u8]) -> Result<ModelCopy, String> {
    let format = image::guess_format(bytes).map_err(|_| "Choose a PNG, JPEG or WebP image")?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err("Choose a PNG, JPEG or WebP image".into());
    }
    let guard = IMAGE_DECODE
        .lock()
        .map_err(|_| "Image decoder is unavailable")?;
    let mut decoded = decode_image(bytes)?;
    let original = decoded.dimensions();
    let transparent =
        decoded.color().has_alpha() && decoded.to_rgba8().pixels().any(|p| p[3] != 255);
    let encoded = loop {
        let mut encoded = Vec::new();
        if transparent {
            decoded
                .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
                .encode_image(&decoded.to_rgb8())
                .map_err(|e| e.to_string())?;
        }
        if encoded.len() <= MAX_IMAGE_BYTES {
            break encoded;
        }
        let (w, h) = decoded.dimensions();
        if w == 1 && h == 1 {
            return Err("Cannot prepare image within the model image limit".into());
        }
        decoded = decoded.resize((w * 3 / 4).max(1), (h * 3 / 4).max(1), FilterType::Lanczos3);
    };
    let (width, height) = decoded.dimensions();
    drop(decoded);
    drop(guard);
    Ok(ModelCopy {
        bytes: encoded,
        mime_type: if transparent { "image/png" } else { "image/jpeg" },
        extension: if transparent { "png" } else { "jpg" },
        width,
        height,
        original_width: original.0,
        original_height: original.1,
    })
}

impl ChatImageStore {
    pub(crate) fn import_source(
        &self,
        name: String,
        bytes: &[u8],
    ) -> Result<(ImageAttachmentV1, Option<Value>), String> {
        if bytes.len() <= MAX_IMAGE_BYTES {
            return self.import_bytes(name, bytes).map(|image| (image, None));
        }
        if bytes.len() > MAX_IMAGE_SOURCE_BYTES {
            return Err("Source images must be 32 MiB or smaller".into());
        }
        let copy = model_copy(bytes)?;
        let name = Path::new(&name)
            .with_extension(copy.extension)
            .to_string_lossy()
            .into_owned();
        let image = self.import_bytes(name, &copy.bytes)?;
        Ok((
            image,
            Some(
                json!({"originalBytes":bytes.len(),"originalSha256":format!("{:x}",Sha256::digest(bytes)),"originalWidth":copy.original_width,"originalHeight":copy.original_height,"width":copy.width,"height":copy.height,"reencoded":true,"resized":(copy.original_width,copy.original_height)!=(copy.width,copy.height)}),
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_transparent_source_is_resized_with_alpha_and_reported_dimensions() {
        let directory = tempfile::tempdir().unwrap();
        let store = ChatImageStore::new(directory.path());
        let mut seed = 71_u32;
        let pixels = image::RgbaImage::from_fn(1400, 1400, |_, _| {
            let mut rgba = [0; 4];
            for value in &mut rgba {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *value = seed as u8;
            }
            image::Rgba(rgba)
        });
        let mut source = Cursor::new(Vec::new());
        pixels
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        assert!(source.get_ref().len() > MAX_IMAGE_BYTES);
        let (reference, preparation) = store
            .import_source("transparent.png".into(), source.get_ref())
            .unwrap();
        let prepared = preparation.unwrap();
        let result = store.read(&reference).unwrap();
        let decoded = decode_image(&result).unwrap();
        assert!(result.len() <= MAX_IMAGE_BYTES);
        assert_eq!(reference.mime_type, "image/png");
        assert!(decoded.to_rgba8().pixels().any(|pixel| pixel[3] != 255));
        assert!(decoded.width() < 1400 && decoded.height() < 1400);
        assert_eq!(prepared["resized"], true);
        assert_eq!(prepared["originalWidth"], 1400);
        assert_eq!(prepared["originalHeight"], 1400);
        assert_eq!(prepared["width"], decoded.width());
        assert_eq!(prepared["height"], decoded.height());
        assert_eq!(
            prepared["originalSha256"],
            format!("{:x}", Sha256::digest(source.get_ref()))
        );
    }

    /// The original stays the stored and linked image; only what the model
    /// receives is reduced, and that copy describes itself for the notice.
    #[test]
    fn an_oversized_attachment_keeps_the_original_and_sends_a_reduced_copy() {
        let directory = tempfile::tempdir().unwrap();
        let store = ChatImageStore::new(directory.path());
        let mut seed = 29_u32;
        let pixels = image::RgbImage::from_fn(1600, 1200, |_, _| {
            let mut rgb = [0; 3];
            for value in &mut rgb {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *value = seed as u8;
            }
            image::Rgb(rgb)
        });
        let mut source = Cursor::new(Vec::new());
        pixels
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        let original = source.get_ref().clone();
        assert!(original.len() > MAX_IMAGE_BYTES);

        // What the Chat stores, previews and links is the original, byte for byte.
        let reference = store.import_bytes("holiday.png".into(), &original).unwrap();
        assert_eq!(reference.byte_length, original.len());
        assert_eq!(store.read(&reference).unwrap(), original);

        // What the model is sent is a reduced copy, described by its own record.
        let (bytes, copy) = store.model_request_bytes(&reference).unwrap();
        let copy = copy.expect("an oversized image is reduced for the model");
        assert!(bytes.len() <= MAX_IMAGE_BYTES);
        assert_eq!(copy.mime_type, "image/jpeg");
        assert_eq!(copy.original_byte_length, original.len());
        assert_eq!((copy.original_width, copy.original_height), (1600, 1200));
        // A JPEG re-encode of this picture already fits, so the copy keeps the
        // pixel size; the reduction that matters is the one the model sees.
        assert!(copy.width <= 1600 && copy.height <= 1200);
        assert!(bytes.len() < original.len());
        copy.verify(&bytes).unwrap();
        let ratio = copy.width as f64 / copy.height as f64;
        assert!((ratio - 1600.0 / 1200.0).abs() < 0.02, "aspect ratio {ratio}");

        // The copy is produced once and then reused as it is.
        let (again, again_copy) = store.model_request_bytes(&reference).unwrap();
        assert_eq!(again, bytes);
        assert_eq!(again_copy, Some(copy));
    }
}
