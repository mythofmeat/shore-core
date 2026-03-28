use std::io::Write;

/// Supported image protocols for inline terminal display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

/// Detect supported image protocol from environment.
pub fn detect_protocol(
    shore_images: Option<&str>,
    term_program: Option<&str>,
    term: Option<&str>,
) -> Option<ImageProtocol> {
    // Explicit override
    if let Some(val) = shore_images {
        return match val.to_lowercase().as_str() {
            "kitty" => Some(ImageProtocol::Kitty),
            "iterm2" | "iterm" => Some(ImageProtocol::Iterm2),
            _ => None,
        };
    }

    // Auto-detect from TERM_PROGRAM
    if let Some(prog) = term_program {
        let lower = prog.to_lowercase();
        if lower.contains("iterm") {
            return Some(ImageProtocol::Iterm2);
        }
        if lower.contains("kitty") {
            return Some(ImageProtocol::Kitty);
        }
    }

    // Fallback to TERM
    if let Some(t) = term {
        if t.contains("kitty") {
            return Some(ImageProtocol::Kitty);
        }
    }

    None
}

/// Detect protocol from actual environment variables.
pub fn detect_protocol_from_env() -> Option<ImageProtocol> {
    detect_protocol(
        std::env::var("SHORE_IMAGES").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// Render an image inline in the terminal using the detected protocol.
/// Returns true if the image was rendered, false if unsupported.
pub fn render_image(path: &str, _caption: Option<&str>) -> bool {
    let protocol = match detect_protocol_from_env() {
        Some(p) => p,
        None => return false,
    };

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let encoded = base64_encode(&data);

    let mut stdout = std::io::stdout();
    match protocol {
        ImageProtocol::Kitty => {
            render_kitty(&mut stdout, &encoded);
        }
        ImageProtocol::Iterm2 => {
            let name = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image");
            render_iterm2(&mut stdout, &encoded, name, data.len());
        }
    }
    let _ = stdout.flush();
    true
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn render_kitty<W: Write>(w: &mut W, encoded: &str) {
    const CHUNK_SIZE: usize = 4096;
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(CHUNK_SIZE)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let more = if i + 1 < chunks.len() { 1 } else { 0 };
        if i == 0 {
            let _ = write!(w, "\x1b_Ga=T,f=100,m={more};{chunk}\x1b\\");
        } else {
            let _ = write!(w, "\x1b_Gm={more};{chunk}\x1b\\");
        }
    }
    let _ = writeln!(w);
}

fn render_iterm2<W: Write>(w: &mut W, encoded: &str, name: &str, size: usize) {
    use base64::Engine;
    let name_b64 = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    let _ = write!(
        w,
        "\x1b]1337;File=name={name_b64};size={size};inline=1:{encoded}\x07"
    );
    let _ = writeln!(w);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_kitty_from_env() {
        assert_eq!(
            detect_protocol(Some("kitty"), None, None),
            Some(ImageProtocol::Kitty)
        );
    }

    #[test]
    fn detect_iterm_from_term_program() {
        assert_eq!(
            detect_protocol(None, Some("iTerm.app"), None),
            Some(ImageProtocol::Iterm2)
        );
    }

    #[test]
    fn detect_kitty_from_term() {
        assert_eq!(
            detect_protocol(None, None, Some("xterm-kitty")),
            Some(ImageProtocol::Kitty)
        );
    }

    #[test]
    fn detect_none() {
        assert_eq!(detect_protocol(None, None, Some("xterm-256color")), None);
    }

    #[test]
    fn detect_off() {
        assert_eq!(detect_protocol(Some("off"), None, None), None);
    }

    #[test]
    fn kitty_render_format() {
        let mut buf = Vec::new();
        render_kitty(&mut buf, "AAAA");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b_Ga=T,f=100,m=0;AAAA\x1b\\"));
    }

    #[test]
    fn iterm2_render_format() {
        let mut buf = Vec::new();
        render_iterm2(&mut buf, "BBBB", "test.png", 100);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b]1337;File="));
        assert!(out.contains("inline=1:BBBB"));
    }
}
