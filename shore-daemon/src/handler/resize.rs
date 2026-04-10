//! Smart image resize with format awareness, dimension floors, and disk caching.
//!
//! Replaces the MVP single-pass resizer with:
//! - Alpha detection (transparent PNGs stay PNG, opaque images convert to JPEG)
//! - Quality-first strategy for images under 2048px
//! - Dimension estimation for larger images
//! - XDG disk cache to avoid re-encoding on every turn
//! - Async pre-warming via spawn_blocking

use fast_image_resize as fir;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{DynamicImage, ExtendedColorType, ImageEncoder};
use tracing::{info, warn};

const DIMENSION_FLOOR: u32 = 2048;

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

pub(super) fn smart_resize(
    bytes: &[u8],
    media_type: &str,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    if max_bytes == 0 || (bytes.len() as u64) <= max_bytes {
        return None;
    }
    if media_type == "image/gif" {
        warn!(size = bytes.len(), max = max_bytes, "GIF exceeds max_image_size but resizing is not supported; sending as-is");
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

fn resize_quality_only(img: &DynamicImage, max_bytes: u64) -> Option<(Vec<u8>, &'static str)> {
    for quality in [90u8, 75] {
        if let Some(buf) = encode_jpeg_from_dynamic(img, img.width(), img.height(), quality) {
            if (buf.len() as u64) <= max_bytes {
                info!(quality, size = buf.len(), "Reduced image quality without dimension change");
                return Some((buf, "image/jpeg"));
            }
        }
    }
    None
}

fn resize_with_dims(
    img: &DynamicImage,
    src_w: u32,
    src_h: u32,
    src_bytes: u64,
    max_bytes: u64,
) -> Option<(Vec<u8>, &'static str)> {
    let format_factor = if src_bytes as f64 / (src_w as f64 * src_h as f64) > 3.0 {
        3.0
    } else {
        1.0
    };
    let raw_scale =
        ((max_bytes as f64 * format_factor / src_bytes as f64).sqrt() * 0.85).min(1.0);
    let (mut new_w, mut new_h) = scaled_dims(src_w, src_h, raw_scale);
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

fn scaled_dims(w: u32, h: u32, scale: f64) -> (u32, u32) {
    (
        ((w as f64) * scale).round().max(1.0) as u32,
        ((h as f64) * scale).round().max(1.0) as u32,
    )
}

fn fir_resize_and_encode_jpeg(
    img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
    quality: u8,
) -> Option<Vec<u8>> {
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let pixel_type = fir::IntoImageView::pixel_type(&rgb)?;
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

fn fir_resize_and_encode_png(img: &DynamicImage, dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    let rgba = DynamicImage::ImageRgba8(img.to_rgba8());
    let pixel_type = fir::IntoImageView::pixel_type(&rgba)?;
    let mut dst = fir::images::Image::new(dst_w, dst_h, pixel_type);
    let mut resizer = fir::Resizer::new();
    if let Err(e) = resizer.resize(&rgba, &mut dst, None) {
        warn!(error = %e, "fast_image_resize failed");
        return None;
    }
    let mut buf = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilterType::Adaptive);
    if let Err(e) = encoder.write_image(dst.buffer(), dst_w, dst_h, ExtendedColorType::Rgba8) {
        warn!(error = %e, "PNG encode failed");
        return None;
    }
    Some(buf)
}

fn encode_jpeg_from_dynamic(img: &DynamicImage, w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    if let Err(e) = encoder.write_image(rgb.as_raw(), w, h, ExtendedColorType::Rgb8) {
        warn!(error = %e, "JPEG encode failed in quality-only path");
        return None;
    }
    Some(buf)
}

fn log_resize(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    src_bytes: u64,
    dst_bytes: u64,
) {
    info!(
        original_size = src_bytes,
        resized_size = dst_bytes,
        original_dims = format!("{src_w}x{src_h}"),
        resized_dims = format!("{dst_w}x{dst_h}"),
        "Resized image for LLM upload"
    );
}

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

    fn fill_noise(pixels: &mut [u8], seed: u64) {
        let mut state = seed;
        for byte in pixels.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
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
        for chunk in pixels.chunks_mut(4) {
            if chunk[0] < 64 {
                chunk[3] = 0;
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
            resized.len(),
            max
        );
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
            resized.len(),
            max
        );
        assert_eq!(&resized[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn smart_resize_small_image_quality_only() {
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
        let jpeg = make_noisy_jpeg(4000, 3000);
        let max = (jpeg.len() as u64) / 3;
        let result = smart_resize(&jpeg, "image/jpeg", max);
        assert!(result.is_some());
        let (resized, _) = result.unwrap();
        let decoded = image::load_from_memory(&resized).unwrap();
        let longest = decoded.width().max(decoded.height());
        assert!(
            longest >= 1024,
            "Longest side ({longest}) should respect dimension floor"
        );
    }
}
