//! Image recognition and header probing for the multimodal read protocol.
//!
//! v7 scope: magic-prefix fast dispatch + header dimension probing only.
//! No decode, no normalization, no re-encoding (phase two uses the same
//! `image` dependency for full decode/transform).

use std::io::Cursor;

use crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits;

/// Raster image formats accepted by the version-one image path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 4] = [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::Webp];

    pub fn mime(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Webp => "image/webp",
        }
    }

    #[allow(dead_code)] // Phase two index/variant layout may persist extensions.
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Gif => "gif",
            ImageFormat::Webp => "webp",
        }
    }

    pub fn from_image_format(format: image::ImageFormat) -> Option<Self> {
        match format {
            image::ImageFormat::Png => Some(ImageFormat::Png),
            image::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
            image::ImageFormat::Gif => Some(ImageFormat::Gif),
            image::ImageFormat::WebP => Some(ImageFormat::Webp),
            _ => None,
        }
    }
}

/// Header facts about one accepted image (encoded dimensions; no EXIF
/// transpose in phase one — phase two normalization owns that).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

impl ImageInfo {
    pub fn mime(&self) -> &'static str {
        self.format.mime()
    }
}

/// Fast dispatch prefix check (first 12 bytes). Header probing via the
/// `image` crate remains authoritative for size extraction; a magic miss
/// routes the file into the ordinary text path unchanged.
pub(crate) fn magic_matches(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const JPEG: [u8; 3] = [0xFF, 0xD8, 0xFF];
    bytes.starts_with(&PNG)
        || bytes.starts_with(&JPEG)
        || bytes.starts_with(b"GIF")
        || (bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP")
}

/// Probe one buffer: magic dispatch, then header-only dimension extraction.
///
/// Returns `None` when the bytes are not a supported raster image or the
/// header cannot be parsed. Callers treat a probe miss as "not an image"
/// (text path), while a supported-format failure after magic hit is
/// `INVALID_IMAGE` (fail closed).
pub fn probe(bytes: &[u8]) -> Option<ImageInfo> {
    if !magic_matches(bytes) {
        return None;
    }
    let reader = image::ImageReader::new(Cursor::new(bytes));
    let reader = reader.with_guessed_format().ok()?;
    let format = ImageFormat::from_image_format(reader.format()?)?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(ImageInfo { format, width, height })
}

/// `width as u64 * height as u64` with overflow rejection.
pub fn checked_pixel_count(width: u32, height: u32) -> Option<u64> {
    u64::from(width).checked_mul(u64::from(height))
}

/// Structured image capture attached to a successful Read outcome
/// (v7 §7.1). The bytes live in the home image cache; the conversation only
/// carries `image_id` plus budget metadata.
#[derive(Debug, Clone)]
pub struct ImageAttachment {
    pub image_id: String,
    pub(crate) format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// Basename stripped of path information; process-local / summary only.
    pub name: String,
}

impl ImageAttachment {
    pub fn mime(&self) -> &'static str {
        self.format.mime()
    }

    /// Model-facing text summary shown beside the injected image.
    pub fn summary(&self, display_path: &str) -> String {
        format!(
            "Image: {}x{} {} ({}) — {}\n[The image will be attached to the next model request.]",
            self.width,
            self.height,
            self.format.mime(),
            format_bytes(self.bytes),
            display_path
        )
    }
}

/// Per-turn image quota derived from the active projection actually sent to
/// the model (v7 §7.3). Same image id repeated in multiple attachment blocks
/// counts per block.
#[derive(Debug, Clone, Default)]
pub struct ImageQuotaState {
    pub used_images: usize,
    pub used_bytes: u64,
}

impl ImageQuotaState {
    #[allow(dead_code)] // Convenience entry without cache awareness; tests use it.
    pub fn from_messages(messages: &[serde_json::Value]) -> Self {
        Self::from_messages_with_cache(messages, None, None)
    }

    /// Count only attachments that will actually be sent (review fix): each
    /// block is verified against the cache — object must exist, pass digest
    /// verification, probe as a supported format, and be in the model's
    /// allowed MIME set — and the counted bytes are the real object size,
    /// not the declared (possibly forged) value. A block that would degrade
    /// at materialization never consumes quota.
    pub fn from_messages_with_cache(
        messages: &[serde_json::Value],
        cache: Option<&crate::session::image_cache::ImageCache>,
        limits: Option<&crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits>,
    ) -> Self {
        let mut used_images = 0usize;
        let mut used_bytes = 0u64;
        for message in messages {
            let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for block in blocks {
                if block.get("type").and_then(serde_json::Value::as_str)
                    != Some("tool_attachment")
                {
                    continue;
                }
                let url = block.get("url").and_then(serde_json::Value::as_str).unwrap_or("");
                let Some(id) = url.strip_prefix("image://") else {
                    continue;
                };
                let (Some(cache), Some(limits)) = (cache, limits) else {
                    // No cache: fall back to the declared facts (legacy path).
                    used_images = used_images.saturating_add(1);
                    used_bytes = used_bytes.saturating_add(
                        block.get("bytes").and_then(serde_json::Value::as_u64).unwrap_or(0),
                    );
                    continue;
                };
                // Full sendability check mirrors materialization: digest,
                // probe, MIME. Object bytes are authoritative for the count.
                let Ok(Some(bytes)) = cache.read_bounded(id, limits.max_image_bytes) else {
                    continue;
                };
                let Some(info) = crate::tools::image::probe(&bytes) else {
                    continue;
                };
                if !limits.allowed_mime.contains(&info.format) {
                    continue;
                }
                used_images = used_images.saturating_add(1);
                used_bytes = used_bytes.saturating_add(bytes.len() as u64);
            }
        }
        Self {
            used_images,
            used_bytes,
        }
    }

    pub fn remaining(&self, limits: &OpenAiChatImageUrlLimits) -> (usize, u64) {
        (
            limits.max_images_per_request.saturating_sub(self.used_images),
            limits
                .max_image_bytes_per_request
                .saturating_sub(self.used_bytes),
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 255) as u8, (y % 255) as u8, 128, 255]);
        }
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode png fixture");
        out
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut img = image::RgbImage::new(16, 8);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 16) as u8, (y * 32) as u8, 64]);
        }
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .expect("encode jpeg fixture");
        out
    }

    #[test]
    fn png_dimensions_from_header() {
        let info = probe(&png_bytes(1024, 768)).expect("png probe");
        assert_eq!(info.format, ImageFormat::Png);
        assert_eq!((info.width, info.height), (1024, 768));
        assert_eq!(info.mime(), "image/png");
    }

    #[test]
    fn jpeg_dimensions_from_sof() {
        let info = probe(&jpeg_bytes()).expect("jpeg probe");
        assert_eq!(info.format, ImageFormat::Jpeg);
        assert_eq!((info.width, info.height), (16, 8));
    }

    #[test]
    fn gif_and_webp_magic_dispatch() {
        // Real GIF via the image crate encoder: dimensions parse from the
        // logical screen descriptor.
        let mut gif_img = image::RgbaImage::new(32, 16);
        let mut gif = Vec::new();
        gif_img
            .write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
            .expect("encode gif fixture");
        let info = probe(&gif).expect("gif probe");
        assert_eq!(info.format, ImageFormat::Gif);
        assert_eq!((info.width, info.height), (32, 16));

        // WebP with a valid RIFF header but truncated body: header probe
        // fails closed (dimensions unknown), which is the phase-one contract.
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&20u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(&[0u8; 8]);
        assert!(probe(&webp).is_none());
    }

    #[test]
    fn text_bytes_do_not_match() {
        assert!(probe(b"hello world, this is text".as_slice()).is_none());
        assert!(probe(b"").is_none());
        assert!(probe(b"short").is_none());
    }

    #[test]
    fn truncated_png_header_fails_closed() {
        // Truncating inside IHDR (well before any pixel data) must fail the
        // header probe; truncating the tail does not, because phase one only
        // reads the header.
        let full = png_bytes(64, 64);
        let mut bytes = full[..20].to_vec(); // signature + partial IHDR
        assert!(probe(&bytes).is_none());
        let tail = full[..full.len() - 5].to_vec();
        assert!(probe(&tail).is_some());
    }

    #[test]
    fn checked_pixel_count_computes_without_overflow() {
        // u32 x u32 always fits u64, so the checked product is total; the
        // guard exists for the pixel-limit comparison itself.
        assert_eq!(checked_pixel_count(u32::MAX, 2), Some(8_589_934_590));
        assert_eq!(checked_pixel_count(1024, 768), Some(786_432));
        assert_eq!(checked_pixel_count(u32::MAX, u32::MAX), Some(18_446_744_065_119_617_025));
    }

    #[test]
    fn format_mime_and_extension_roundtrip() {
        for format in ImageFormat::ALL {
            assert!(!format.mime().is_empty());
            assert!(!format.extension().is_empty());
        }
    }
}

#[cfg(test)]
mod quota_precision_tests {
    use super::*;
    use crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits;
    use crate::session::image_cache::ImageCache;
    use serde_json::json;

    fn block(id: &str, bytes: u64) -> serde_json::Value {
        json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 1, "height": 1, "bytes": bytes})
    }

    #[test]
    fn damaged_objects_do_not_consume_quota() {
        let home = std::env::temp_dir().join(format!(
            "mink-quota-prec-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let id = cache.commit(b"payload-bytes").unwrap();
        // Corrupt the object: it would degrade at materialization.
        let path = home.join(".mink/cache/images/v1/objects").join(&id["sha256:".len()..][..2]).join(&id["sha256:".len()..]);
        std::fs::write(path, b"tampered").unwrap();
        let messages = vec![json!({"role": "user", "content": [block(&id, 100)]})];
        let state = ImageQuotaState::from_messages_with_cache(
            &messages,
            Some(&cache),
            Some(&OpenAiChatImageUrlLimits::default()),
        );
        assert_eq!(state.used_images, 0, "damaged object must not count");
        assert_eq!(state.used_bytes, 0);
        let _ = std::fs::remove_dir_all(&home);
    }
}
