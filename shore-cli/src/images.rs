use std::fs;
use std::io::{self, Write};

use base64::Engine;
pub use shore_client::image_protocol::{ImageProtocol, detect_protocol};

/// Render an image inline using the detected protocol, or fall back to text.
///
/// `path` is the filesystem path to the image.
/// `caption` is an optional human-readable name.
pub fn render_image(path: &str, caption: Option<&str>) {
    let label = caption.unwrap_or(path);

    match detect_protocol() {
        Some(ImageProtocol::Kitty) => {
            if let Err(e) = render_kitty(path) {
                eprintln!("[img: {label}] (kitty render failed: {e})");
            }
        }
        Some(ImageProtocol::Iterm2) => {
            if let Err(e) = render_iterm2(path, label) {
                eprintln!("[img: {label}] (iterm2 render failed: {e})");
            }
        }
        None => {
            println!("[img: {label}]");
        }
    }
}

/// Render an image using the Kitty graphics protocol.
///
/// Uses the "transmit and display" action with base64-encoded data,
/// chunked into 4096-byte pieces per the protocol spec.
fn render_kitty(path: &str) -> io::Result<()> {
    let data = fs::read(path)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let chunk_size = 4096;
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(chunk_size)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == chunks.len() - 1;
        if i == 0 {
            // First chunk: action=transmit+display, format=100 (PNG/auto)
            write!(
                out,
                "\x1b_Ga=T,f=100,m={};{}\x1b\\",
                if is_last { 0 } else { 1 },
                chunk
            )?;
        } else {
            // Continuation chunk
            write!(
                out,
                "\x1b_Gm={};{}\x1b\\",
                if is_last { 0 } else { 1 },
                chunk
            )?;
        }
    }
    writeln!(out)?;
    out.flush()
}

/// Render an image using the iTerm2 inline images protocol.
fn render_iterm2(path: &str, name: &str) -> io::Result<()> {
    let data = fs::read(path)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
    let name_b64 = base64::engine::general_purpose::STANDARD.encode(name);
    let stdout = io::stdout();
    let mut out = stdout.lock();

    write!(
        out,
        "\x1b]1337;File=name={};size={};inline=1:{}\x07",
        name_b64,
        data.len(),
        encoded
    )?;
    writeln!(out)?;
    out.flush()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shore_client::image_protocol::detect_protocol_from_env;

    // ── Protocol detection from env ──────────────────────────────────

    #[test]
    fn shore_images_kitty_override() {
        let proto = detect_protocol_from_env(Some("kitty"), None, None);
        assert_eq!(proto, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn shore_images_iterm2_override() {
        let proto = detect_protocol_from_env(Some("iterm2"), None, None);
        assert_eq!(proto, Some(ImageProtocol::Iterm2));
    }

    #[test]
    fn shore_images_off_override() {
        let proto = detect_protocol_from_env(Some("off"), None, None);
        assert_eq!(proto, None);
    }

    #[test]
    fn shore_images_case_insensitive() {
        assert_eq!(
            detect_protocol_from_env(Some("KITTY"), None, None),
            Some(ImageProtocol::Kitty)
        );
        assert_eq!(
            detect_protocol_from_env(Some("iTerm2"), None, None),
            Some(ImageProtocol::Iterm2)
        );
        assert_eq!(
            detect_protocol_from_env(Some("OFF"), None, None),
            None
        );
    }

    #[test]
    fn shore_images_unknown_value_is_off() {
        let proto = detect_protocol_from_env(Some("sixel"), None, None);
        assert_eq!(proto, None);
    }

    #[test]
    fn shore_images_overrides_term_program() {
        let proto = detect_protocol_from_env(Some("kitty"), Some("iTerm.app"), None);
        assert_eq!(proto, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn shore_images_off_overrides_term_program() {
        let proto = detect_protocol_from_env(Some("off"), Some("iTerm.app"), None);
        assert_eq!(proto, None);
    }

    #[test]
    fn detect_iterm_from_term_program() {
        let proto = detect_protocol_from_env(None, Some("iTerm.app"), None);
        assert_eq!(proto, Some(ImageProtocol::Iterm2));
    }

    #[test]
    fn detect_iterm_from_term_program_case_insensitive() {
        let proto = detect_protocol_from_env(None, Some("ITERM.APP"), None);
        assert_eq!(proto, Some(ImageProtocol::Iterm2));
    }

    #[test]
    fn detect_kitty_from_term_program() {
        let proto = detect_protocol_from_env(None, Some("kitty"), None);
        assert_eq!(proto, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn detect_kitty_from_term() {
        let proto = detect_protocol_from_env(None, None, Some("xterm-kitty"));
        assert_eq!(proto, Some(ImageProtocol::Kitty));
    }

    #[test]
    fn no_detection_on_generic_terminal() {
        let proto = detect_protocol_from_env(None, None, Some("xterm-256color"));
        assert_eq!(proto, None);
    }

    #[test]
    fn no_detection_when_all_none() {
        let proto = detect_protocol_from_env(None, None, None);
        assert_eq!(proto, None);
    }

    #[test]
    fn term_program_takes_priority_over_term() {
        let proto = detect_protocol_from_env(None, Some("iTerm.app"), Some("xterm-kitty"));
        assert_eq!(proto, Some(ImageProtocol::Iterm2));
    }

    #[test]
    fn display_protocol_names() {
        assert_eq!(ImageProtocol::Kitty.to_string(), "kitty");
        assert_eq!(ImageProtocol::Iterm2.to_string(), "iterm2");
    }
}
