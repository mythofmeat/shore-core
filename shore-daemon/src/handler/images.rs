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
