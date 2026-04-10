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
/// (base64-encoded, resized if over `max_image_size`) followed by a text block.
/// Otherwise returns a plain string. Pass `0` for `max_image_size` to disable resizing.
pub(crate) fn build_content(text: &str, images: &[ImageRef], max_image_size: u64, cache_dir: &std::path::Path) -> Value {
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
                let (final_bytes, final_media_type) =
                    if let Some((resized, mt)) = super::resize::cached_resize(&img.path, &bytes, media_type, max_image_size, cache_dir) {
                        (resized, mt)
                    } else {
                        (bytes, media_type)
                    };
                let encoded = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
                blocks.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": final_media_type,
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

/// Encode a single image to a JSON block for the LLM API, resizing if needed.
pub(crate) fn encode_image_block(img: &ImageRef, max_image_size: u64, cache_dir: &std::path::Path) -> Option<Value> {
    let media_type = media_type_for_path(&img.path)?;
    match std::fs::read(&img.path) {
        Ok(bytes) => {
            let (final_bytes, final_media_type) =
                if let Some((resized, mt)) = super::resize::cached_resize(&img.path, &bytes, media_type, max_image_size, cache_dir) {
                    (resized, mt)
                } else {
                    (bytes, media_type)
                };
            let encoded = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
            Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": final_media_type,
                    "data": encoded,
                }
            }))
        }
        Err(e) => {
            warn!(path = %img.path, error = %e, "Failed to read image file for LLM");
            None
        }
    }
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

    // ── build_content integration ──────────────────────────────────────

    #[test]
    fn build_content_resizes_oversized_image_on_disk() {
        use base64::Engine;
        use shore_protocol::types::ImageRef;

        let tmp = tempfile::TempDir::new().unwrap();

        // Write a large noisy JPEG to disk (~3-4 MB at 4000x3000).
        let big_jpeg = make_noisy_jpeg(4000, 3000);
        let img_path = tmp.path().join("big.jpg");
        std::fs::write(&img_path, &big_jpeg).unwrap();
        let original_size = big_jpeg.len();
        assert!(
            original_size > 2_000_000,
            "Test image should exceed 2MB, got {} bytes",
            original_size
        );

        let images = vec![ImageRef {
            path: img_path.to_str().unwrap().to_string(),
            caption: None,
            data: None,
        }];

        // Call build_content with a 2MB limit.
        let result = build_content("describe this", &images, 2_000_000, tmp.path());
        let blocks = result.as_array().expect("Should be a JSON array");
        assert_eq!(blocks.len(), 2, "image block + text block");

        // Image block should be resized JPEG.
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/jpeg");

        // Decode the base64 and verify the raw bytes are under 2MB.
        let b64_data = blocks[0]["source"]["data"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .unwrap();
        assert!(
            decoded.len() < 2_000_000,
            "Resized image should be under 2MB, got {} bytes (original was {})",
            decoded.len(),
            original_size
        );

        // Verify it's valid JPEG (starts with FFD8).
        assert_eq!(&decoded[..2], &[0xFF, 0xD8], "Should be valid JPEG");
    }

    #[test]
    fn build_content_does_not_resize_small_image() {
        use base64::Engine;
        use shore_protocol::types::ImageRef;

        let tmp = tempfile::TempDir::new().unwrap();

        // Write a small JPEG to disk.
        let small_jpeg = make_jpeg(200, 200);
        let img_path = tmp.path().join("small.jpg");
        std::fs::write(&img_path, &small_jpeg).unwrap();
        assert!(small_jpeg.len() < 2_000_000);

        let images = vec![ImageRef {
            path: img_path.to_str().unwrap().to_string(),
            caption: None,
            data: None,
        }];

        let result = build_content("test", &images, 2_000_000, tmp.path());
        let blocks = result.as_array().unwrap();

        // Should still be image/jpeg (not re-encoded).
        assert_eq!(blocks[0]["source"]["media_type"], "image/jpeg");

        // Base64 should decode to the exact original bytes (no resize).
        let b64_data = blocks[0]["source"]["data"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .unwrap();
        assert_eq!(decoded.len(), small_jpeg.len(), "Small image should pass through unchanged");
    }

    #[test]
    fn build_content_resizes_oversized_png_to_jpeg() {
        use base64::Engine;
        use shore_protocol::types::ImageRef;

        let tmp = tempfile::TempDir::new().unwrap();

        // Write a large noisy PNG to disk.
        let big_png = make_noisy_png(3000, 2000);
        let img_path = tmp.path().join("big.png");
        std::fs::write(&img_path, &big_png).unwrap();
        let original_size = big_png.len();
        assert!(
            original_size > 2_000_000,
            "Test PNG should exceed 2MB, got {} bytes",
            original_size
        );

        let images = vec![ImageRef {
            path: img_path.to_str().unwrap().to_string(),
            caption: None,
            data: None,
        }];

        let result = build_content("describe", &images, 2_000_000, tmp.path());
        let blocks = result.as_array().unwrap();

        // Should be converted to JPEG.
        assert_eq!(blocks[0]["source"]["media_type"], "image/jpeg");

        let b64_data = blocks[0]["source"]["data"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .unwrap();
        assert!(
            decoded.len() < 2_000_000,
            "Resized PNG→JPEG should be under 2MB, got {} bytes",
            decoded.len()
        );
        assert_eq!(&decoded[..2], &[0xFF, 0xD8], "Should be valid JPEG");
    }
}
