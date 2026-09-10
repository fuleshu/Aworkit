//! Oversized local images are converted into an explicitly described vision
//! copy. Small accepted images keep their original bytes; user files never change.
use super::*;
use aworkit_capability_host::model_images::MAX_IMAGE_SOURCE_BYTES;
use image::{GenericImageView, imageops::FilterType};
use serde_json::json;

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
        let name = Path::new(&name)
            .with_extension(if transparent { "png" } else { "jpg" })
            .to_string_lossy()
            .into_owned();
        let image = self.import_bytes(name, &encoded)?;
        Ok((
            image,
            Some(
                json!({"originalBytes":bytes.len(),"originalSha256":format!("{:x}",Sha256::digest(bytes)),"originalWidth":original.0,"originalHeight":original.1,"width":width,"height":height,"reencoded":true,"resized":original!=(width,height)}),
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
}
