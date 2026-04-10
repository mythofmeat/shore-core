//! Image processing helpers for the handler pipeline.
//!
//! Contains MIME detection, content building, image ingestion (base64 uploads
//! and legacy filesystem paths), and wire-embedding of image data.

use base64::Engine as _;
use serde_json::{json, Value};
use shore_protocol::types::{ContentBlock, ImageRef};
use tracing::{info, warn};

/// Detect MIME type from file extension.
pub(crate) fn media_type_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Build a `content` value for an LLM message.
///
/// If `images` is non-empty, returns a JSON array containing image blocks
/// (base64-encoded) followed by a text block. Otherwise returns a plain string.
pub(crate) fn build_content(text: &str, images: &[ImageRef]) -> Value {
    if images.is_empty() {
        return json!(text);
    }

    let mut blocks: Vec<Value> = Vec::with_capacity(images.len() + 1);

    for img in images {
        let media_type = match media_type_for_path(&img.path) {
            Some(mt) => mt,
            None => {
                warn!(path = %img.path, "Skipping image with unsupported extension");
                continue;
            }
        };
        match std::fs::read(&img.path) {
            Ok(bytes) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                blocks.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": encoded,
                    }
                }));
            }
            Err(e) => {
                warn!(path = %img.path, error = %e, "Failed to read image file");
            }
        }
    }

    blocks.push(json!({ "type": "text", "text": text }));
    json!(blocks)
}

/// Ingest incoming images to durable attachments/ directory.
///
/// Prefers `image_data` (base64-encoded uploads from new clients) over
/// `image_paths` (legacy path-based, same-machine only).
/// Returns (persisted ImageRefs, annotation ContentBlocks).
pub(super) fn ingest_images(
    data_dir: &std::path::Path,
    char_name: &str,
    image_paths: &[String],
    image_data: &[shore_protocol::client_msg::ImageUpload],
) -> (Vec<ImageRef>, Vec<ContentBlock>) {
    use base64::Engine;

    let character_data_dir = data_dir.join(char_name);
    let attachments_dir = character_data_dir.join("images").join("attachments");
    let mut images: Vec<ImageRef> = Vec::with_capacity(image_data.len() + image_paths.len());
    let mut content_blocks: Vec<ContentBlock> = Vec::new();

    // Preferred path: base64-encoded uploads (works across machines).
    for upload in image_data {
        if let Err(e) = std::fs::create_dir_all(&attachments_dir) {
            warn!(error = %e, "Failed to create attachments directory");
            continue;
        }
        let bytes = match base64::engine::general_purpose::STANDARD.decode(&upload.data) {
            Ok(b) => b,
            Err(e) => {
                warn!(filename = %upload.filename, error = %e, "Failed to decode base64 image data");
                continue;
            }
        };
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let dest_name = format!("{timestamp}_{}", upload.filename);
        let dest_path = attachments_dir.join(&dest_name);

        match std::fs::write(&dest_path, &bytes) {
            Ok(()) => {
                let abs_path = dest_path.to_string_lossy().to_string();
                let rel_path = format!("attachments/{dest_name}");
                images.push(ImageRef {
                    path: abs_path,
                    caption: None,
                    data: None,
                });
                content_blocks.push(ContentBlock::Text {
                    text: format!("[Attached image saved as: {rel_path}]"),
                });
                info!(filename = %upload.filename, dest = %rel_path, "Saved uploaded image to attachments");
            }
            Err(e) => {
                warn!(filename = %upload.filename, error = %e, "Failed to write image to attachments");
            }
        }
    }

    // Legacy fallback: copy from filesystem paths (same-machine only).
    if image_data.is_empty() {
        for src_path_str in image_paths {
            let src_path = std::path::Path::new(src_path_str);
            if !src_path.exists() {
                warn!(path = %src_path_str, "Skipping non-existent image");
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&attachments_dir) {
                warn!(error = %e, "Failed to create attachments directory");
                continue;
            }
            let original_name = src_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "image".to_string());
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let dest_name = format!("{timestamp}_{original_name}");
            let dest_path = attachments_dir.join(&dest_name);

            match std::fs::copy(src_path, &dest_path) {
                Ok(_) => {
                    let abs_path = dest_path.to_string_lossy().to_string();
                    let rel_path = format!("attachments/{dest_name}");
                    images.push(ImageRef {
                        path: abs_path,
                        caption: None,
                        data: None,
                    });
                    content_blocks.push(ContentBlock::Text {
                        text: format!("[Attached image saved as: {rel_path}]"),
                    });
                    info!(src = %src_path_str, dest = %rel_path, "Copied incoming image to attachments");
                }
                Err(e) => {
                    warn!(src = %src_path_str, error = %e, "Failed to copy image to attachments");
                    images.push(ImageRef {
                        path: src_path_str.clone(),
                        caption: None,
                        data: None,
                    });
                }
            }
        }
    }

    (images, content_blocks)
}

/// Populate the `data` field on ImageRefs by reading and base64-encoding files.
/// Called before sending Messages over the wire so clients can display images
/// without needing filesystem access to the server's paths.
pub(crate) fn embed_image_data(images: &mut [ImageRef]) {
    use base64::Engine;
    for img in images {
        if img.data.is_some() {
            continue;
        }
        match std::fs::read(&img.path) {
            Ok(bytes) => {
                img.data = Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
            }
            Err(e) => {
                warn!(path = %img.path, error = %e, "Failed to read image for wire embedding");
            }
        }
    }
}

/// Resize an image if it exceeds `max_bytes`.
///
/// Returns `Some((resized_bytes, media_type))` if the image was resized,
/// or `None` if it fits within the limit (or resizing is disabled/unsupported).
/// Oversized JPEG/PNG/WebP are re-encoded as JPEG at quality 85.
/// GIFs are passed through unchanged (animated GIF resizing is unsupported).
pub(crate) fn maybe_resize(
    bytes: &[u8],
    media_type: &str,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    if max_bytes == 0 || (bytes.len() as u64) <= max_bytes {
        return None;
    }

    // GIF: pass through (animated GIF support is limited).
    if media_type == "image/gif" {
        warn!(
            size = bytes.len(),
            max = max_bytes,
            "GIF exceeds max_image_size but resizing is not supported; sending as-is"
        );
        return None;
    }

    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        Err(e) => {
            warn!(error = %e, "Failed to decode image for resizing; sending original");
            return None;
        }
    };

    let ratio = max_bytes as f64 / bytes.len() as f64;
    let scale = ratio.sqrt() * 0.9; // conservative safety margin
    let new_width = ((img.width() as f64) * scale).max(1.0) as u32;
    let new_height = ((img.height() as f64) * scale).max(1.0) as u32;

    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);

    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    if let Err(e) = resized.write_to(&mut cursor, image::ImageFormat::Jpeg) {
        warn!(error = %e, "Failed to re-encode resized image; sending original");
        return None;
    }

    info!(
        original_size = bytes.len(),
        resized_size = buf.len(),
        original_dims = format!("{}x{}", img.width(), img.height()),
        resized_dims = format!("{}x{}", resized.width(), resized.height()),
        "Resized image for LLM upload"
    );

    Some((buf, "image/jpeg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a valid JPEG image with pseudo-random pixels (high entropy, resists compression).
    ///
    /// Pseudo-random pixels prevent JPEG/PNG from compressing the image to near-zero,
    /// ensuring the resulting file is large enough for the resize tests to be meaningful.
    fn make_noisy_jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (width * height * 3) as usize];
        // Simple LCG to fill with pseudo-random values without pulling in rand.
        let mut state: u64 = 0xdeadbeef_cafebabe;
        for byte in &mut pixels {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (state >> 33) as u8;
        }
        let img = image::RgbImage::from_raw(width, height, pixels).unwrap();
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    /// Create a valid JPEG image of the given dimensions filled with a solid color.
    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([128, 64, 200]));
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    /// Create a valid PNG image with pseudo-random pixels (high entropy).
    fn make_noisy_png(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (width * height * 3) as usize];
        let mut state: u64 = 0xcafe_f00d_1234_5678;
        for byte in &mut pixels {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (state >> 33) as u8;
        }
        let img = image::RgbImage::from_raw(width, height, pixels).unwrap();
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn maybe_resize_returns_none_when_under_limit() {
        let jpeg = make_jpeg(100, 100);
        assert!(maybe_resize(&jpeg, "image/jpeg", 10_000_000).is_none());
    }

    #[test]
    fn maybe_resize_returns_none_when_disabled() {
        let jpeg = make_jpeg(100, 100);
        assert!(maybe_resize(&jpeg, "image/jpeg", 0).is_none());
    }

    #[test]
    fn maybe_resize_shrinks_oversized_jpeg() {
        // Noisy pixels resist JPEG compression; 2000x2000 random JPEG is reliably large.
        let jpeg = make_noisy_jpeg(2000, 2000);
        assert!(
            jpeg.len() > 100_000,
            "Noisy JPEG should be large, got {} bytes",
            jpeg.len()
        );

        // Set max to half the actual size — the function must produce output under that.
        let max = (jpeg.len() as u64) / 2;
        let result = maybe_resize(&jpeg, "image/jpeg", max);
        assert!(result.is_some(), "Should resize oversized JPEG");

        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert!(
            (resized.len() as u64) < max,
            "Resized image ({}) should be under limit ({})",
            resized.len(),
            max
        );
    }

    #[test]
    fn maybe_resize_converts_png_to_jpeg() {
        // Noisy pixels give a large PNG; max is set to 25% of original so resize is triggered.
        let png = make_noisy_png(1000, 1000);
        assert!(
            png.len() > 50_000,
            "Noisy PNG should be large, got {} bytes",
            png.len()
        );
        let max = (png.len() as u64) / 4;

        let result = maybe_resize(&png, "image/png", max);
        assert!(result.is_some(), "Should resize oversized PNG");

        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert!(
            (resized.len() as u64) < max,
            "Resized image ({}) should be under limit ({})",
            resized.len(),
            max
        );
    }

    #[test]
    fn maybe_resize_passes_through_gif() {
        let fake_gif = vec![0u8; 1_000_000];
        assert!(maybe_resize(&fake_gif, "image/gif", 100).is_none());
    }

    #[test]
    fn maybe_resize_handles_invalid_image_data() {
        let garbage = vec![0u8; 1_000_000];
        assert!(maybe_resize(&garbage, "image/jpeg", 100).is_none());
    }
}
