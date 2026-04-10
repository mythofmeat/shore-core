# Image Resize Before LLM Upload — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resize images that exceed a configurable size limit (default 2MB) before sending them to LLM APIs, converting oversized JPEG/PNG/WebP to JPEG at reduced dimensions.

**Architecture:** A `maybe_resize()` function in `shore-daemon/src/handler/images.rs` decodes oversized images, scales dimensions by `sqrt(target/actual) * 0.9`, and re-encodes as JPEG at quality 85. The max size is configured via `max_image_size` on `AdvancedConfig` (default 2,000,000 bytes, 0 disables). Both `build_content()` and the inline image encoding in `build_llm_messages()` call through `maybe_resize()`.

**Tech Stack:** Rust `image` crate (v0.25, already used in `shore-tui`), JPEG encoding via `image::codecs::jpeg::JpegEncoder`.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `shore-config/src/app.rs` | Modify | Add `max_image_size: Option<u64>` to `AdvancedConfig` |
| `shore-daemon/Cargo.toml` | Modify | Add `image` dependency |
| `shore-daemon/src/handler/images.rs` | Modify | Add `maybe_resize()`, update `build_content()` signature |
| `shore-daemon/src/handler/mod.rs` | Modify | Thread `max_image_size` into `build_content()` and inline block, extract inline block into helper |
| `shore-daemon/src/autonomy/manager.rs` | Modify | Thread `max_image_size` into `build_llm_messages()` call |
| `examples/config.toml` | Modify | Document `max_image_size` |

---

### Task 1: Add `max_image_size` to `AdvancedConfig`

**Files:**
- Modify: `shore-config/src/app.rs:682-713` (AdvancedConfig struct + Default impl)

- [ ] **Step 1: Add the field to `AdvancedConfig`**

In `shore-config/src/app.rs`, add a `max_image_size` field to `AdvancedConfig` and its `Default` impl. Add the serde default function.

Add the serde_default macro before the struct:

```rust
serde_default!(default_max_image_size -> u64 { 2_000_000 });
```

Add to the `AdvancedConfig` struct (after `retry_backoff`):

```rust
    /// Maximum image file size (bytes) before resizing for LLM upload.
    /// Images larger than this are scaled down and re-encoded as JPEG.
    /// Set to 0 to disable resizing. Default: 2,000,000 (2 MB).
    #[serde(default = "default_max_image_size")]
    pub max_image_size: u64,
```

Add to the `Default` impl:

```rust
    max_image_size: default_max_image_size(),
```

- [ ] **Step 2: Update existing tests to include the new field**

No existing tests assert on `max_image_size` directly, but `defaults_are_sensible` asserts on the full `AdvancedConfig`. Verify this test still passes by running:

```bash
cargo test -p shore-config -- defaults_are_sensible
```

Expected: PASS (serde default fills the field automatically).

- [ ] **Step 3: Add a config parse test**

Add a test in the existing `mod tests` block in `shore-config/src/app.rs`:

```rust
    #[test]
    fn max_image_size_defaults_and_overrides() {
        // Default: 2 MB.
        let config = AppConfig::default();
        assert_eq!(config.advanced.max_image_size, 2_000_000);

        // Override via TOML.
        let toml_str = r#"
[advanced]
max_image_size = 5000000
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.advanced.max_image_size, 5_000_000);

        // Disable via 0.
        let toml_str = r#"
[advanced]
max_image_size = 0
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.advanced.max_image_size, 0);
    }
```

- [ ] **Step 4: Run config tests**

```bash
cargo test -p shore-config
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add shore-config/src/app.rs
git commit -m "feat(config): add max_image_size to AdvancedConfig (default 2MB)"
```

---

### Task 2: Add `image` crate dependency to `shore-daemon`

**Files:**
- Modify: `shore-daemon/Cargo.toml`

- [ ] **Step 1: Add the dependency**

Add to `[dependencies]` in `shore-daemon/Cargo.toml`:

```toml
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "webp"] }
```

This matches the version and feature set used by `shore-tui`.

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p shore-daemon
```

Expected: success (no code uses it yet).

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/Cargo.toml
git commit -m "build(daemon): add image crate for LLM upload resizing"
```

---

### Task 3: Implement `maybe_resize()` with tests

**Files:**
- Modify: `shore-daemon/src/handler/images.rs:1-187`

- [ ] **Step 1: Write the `maybe_resize()` function**

Add at the end of `shore-daemon/src/handler/images.rs` (before the closing, there is no closing — the file ends at line 187), add:

```rust
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
```

- [ ] **Step 2: Write unit tests for `maybe_resize()`**

Add a `#[cfg(test)]` module at the bottom of `shore-daemon/src/handler/images.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Create a valid JPEG image of the given dimensions filled with a solid color.
    fn make_jpeg(width: u32, height: u32, quality: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([128, 64, 200]));
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    /// Create a valid PNG image of the given dimensions.
    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([128, 64, 200]));
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn maybe_resize_returns_none_when_under_limit() {
        let jpeg = make_jpeg(100, 100, 85);
        assert!(maybe_resize(&jpeg, "image/jpeg", 10_000_000).is_none());
    }

    #[test]
    fn maybe_resize_returns_none_when_disabled() {
        let jpeg = make_jpeg(100, 100, 85);
        assert!(maybe_resize(&jpeg, "image/jpeg", 0).is_none());
    }

    #[test]
    fn maybe_resize_shrinks_oversized_jpeg() {
        // Create a large-ish JPEG (high quality, big dimensions).
        let jpeg = make_jpeg(4000, 3000, 100);
        assert!(jpeg.len() > 500_000, "Test JPEG should be large");

        let max = 500_000_u64;
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
        let png = make_png(2000, 2000);
        let max = (png.len() as u64) / 2; // force resize

        let result = maybe_resize(&png, "image/png", max);
        assert!(result.is_some(), "Should resize oversized PNG");

        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert!((resized.len() as u64) < max);
    }

    #[test]
    fn maybe_resize_passes_through_gif() {
        // GIF should always return None (not resized).
        let fake_gif = vec![0u8; 1_000_000];
        assert!(maybe_resize(&fake_gif, "image/gif", 100).is_none());
    }

    #[test]
    fn maybe_resize_handles_invalid_image_data() {
        let garbage = vec![0u8; 1_000_000];
        assert!(maybe_resize(&garbage, "image/jpeg", 100).is_none());
    }
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p shore-daemon -- handler::images::tests
```

Expected: all 6 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/src/handler/images.rs
git commit -m "feat(daemon): add maybe_resize() for oversized LLM image uploads"
```

---

### Task 4: Integrate `maybe_resize()` into `build_content()` and `build_llm_messages()`

**Files:**
- Modify: `shore-daemon/src/handler/images.rs:27-62` (`build_content` signature)
- Modify: `shore-daemon/src/handler/mod.rs:14` (re-export), `mod.rs:743-794` (`build_llm_messages`), `mod.rs:760-778` (inline image block)
- Modify: `shore-daemon/src/autonomy/manager.rs:961` (call site)

- [ ] **Step 1: Add `max_image_size` parameter to `build_content()`**

In `shore-daemon/src/handler/images.rs`, change the `build_content` signature and body to accept and use the limit:

```rust
pub(crate) fn build_content(text: &str, images: &[ImageRef], max_image_size: u64) -> Value {
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
                    if let Some((resized, mt)) = maybe_resize(&bytes, media_type, max_image_size) {
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
```

- [ ] **Step 2: Extract inline image encoding in `build_llm_messages()` to use a shared helper**

In `shore-daemon/src/handler/images.rs`, add a helper that encodes a single `ImageRef` to a JSON image block with resize support:

```rust
/// Encode a single image to a JSON block for the LLM API, resizing if needed.
pub(crate) fn encode_image_block(img: &ImageRef, max_image_size: u64) -> Option<Value> {
    let media_type = media_type_for_path(&img.path)?;
    match std::fs::read(&img.path) {
        Ok(bytes) => {
            let (final_bytes, final_media_type) =
                if let Some((resized, mt)) = maybe_resize(&bytes, media_type, max_image_size) {
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
```

- [ ] **Step 3: Update `build_llm_messages()` signature and body**

In `shore-daemon/src/handler/mod.rs`, update `build_llm_messages` to accept `max_image_size` and use the new helpers.

Update the re-export at line 14:

```rust
pub(crate) use images::{build_content, embed_image_data, encode_image_block, media_type_for_path};
```

Change the function signature at line 743:

```rust
pub(crate) fn build_llm_messages(
    prompt_result: &prompt::AssembledPrompt,
    include_unsigned_thinking: bool,
    max_image_size: u64,
) -> (Vec<Value>, Option<Value>) {
```

Replace the inline image loop (lines 760-778) with:

```rust
                for img in &m.images {
                    if let Some(block) = encode_image_block(img, max_image_size) {
                        blocks.push(block);
                    }
                }
```

Update the `build_content` call at line 790:

```rust
                build_content(&m.content, &m.images, max_image_size)
```

- [ ] **Step 4: Update call site in `mod.rs`**

At line 615, pass the config value:

```rust
    let (llm_messages, system) = build_llm_messages(
        &prompt_result,
        include_unsigned_thinking,
        effective_config.app.advanced.max_image_size,
    );
```

- [ ] **Step 5: Update call site in `autonomy/manager.rs`**

At line 961, pass the config value:

```rust
    let (llm_messages, system) = crate::handler::build_llm_messages(&prompt_result, false, config.app.advanced.max_image_size);
```

- [ ] **Step 6: Update existing `build_content` tests in `mod.rs`**

The three existing tests (`build_content_text_only`, `build_content_with_image`, `build_content_skips_unsupported_and_missing`) call `build_content` without the new parameter. Update each call to pass `0` (disabled) so they test the same behavior as before:

```rust
    // In build_content_text_only:
    let result = build_content("hello", &[], 0);

    // In build_content_with_image:
    let result = build_content("describe this", &images, 0);

    // In build_content_skips_unsupported_and_missing:
    let result = build_content("text", &images, 0);
```

- [ ] **Step 7: Verify full workspace compiles and tests pass**

```bash
cargo test --workspace
```

Expected: all tests PASS.

- [ ] **Step 8: Commit**

```bash
git add shore-daemon/src/handler/images.rs shore-daemon/src/handler/mod.rs shore-daemon/src/autonomy/manager.rs
git commit -m "feat(daemon): integrate image resizing into LLM message pipeline"
```

---

### Task 5: Update example config

**Files:**
- Modify: `examples/config.toml:136-144`

- [ ] **Step 1: Add `max_image_size` to the `[advanced]` section**

In `examples/config.toml`, add to the `[advanced]` section (after `# retry_backoff = "500ms"`):

```toml
# max_image_size = 2000000    # Resize images above this size (bytes) before LLM upload; 0 = disable
```

- [ ] **Step 2: Commit**

```bash
git add examples/config.toml
git commit -m "docs: add max_image_size to example config"
```

---

### Task 6: Final verification

- [ ] **Step 1: Type check**

```bash
cargo check --workspace
```

Expected: success.

- [ ] **Step 2: Full test suite**

```bash
cargo test --workspace
```

Expected: all tests PASS.

- [ ] **Step 3: Verify no clippy warnings in touched crates**

```bash
cargo clippy -p shore-config -p shore-daemon -- -D warnings
```

Expected: no warnings.
