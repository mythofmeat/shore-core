//! System clipboard image paste support.
//!
//! Reads RGBA image data from the system clipboard via arboard, encodes
//! it as PNG, and writes it to a temp file. The resulting path is fed
//! into shore-tui's existing pending-image flow.

use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ClipboardError {
    NoImage,
    ClipboardUnavailable(String),
    EncodeFailed(String),
    WriteFailed(io::Error),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::NoImage => write!(f, "clipboard has no image"),
            ClipboardError::ClipboardUnavailable(e) => write!(f, "clipboard unavailable: {e}"),
            ClipboardError::EncodeFailed(e) => write!(f, "failed to encode pasted image: {e}"),
            ClipboardError::WriteFailed(e) => write!(f, "failed to write paste temp: {e}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Encode an RGBA8 buffer as PNG bytes.
fn encode_rgba_to_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, ClipboardError> {
    let buffer: image::RgbaImage = image::ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| ClipboardError::EncodeFailed("buffer dimensions invalid".into()))?;
    let mut out = Vec::new();
    buffer
        .write_to(&mut io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| ClipboardError::EncodeFailed(e.to_string()))?;
    Ok(out)
}

/// Generate a unique temp-file path under the OS temp dir.
fn fresh_temp_path() -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("shore_paste_{ts}.png"));
    if path.exists() {
        // Vanishingly rare at ms resolution; one retry with a counter.
        for n in 1..1000 {
            let mut alt = std::env::temp_dir();
            alt.push(format!("shore_paste_{ts}_{n}.png"));
            if !alt.exists() {
                return alt;
            }
        }
    }
    path
}

/// Read an image from the system clipboard, encode as PNG, and write to
/// a temp file. Returns the path on success.
///
/// Synchronous and blocking — designed to be invoked via
/// `tokio::task::spawn_blocking`. arboard's Wayland backend can briefly
/// stall during the clipboard handoff; the caller is expected to wrap
/// this in a timeout.
pub fn read_image_to_temp() -> Result<PathBuf, ClipboardError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| ClipboardError::ClipboardUnavailable(e.to_string()))?;

    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(arboard::Error::ContentNotAvailable) => return Err(ClipboardError::NoImage),
        Err(e) => return Err(ClipboardError::ClipboardUnavailable(e.to_string())),
    };

    let png = encode_rgba_to_png(img.width as u32, img.height as u32, img.bytes.into_owned())?;

    let path = fresh_temp_path();
    std::fs::write(&path, &png).map_err(ClipboardError::WriteFailed)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_rgba_to_png_roundtrip() {
        // 2x1 image: red pixel, then green pixel.
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let png = encode_rgba_to_png(2, 1, rgba.clone()).expect("encode");
        let decoded = image::load_from_memory(&png).expect("decode");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 1);
        let round = decoded.into_rgba8().into_raw();
        assert_eq!(round, rgba);
    }

    #[test]
    fn temp_path_format() {
        let p = fresh_temp_path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("shore_paste_"), "name was {name}");
        assert!(name.ends_with(".png"), "name was {name}");
        assert_eq!(p.parent().unwrap(), std::env::temp_dir().as_path());
    }
}
