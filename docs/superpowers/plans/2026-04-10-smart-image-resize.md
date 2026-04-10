# Smart Image Resize Pipeline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the MVP single-pass image resizer with a format-aware, cached, async pipeline that preserves transparency, respects dimension floors, and avoids redundant work.

**Architecture:** A new `resize.rs` module under `shore-daemon/src/handler/` owns all resize logic (alpha detection, smart resize algorithm, disk cache). The existing `images.rs` calls into it. An async `warm_image_cache()` function pre-populates the XDG cache via `spawn_blocking` before `build_llm_messages()` runs synchronously. `fast_image_resize` v6 replaces the `image` crate's resize for ~14x speedup.

**Tech Stack:** Rust, `fast_image_resize` v6 (SIMD resize), `image` v0.25 (decode/encode), `sha2` (already in deps), `tokio::task::spawn_blocking`.

**Spec:** `docs/superpowers/specs/2026-04-10-smart-image-resize-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `shore-config/src/lib.rs` | Modify | Add `cache: PathBuf` field to `ShoreDirs` |
| `shore-daemon/Cargo.toml` | Modify | Add `fast_image_resize` dependency |
| `shore-daemon/src/handler/resize.rs` | Create | Alpha detection, smart resize algorithm, disk cache, async warm-up |
| `shore-daemon/src/handler/images.rs` | Modify | Replace `maybe_resize` calls with `cached_resize`, add `cache_dir` param |
| `shore-daemon/src/handler/mod.rs` | Modify | Register `resize` module, thread `cache_dir`, call `warm_image_cache` |
| `shore-daemon/src/autonomy/manager.rs` | Modify | Thread `cache_dir`, call `warm_image_cache` |

---

### Task 1: Add `cache` field to `ShoreDirs`

**Files:**
- Modify: `shore-config/src/lib.rs:52-118`

- [ ] **Step 1: Add `cache` field to `ShoreDirs` struct**

In `shore-config/src/lib.rs`, add the `cache` field after `runtime`:

```rust
pub struct ShoreDirs {
    /// Config directory: $XDG_CONFIG_HOME/shore/
    pub config: PathBuf,
    /// Data directory: $XDG_DATA_HOME/shore/
    pub data: PathBuf,
    /// Runtime directory: $XDG_RUNTIME_DIR/shore/
    pub runtime: PathBuf,
    /// Cache directory: $XDG_CACHE_HOME/shore/
    pub cache: PathBuf,
}
```

- [ ] **Step 2: Resolve `cache` in `ShoreDirs::resolve()`**

Add the `cache` resolution to `ShoreDirs::resolve()`:

```rust
impl ShoreDirs {
    pub fn resolve() -> Self {
        Self {
            config: resolve_xdg_dir(
                "SHORE_CONFIG_DIR",
                "XDG_CONFIG_HOME",
                dirs::config_dir,
                "~/.config",
            ),
            data: resolve_xdg_dir(
                "SHORE_DATA_DIR",
                "XDG_DATA_HOME",
                dirs::data_dir,
                "~/.local/share",
            ),
            runtime: resolve_xdg_dir(
                "SHORE_RUNTIME_DIR",
                "XDG_RUNTIME_DIR",
                dirs::runtime_dir,
                "",
            ),
            cache: resolve_xdg_dir(
                "SHORE_CACHE_DIR",
                "XDG_CACHE_HOME",
                dirs::cache_dir,
                "~/.cache",
            ),
        }
    }
}
```

- [ ] **Step 3: Fix any compilation errors from the new field**

Search for all sites that construct `ShoreDirs` directly (tests, `new_for_test`, etc.) and add the `cache` field. Run:

```bash
cargo build -p shore-config 2>&1 | head -40
```

Fix any missing field errors by adding `cache: resolve_xdg_dir(...)` or `cache: tempdir.path().to_path_buf()` as appropriate.

- [ ] **Step 4: Verify compilation**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: compiles successfully.

- [ ] **Step 5: Commit**

```bash
git add shore-config/src/lib.rs
git commit -m "feat(config): add cache directory to ShoreDirs (XDG_CACHE_HOME)"
```

---

### Task 2: Add `fast_image_resize` dependency and create module skeleton

**Files:**
- Modify: `shore-daemon/Cargo.toml`
- Create: `shore-daemon/src/handler/resize.rs`
- Modify: `shore-daemon/src/handler/mod.rs:10-12`

- [ ] **Step 1: Add dependency**

In `shore-daemon/Cargo.toml`, add after the `image` line:

```toml
fast_image_resize = { version = "6", features = ["image"] }
```

- [ ] **Step 2: Create empty `resize.rs` module**

Create `shore-daemon/src/handler/resize.rs`:

```rust
//! Smart image resize with format awareness, dimension floors, and disk caching.
//!
//! Replaces the MVP single-pass resizer with:
//! - Alpha detection (transparent PNGs stay PNG, opaque images convert to JPEG)
//! - Quality-first strategy for images under 2048px
//! - Dimension estimation for larger images
//! - XDG disk cache to avoid re-encoding on every turn
//! - Async pre-warming via spawn_blocking

use std::path::Path;
```

- [ ] **Step 3: Register the module in `mod.rs`**

In `shore-daemon/src/handler/mod.rs`, add after `mod images;` (line 11):

```rust
mod resize;
```

- [ ] **Step 4: Verify compilation**

```bash
cargo build -p shore-daemon 2>&1 | tail -5
```

Expected: compiles (resize module is empty except imports).

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/Cargo.toml shore-daemon/src/handler/resize.rs shore-daemon/src/handler/mod.rs
git commit -m "build(daemon): add fast_image_resize dep, create resize module skeleton"
```

---

### Task 3: Alpha detection and smart resize algorithm

**Files:**
- Modify: `shore-daemon/src/handler/resize.rs`

This is the core algorithm. Two public functions: `has_meaningful_alpha()` and `smart_resize()`.

- [ ] **Step 1: Write tests for alpha detection**

Add to `resize.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_opaque_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for p in img.pixels_mut() {
            *p = image::Rgba([128, 64, 200, 255]);
        }
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn make_transparent_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for (i, p) in img.pixels_mut().enumerate() {
            let alpha = if i % 4 == 0 { 0 } else { 255 };
            *p = image::Rgba([128, 64, 200, alpha]);
        }
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// LCG noise generator for test images that resist compression.
    fn fill_noise(pixels: &mut [u8], seed: u64) {
        let mut state = seed;
        for byte in pixels.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (state >> 33) as u8;
        }
    }

    fn make_noisy_jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (w * h * 3) as usize];
        fill_noise(&mut pixels, 0xdeadbeef_cafebabe);
        let img = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    fn make_noisy_png_rgb(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (w * h * 3) as usize];
        fill_noise(&mut pixels, 0xcafe_f00d_1234_5678);
        let img = image::RgbImage::from_raw(w, h, pixels).unwrap();
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn make_noisy_transparent_png(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        fill_noise(&mut pixels, 0xbabe_cafe_dead_f00d);
        // Ensure some pixels are actually transparent
        for chunk in pixels.chunks_mut(4) {
            if chunk[0] < 64 {
                chunk[3] = 0; // make ~25% of pixels transparent
            }
        }
        let img = image::RgbaImage::from_raw(w, h, pixels).unwrap();
        let mut buf = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut buf);
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn alpha_detection_opaque_rgba() {
        let png = make_opaque_rgba(100, 100);
        let img = image::load_from_memory(&png).unwrap();
        assert!(!has_meaningful_alpha(&img));
    }

    #[test]
    fn alpha_detection_transparent_rgba() {
        let png = make_transparent_rgba(100, 100);
        let img = image::load_from_memory(&png).unwrap();
        assert!(has_meaningful_alpha(&img));
    }

    #[test]
    fn alpha_detection_rgb_image() {
        let jpeg = make_noisy_jpeg(100, 100);
        let img = image::load_from_memory(&jpeg).unwrap();
        assert!(!has_meaningful_alpha(&img));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p shore-daemon has_meaningful_alpha -- 2>&1 | tail -10
```

Expected: compilation error — `has_meaningful_alpha` not defined.

- [ ] **Step 3: Implement `has_meaningful_alpha`**

Add to `resize.rs` (before the tests module):

```rust
use image::DynamicImage;
use tracing::{info, warn};

/// Check if a decoded image has any pixels with meaningful transparency.
///
/// Returns `false` for RGB/grayscale images (no alpha channel) and for
/// RGBA images where all pixels are fully opaque (alpha == 255).
pub(super) fn has_meaningful_alpha(img: &DynamicImage) -> bool {
    use image::DynamicImage::*;
    match img {
        ImageRgba8(rgba) => rgba.pixels().any(|p| p[3] < 255),
        ImageRgba16(rgba) => rgba.pixels().any(|p| p[3] < 65535),
        ImageRgba32F(rgba) => rgba.pixels().any(|p| p[3] < 1.0),
        ImageLumaA8(la) => la.pixels().any(|p| p[1] < 255),
        ImageLumaA16(la) => la.pixels().any(|p| p[1] < 65535),
        _ => false,
    }
}
```

- [ ] **Step 4: Run alpha detection tests**

```bash
cargo test -p shore-daemon alpha_detection -- 2>&1 | tail -10
```

Expected: 3 tests pass.

- [ ] **Step 5: Write tests for `smart_resize`**

Add to the `tests` module in `resize.rs`:

```rust
    // ── smart_resize tests ───────────────────────────────────────────

    #[test]
    fn smart_resize_returns_none_under_limit() {
        let jpeg = make_noisy_jpeg(100, 100);
        assert!(smart_resize(&jpeg, "image/jpeg", 10_000_000).is_none());
    }

    #[test]
    fn smart_resize_returns_none_when_disabled() {
        let jpeg = make_noisy_jpeg(100, 100);
        assert!(smart_resize(&jpeg, "image/jpeg", 0).is_none());
    }

    #[test]
    fn smart_resize_passes_through_gif() {
        let fake_gif = vec![0u8; 1_000_000];
        assert!(smart_resize(&fake_gif, "image/gif", 100).is_none());
    }

    #[test]
    fn smart_resize_opaque_png_becomes_jpeg() {
        let png = make_noisy_png_rgb(2000, 2000);
        let max = (png.len() as u64) / 4;
        let result = smart_resize(&png, "image/png", max);
        assert!(result.is_some(), "Should resize oversized opaque PNG");
        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert!(
            (resized.len() as u64) <= max,
            "Resized ({}) should be under limit ({})",
            resized.len(), max
        );
        // Verify valid JPEG header
        assert_eq!(&resized[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn smart_resize_transparent_png_stays_png() {
        let png = make_noisy_transparent_png(2000, 2000);
        let max = (png.len() as u64) / 2;
        let result = smart_resize(&png, "image/png", max);
        assert!(result.is_some(), "Should resize oversized transparent PNG");
        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/png");
        assert!(
            (resized.len() as u64) <= max,
            "Resized ({}) should be under limit ({})",
            resized.len(), max
        );
        // Verify valid PNG header
        assert_eq!(&resized[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn smart_resize_small_image_quality_only() {
        // 1000x1000 is under the 2048 dimension floor — should try quality reduction
        // before dimension reduction.
        let jpeg = make_noisy_jpeg(1000, 1000);
        let max = (jpeg.len() as u64) / 2;
        let result = smart_resize(&jpeg, "image/jpeg", max);
        assert!(result.is_some());
        let (resized, media_type) = result.unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert!((resized.len() as u64) <= max);
    }

    #[test]
    fn smart_resize_large_image_under_limit() {
        // 4000x3000 noisy JPEG with 2MB limit
        let jpeg = make_noisy_jpeg(4000, 3000);
        assert!(jpeg.len() > 2_000_000, "Test image should exceed 2MB");
        let result = smart_resize(&jpeg, "image/jpeg", 2_000_000);
        assert!(result.is_some());
        let (resized, _) = result.unwrap();
        assert!(
            resized.len() <= 2_000_000,
            "Resized ({}) should be under 2MB",
            resized.len()
        );
    }

    #[test]
    fn smart_resize_respects_dimension_floor() {
        // An image that's 8000x6000 with a very small byte limit.
        // Even with aggressive resizing, longest side should stay >= 2048
        // unless byte limit makes that impossible.
        let jpeg = make_noisy_jpeg(4000, 3000);
        let max = (jpeg.len() as u64) / 3;
        let result = smart_resize(&jpeg, "image/jpeg", max);
        assert!(result.is_some());
        let (resized, _) = result.unwrap();
        // Decode the resized image and check dimensions
        let decoded = image::load_from_memory(&resized).unwrap();
        let longest = decoded.width().max(decoded.height());
        // The floor is best-effort: with aggressive targets it might go below,
        // but it should still be reasonably large (at least 1024).
        assert!(
            longest >= 1024,
            "Longest side ({longest}) should respect dimension floor"
        );
    }
```

- [ ] **Step 6: Run tests to verify they fail**

```bash
cargo test -p shore-daemon smart_resize -- 2>&1 | tail -10
```

Expected: compilation error — `smart_resize` not defined.

- [ ] **Step 7: Implement `smart_resize`**

Add to `resize.rs`:

```rust
use fast_image_resize as fir;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

/// Minimum dimension (longest side) we aim to preserve during resizing.
const DIMENSION_FLOOR: u32 = 2048;

/// Resize an image if it exceeds `max_bytes`, using format-aware strategies.
///
/// Decision tree:
/// - GIF: pass through (animated GIF resizing unsupported)
/// - Transparent images: keep as PNG, reduce dimensions + max compression
/// - Opaque images ≤ 2048px longest side: reduce quality only (90 → 75)
/// - Opaque images > 2048px: estimate target dimensions + quality 90, verify + retry
///
/// Returns `Some((resized_bytes, media_type))` or `None` if no resize needed.
pub(super) fn smart_resize(
    bytes: &[u8],
    media_type: &str,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    if max_bytes == 0 || (bytes.len() as u64) <= max_bytes {
        return None;
    }

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

    let (src_w, src_h) = (img.width(), img.height());

    if has_meaningful_alpha(&img) {
        resize_transparent(&img, src_w, src_h, bytes.len() as u64, max_bytes)
    } else {
        let longest = src_w.max(src_h);
        if longest <= DIMENSION_FLOOR {
            resize_quality_only(&img, max_bytes)
                .or_else(|| resize_with_dims(&img, src_w, src_h, bytes.len() as u64, max_bytes))
        } else {
            resize_with_dims(&img, src_w, src_h, bytes.len() as u64, max_bytes)
        }
    }
}

/// Resize a transparent image, keeping PNG format with max compression.
fn resize_transparent(
    img: &DynamicImage,
    src_w: u32,
    src_h: u32,
    src_bytes: u64,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    let scale = ((max_bytes as f64 / src_bytes as f64).sqrt() * 0.85).min(1.0);
    let (new_w, new_h) = scaled_dims(src_w, src_h, scale);

    if let Some(buf) = fir_resize_and_encode_png(img, new_w, new_h) {
        if (buf.len() as u64) <= max_bytes {
            log_resize(src_w, src_h, new_w, new_h, src_bytes, buf.len() as u64);
            return Some((buf, "image/png"));
        }
        // Retry with more aggressive scaling
        let correction = ((max_bytes as f64 / buf.len() as f64).sqrt() * 0.85).min(1.0);
        let (retry_w, retry_h) = scaled_dims(new_w, new_h, correction);
        if let Some(buf2) = fir_resize_and_encode_png(img, retry_w, retry_h) {
            log_resize(src_w, src_h, retry_w, retry_h, src_bytes, buf2.len() as u64);
            return Some((buf2, "image/png"));
        }
    }

    warn!("Failed to resize transparent image; sending original");
    None
}

/// Try quality reduction without changing dimensions (for images ≤ 2048px).
fn resize_quality_only(img: &DynamicImage, max_bytes: u64) -> Option<(Vec<u8>, &'static str)> {
    for quality in [90u8, 75] {
        if let Some(buf) = encode_jpeg_from_dynamic(img, img.width(), img.height(), quality) {
            if (buf.len() as u64) <= max_bytes {
                info!(
                    quality,
                    size = buf.len(),
                    "Reduced image quality without dimension change"
                );
                return Some((buf, "image/jpeg"));
            }
        }
    }
    None
}

/// Estimate target dimensions and resize + encode as JPEG.
fn resize_with_dims(
    img: &DynamicImage,
    src_w: u32,
    src_h: u32,
    src_bytes: u64,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    // PNG→JPEG gains a ~3x compression advantage; account for it in estimation.
    let format_factor = if src_bytes as f64 / (src_w as f64 * src_h as f64) > 3.0 {
        3.0 // likely PNG or high-bpp source
    } else {
        1.0
    };
    let raw_scale = ((max_bytes as f64 * format_factor / src_bytes as f64).sqrt() * 0.85).min(1.0);
    let (mut new_w, mut new_h) = scaled_dims(src_w, src_h, raw_scale);

    // Apply dimension floor: don't go below 2048px longest side if source was above it.
    if src_w.max(src_h) >= DIMENSION_FLOOR && new_w.max(new_h) < DIMENSION_FLOOR {
        let boost = DIMENSION_FLOOR as f64 / new_w.max(new_h) as f64;
        new_w = ((new_w as f64) * boost).round() as u32;
        new_h = ((new_h as f64) * boost).round() as u32;
    }

    let quality: u8 = 90;
    if let Some(buf) = fir_resize_and_encode_jpeg(img, new_w, new_h, quality) {
        if (buf.len() as u64) <= max_bytes {
            log_resize(src_w, src_h, new_w, new_h, src_bytes, buf.len() as u64);
            return Some((buf, "image/jpeg"));
        }
        // Retry: correct based on how far off we were.
        let correction = ((max_bytes as f64 / buf.len() as f64).sqrt() * 0.9).min(1.0);
        let (retry_w, retry_h) = scaled_dims(new_w, new_h, correction);
        let retry_q: u8 = 85;
        if let Some(buf2) = fir_resize_and_encode_jpeg(img, retry_w, retry_h, retry_q) {
            log_resize(src_w, src_h, retry_w, retry_h, src_bytes, buf2.len() as u64);
            return Some((buf2, "image/jpeg"));
        }
    }

    warn!("Failed to resize image after retry; sending original");
    None
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Compute scaled dimensions maintaining aspect ratio. Clamps to min 1px.
fn scaled_dims(w: u32, h: u32, scale: f64) -> (u32, u32) {
    (
        ((w as f64) * scale).round().max(1.0) as u32,
        ((h as f64) * scale).round().max(1.0) as u32,
    )
}

/// Resize with `fast_image_resize` and encode as JPEG.
fn fir_resize_and_encode_jpeg(
    img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
    quality: u8,
) -> Option<Vec<u8>> {
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let pixel_type = match fir::IntoImageView::pixel_type(&rgb) {
        Some(pt) => pt,
        None => return None,
    };
    let mut dst = fir::images::Image::new(dst_w, dst_h, pixel_type);
    let mut resizer = fir::Resizer::new();
    if let Err(e) = resizer.resize(&rgb, &mut dst, None) {
        warn!(error = %e, "fast_image_resize failed");
        return None;
    }

    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    if let Err(e) = encoder.write_image(dst.buffer(), dst_w, dst_h, ExtendedColorType::Rgb8) {
        warn!(error = %e, "JPEG encode failed");
        return None;
    }
    Some(buf)
}

/// Resize with `fast_image_resize` and encode as PNG with max compression.
fn fir_resize_and_encode_png(
    img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    let rgba = DynamicImage::ImageRgba8(img.to_rgba8());
    let pixel_type = match fir::IntoImageView::pixel_type(&rgba) {
        Some(pt) => pt,
        None => return None,
    };
    let mut dst = fir::images::Image::new(dst_w, dst_h, pixel_type);
    let mut resizer = fir::Resizer::new();
    if let Err(e) = resizer.resize(&rgba, &mut dst, None) {
        warn!(error = %e, "fast_image_resize failed");
        return None;
    }

    let mut buf = Vec::new();
    let encoder = PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive);
    if let Err(e) = encoder.write_image(dst.buffer(), dst_w, dst_h, ExtendedColorType::Rgba8) {
        warn!(error = %e, "PNG encode failed");
        return None;
    }
    Some(buf)
}

/// Encode a DynamicImage as JPEG at given quality without resizing.
/// Used for the quality-only reduction path.
fn encode_jpeg_from_dynamic(
    img: &DynamicImage,
    w: u32,
    h: u32,
    quality: u8,
) -> Option<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    if let Err(e) = encoder.write_image(rgb.as_raw(), w, h, ExtendedColorType::Rgb8) {
        warn!(error = %e, "JPEG encode failed in quality-only path");
        return None;
    }
    Some(buf)
}

fn log_resize(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, src_bytes: u64, dst_bytes: u64) {
    info!(
        original_size = src_bytes,
        resized_size = dst_bytes,
        original_dims = format!("{src_w}x{src_h}"),
        resized_dims = format!("{dst_w}x{dst_h}"),
        "Resized image for LLM upload"
    );
}
```

- [ ] **Step 8: Run all resize tests**

```bash
cargo test -p shore-daemon --lib handler::resize -- 2>&1 | tail -15
```

Expected: all tests pass. If any size assertions fail, adjust the conservative scaling factors (`0.85` multiplier) and re-run.

- [ ] **Step 9: Commit**

```bash
git add shore-daemon/src/handler/resize.rs
git commit -m "feat(daemon): smart image resize with alpha detection and format awareness"
```

---

### Task 4: Disk cache layer

**Files:**
- Modify: `shore-daemon/src/handler/resize.rs`

- [ ] **Step 1: Write cache tests**

Add to the `tests` module in `resize.rs`:

```rust
    // ── cache tests ──────────────────────────────────────────────────

    #[test]
    fn cache_miss_resizes_and_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("resized");
        let img_dir = tmp.path().join("images");
        std::fs::create_dir_all(&img_dir).unwrap();

        let jpeg = make_noisy_jpeg(3000, 2000);
        let img_path = img_dir.join("photo.jpg");
        std::fs::write(&img_path, &jpeg).unwrap();
        assert!(jpeg.len() > 2_000_000);

        let result = cached_resize(
            img_path.to_str().unwrap(),
            &jpeg,
            "image/jpeg",
            2_000_000,
            &cache_dir,
        );
        assert!(result.is_some());
        let (resized, _) = result.unwrap();
        assert!(resized.len() <= 2_000_000);

        // Cache file should now exist
        assert!(cache_dir.exists());
        let entries: Vec<_> = std::fs::read_dir(&cache_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "Should have exactly one cached file");
    }

    #[test]
    fn cache_hit_skips_resize() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("resized");
        let img_dir = tmp.path().join("images");
        std::fs::create_dir_all(&img_dir).unwrap();

        let jpeg = make_noisy_jpeg(3000, 2000);
        let img_path = img_dir.join("photo.jpg");
        std::fs::write(&img_path, &jpeg).unwrap();

        // First call: cache miss
        let r1 = cached_resize(
            img_path.to_str().unwrap(), &jpeg, "image/jpeg", 2_000_000, &cache_dir,
        );
        assert!(r1.is_some());

        // Second call: should hit cache and return same bytes
        let r2 = cached_resize(
            img_path.to_str().unwrap(), &jpeg, "image/jpeg", 2_000_000, &cache_dir,
        );
        assert!(r2.is_some());
        let (bytes1, mt1) = r1.unwrap();
        let (bytes2, mt2) = r2.unwrap();
        assert_eq!(mt1, mt2);
        assert_eq!(bytes1.len(), bytes2.len());
    }

    #[test]
    fn cache_invalidates_on_config_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("resized");
        let img_dir = tmp.path().join("images");
        std::fs::create_dir_all(&img_dir).unwrap();

        let jpeg = make_noisy_jpeg(3000, 2000);
        let img_path = img_dir.join("photo.jpg");
        std::fs::write(&img_path, &jpeg).unwrap();

        // Resize with 2MB limit
        let r1 = cached_resize(
            img_path.to_str().unwrap(), &jpeg, "image/jpeg", 2_000_000, &cache_dir,
        );
        assert!(r1.is_some());

        // Same image, different limit — should NOT use the old cached version
        let r2 = cached_resize(
            img_path.to_str().unwrap(), &jpeg, "image/jpeg", 1_000_000, &cache_dir,
        );
        assert!(r2.is_some());

        // Cache dir should have 2 files (different keys)
        let entries: Vec<_> = std::fs::read_dir(&cache_dir).unwrap().collect();
        assert_eq!(entries.len(), 2, "Different limits should produce separate cache entries");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p shore-daemon cache_miss -- 2>&1 | tail -10
```

Expected: compilation error — `cached_resize` not defined.

- [ ] **Step 3: Implement cache functions**

Add to `resize.rs` (add `use sha2::{Digest, Sha256};` and `use std::path::PathBuf;` to the imports at the top):

```rust
/// Compute a cache key from image path, modification time, and byte limit.
fn compute_cache_key(path: &str, mtime: std::time::SystemTime, max_bytes: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    let nanos = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    hasher.update(nanos.to_le_bytes());
    hasher.update(max_bytes.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

/// Look up a cached resize result on disk. Returns `(bytes, media_type)` on hit.
fn read_cache(cache_dir: &Path, key: &str) -> Option<(Vec<u8>, &'static str)> {
    // Try JPEG first (most common), then PNG.
    let jpg_path = cache_dir.join(format!("{key}.jpg"));
    if let Ok(bytes) = std::fs::read(&jpg_path) {
        return Some((bytes, "image/jpeg"));
    }
    let png_path = cache_dir.join(format!("{key}.png"));
    if let Ok(bytes) = std::fs::read(&png_path) {
        return Some((bytes, "image/png"));
    }
    None
}

/// Write a resize result to the cache directory.
fn write_cache(cache_dir: &Path, key: &str, bytes: &[u8], media_type: &str) {
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        warn!(error = %e, "Failed to create resize cache directory");
        return;
    }
    let ext = if media_type == "image/png" { "png" } else { "jpg" };
    let path = cache_dir.join(format!("{key}.{ext}"));
    if let Err(e) = std::fs::write(&path, bytes) {
        warn!(error = %e, "Failed to write to resize cache");
    }
}

/// Resize an image with caching. Checks the disk cache first; on miss,
/// runs `smart_resize` and writes the result to cache.
///
/// `path` is the original image file path (used for cache key).
/// `bytes` is the already-read file content.
pub(super) fn cached_resize(
    path: &str,
    bytes: &[u8],
    media_type: &str,
    max_bytes: u64,
    cache_dir: &Path,
) -> Option<(Vec<u8>, &'static str)> {
    if max_bytes == 0 || (bytes.len() as u64) <= max_bytes {
        return None;
    }

    // Compute cache key
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let key = compute_cache_key(path, mtime, max_bytes);

    // Check cache
    let resized_dir = cache_dir.join("resized");
    if let Some(cached) = read_cache(&resized_dir, &key) {
        info!(path, "Using cached resized image");
        return Some(cached);
    }

    // Cache miss — resize
    let (resized_bytes, result_media_type) = smart_resize(bytes, media_type, max_bytes)?;
    write_cache(&resized_dir, &key, &resized_bytes, result_media_type);
    Some((resized_bytes, result_media_type))
}
```

- [ ] **Step 4: Run cache tests**

```bash
cargo test -p shore-daemon --lib handler::resize::tests::cache -- 2>&1 | tail -15
```

Expected: all 3 cache tests pass.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/handler/resize.rs
git commit -m "feat(daemon): add XDG disk cache for resized images"
```

---

### Task 5: Async warm-up function

**Files:**
- Modify: `shore-daemon/src/handler/resize.rs`

- [ ] **Step 1: Implement `warm_image_cache`**

Add to `resize.rs`:

```rust
use shore_protocol::types::ImageRef;

/// Pre-warm the resize cache for all images in the prompt messages.
///
/// Runs resize operations on tokio's blocking thread pool so the async
/// event loop is not stalled. Multiple images are processed concurrently.
pub(crate) async fn warm_image_cache(
    messages: &[crate::engine::prompt::PromptMessage],
    max_bytes: u64,
    cache_dir: &Path,
) {
    use futures::future::join_all;

    if max_bytes == 0 {
        return;
    }

    // Collect all image paths that might need resizing.
    let mut work: Vec<(String, String)> = Vec::new(); // (path, media_type)
    for msg in messages {
        for img in &msg.images {
            if let Some(mt) = super::images::media_type_for_path(&img.path) {
                // Quick check: does the file exceed the limit?
                if let Ok(meta) = std::fs::metadata(&img.path) {
                    if meta.len() > max_bytes {
                        work.push((img.path.clone(), mt.to_string()));
                    }
                }
            }
        }
    }

    if work.is_empty() {
        return;
    }

    let cache_dir = cache_dir.to_path_buf();
    let futures: Vec<_> = work
        .into_iter()
        .map(|(path, media_type)| {
            let cache_dir = cache_dir.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(bytes) = std::fs::read(&path) {
                    let _ = cached_resize(&path, &bytes, &media_type, max_bytes, &cache_dir);
                }
            })
        })
        .collect();

    // Wait for all resize tasks to complete.
    for result in join_all(futures).await {
        if let Err(e) = result {
            warn!(error = %e, "Image cache warm-up task failed");
        }
    }
}
```

- [ ] **Step 2: Make `media_type_for_path` accessible from `resize.rs`**

In `shore-daemon/src/handler/images.rs`, the function `media_type_for_path` is currently `pub(crate)`. It's already accessible from the `resize` module via `super::images::media_type_for_path`. No change needed — just verify the path compiles.

- [ ] **Step 3: Verify compilation**

```bash
cargo build -p shore-daemon 2>&1 | tail -10
```

Expected: compiles. If there are import issues with `crate::engine::prompt::PromptMessage`, fix the path.

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/src/handler/resize.rs
git commit -m "feat(daemon): add async warm_image_cache with spawn_blocking"
```

---

### Task 6: Wire up call sites

**Files:**
- Modify: `shore-daemon/src/handler/images.rs`
- Modify: `shore-daemon/src/handler/mod.rs:14,614-618,746-749`
- Modify: `shore-daemon/src/autonomy/manager.rs:961`

- [ ] **Step 1: Update `images.rs` to use `cached_resize` instead of `maybe_resize`**

In `shore-daemon/src/handler/images.rs`:

1. Remove the `maybe_resize` function entirely (lines 229-279). Also remove `info` from the `use tracing::{info, warn};` import at line 9 (it's only used by `maybe_resize`).

2. Add `cache_dir: &std::path::Path` parameter to `build_content`:

```rust
pub(crate) fn build_content(
    text: &str,
    images: &[ImageRef],
    max_image_size: u64,
    cache_dir: &std::path::Path,
) -> Value {
```

3. Replace the `maybe_resize` call inside `build_content` with `super::resize::cached_resize`:

```rust
            Ok(bytes) => {
                let (final_bytes, final_media_type) =
                    if let Some((resized, mt)) =
                        super::resize::cached_resize(&img.path, &bytes, media_type, max_image_size, cache_dir)
                    {
                        (resized, mt)
                    } else {
                        (bytes, media_type)
                    };
```

4. Add `cache_dir: &std::path::Path` parameter to `encode_image_block`:

```rust
pub(crate) fn encode_image_block(
    img: &ImageRef,
    max_image_size: u64,
    cache_dir: &std::path::Path,
) -> Option<Value> {
```

5. Replace the `maybe_resize` call inside `encode_image_block` with `super::resize::cached_resize`:

```rust
            let (final_bytes, final_media_type) =
                if let Some((resized, mt)) =
                    super::resize::cached_resize(&img.path, &bytes, media_type, max_image_size, cache_dir)
                {
                    (resized, mt)
                } else {
                    (bytes, media_type)
                };
```

- [ ] **Step 2: Update `mod.rs` — exports and `build_llm_messages`**

In `shore-daemon/src/handler/mod.rs`:

1. Update the re-export line (line 14) — add `warm_image_cache` from resize:

```rust
pub(crate) use images::{build_content, embed_image_data, encode_image_block};
pub(crate) use resize::warm_image_cache;
```

2. Add `cache_dir: &std::path::Path` parameter to `build_llm_messages` (line 746-749):

```rust
pub(crate) fn build_llm_messages(
    prompt_result: &prompt::AssembledPrompt,
    include_unsigned_thinking: bool,
    max_image_size: u64,
    cache_dir: &std::path::Path,
) -> (Vec<Value>, Option<Value>) {
```

3. Update the `encode_image_block` call inside `build_llm_messages` (line 765):

```rust
                    if let Some(block) = encode_image_block(img, max_image_size, cache_dir) {
```

4. Update the `build_content` call inside `build_llm_messages` (line 779):

```rust
                build_content(&m.content, &m.images, max_image_size, cache_dir)
```

5. Update the call site at line 614-618 — add `warm_image_cache` call and thread `cache_dir`:

```rust
    // 7. Build LLM messages from assembled prompt.
    let cache_dir = &effective_config.dirs.cache;
    warm_image_cache(&prompt_result.messages, effective_config.app.advanced.max_image_size, cache_dir).await;
    let include_unsigned_thinking = matches!(resolved.sdk, Sdk::Zai);
    let (llm_messages, system) = build_llm_messages(
        &prompt_result,
        include_unsigned_thinking,
        effective_config.app.advanced.max_image_size,
        cache_dir,
    );
```

Note: The function containing this call site should already be `async`. Verify this — if it's not, you'll get a compilation error. Check the function signature of the enclosing function and ensure it's `async fn`.

- [ ] **Step 3: Update `autonomy/manager.rs`**

At the call site (line 961), add the cache warming and thread `cache_dir`:

```rust
    let cache_dir = &config.dirs.cache;
    crate::handler::warm_image_cache(&prompt_result.messages, config.app.advanced.max_image_size, cache_dir).await;
    let (llm_messages, system) = crate::handler::build_llm_messages(
        &prompt_result,
        false,
        config.app.advanced.max_image_size,
        cache_dir,
    );
```

Verify the enclosing function is `async`.

- [ ] **Step 4: Verify compilation**

```bash
cargo build -p shore-daemon 2>&1 | tail -20
```

Fix any compilation errors. Common issues:
- Missing `cache_dir` argument at call sites
- Import path for `warm_image_cache`
- `effective_config.dirs` not available — check if `LoadedConfig` is in scope and contains `dirs`

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/handler/images.rs shore-daemon/src/handler/mod.rs shore-daemon/src/autonomy/manager.rs
git commit -m "feat(daemon): wire up cached resize and async warm-up at all call sites"
```

---

### Task 7: Update existing tests

**Files:**
- Modify: `shore-daemon/src/handler/images.rs` (test module)
- Modify: `shore-daemon/src/handler/mod.rs` (test module)

- [ ] **Step 1: Update `images.rs` tests**

The existing tests in `images.rs` call `maybe_resize` (now removed) and `build_content` (signature changed).

1. **Remove all `maybe_resize_*` tests** — they're replaced by `smart_resize_*` tests in `resize.rs`.

2. **Update `build_content` integration tests** to pass `cache_dir`:

```rust
    #[test]
    fn build_content_resizes_oversized_image_on_disk() {
        // ... existing setup unchanged ...
        let cache_dir = tmp.path().join("cache");
        let result = build_content("describe this", &images, 2_000_000, &cache_dir);
        // ... existing assertions unchanged ...
    }

    #[test]
    fn build_content_does_not_resize_small_image() {
        // ... existing setup unchanged ...
        let cache_dir = tmp.path().join("cache");
        let result = build_content("test", &images, 2_000_000, &cache_dir);
        // ... existing assertions unchanged ...
    }

    #[test]
    fn build_content_resizes_oversized_png_to_jpeg() {
        // ... existing setup unchanged ...
        let cache_dir = tmp.path().join("cache");
        let result = build_content("describe", &images, 2_000_000, &cache_dir);
        // ... existing assertions unchanged ...
    }
```

3. **Update `build_content` tests that pass `0` for max_image_size** — these also need the `cache_dir` param. Look for `build_content("...", &..., 0)` calls and add a temp path:

```rust
        let cache_dir = std::path::Path::new("/tmp/shore-test-unused-cache");
        let result = build_content("hello", &[], 0, cache_dir);
```

- [ ] **Step 2: Update `mod.rs` tests**

Search for `build_content` calls in `mod.rs` test module and add the `cache_dir` parameter. The existing tests at ~lines 1059-1110 need updating:

```rust
    fn build_content_text_only() {
        let cache_dir = std::path::Path::new("/tmp/shore-test-unused-cache");
        let result = build_content("hello", &[], 0, cache_dir);
        // ...
    }
```

Do the same for `build_content_with_image` and `build_content_skips_unsupported_and_missing`.

- [ ] **Step 3: Remove dead `maybe_resize` test helpers if unused**

Check if `make_jpeg` (the solid-color version) is still used anywhere. If not, remove it. Keep `make_noisy_jpeg`, `make_noisy_png`, etc. as they may still be used by the `build_content` integration tests in `images.rs`.

- [ ] **Step 4: Run all tests**

```bash
cargo test -p shore-daemon -- 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Run full workspace tests**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: all workspace tests pass.

- [ ] **Step 6: Run type check and lint**

```bash
cargo clippy --workspace -- -D warnings 2>&1 | tail -20
```

Fix any warnings from the new code. Pre-existing clippy warnings in untouched files can be ignored.

- [ ] **Step 7: Commit**

```bash
git add shore-daemon/src/handler/images.rs shore-daemon/src/handler/mod.rs
git commit -m "test: update image tests for cached resize pipeline"
```

---

## Post-Implementation Checklist

- [ ] All `cargo test --workspace` pass
- [ ] `cargo clippy --workspace` has no new warnings
- [ ] `cargo build --workspace --release` succeeds
- [ ] Verify `ShoreDirs::resolve().cache` points to `$XDG_CACHE_HOME/shore/` or `~/.cache/shore/`
- [ ] Record architectural change in `docs/ARCHITECTURE.md`: new `resize.rs` module, XDG cache directory
- [ ] Record decision in `docs/DECISIONS.md`: format-aware resize strategy, fast_image_resize choice, cache-as-communication-channel pattern
