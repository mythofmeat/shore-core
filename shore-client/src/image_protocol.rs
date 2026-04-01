//! Terminal image protocol detection.
//!
//! Shared between shore-cli and shore-tui for consistent protocol detection.

use std::fmt;

/// Supported inline image protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

impl fmt::Display for ImageProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageProtocol::Kitty => write!(f, "kitty"),
            ImageProtocol::Iterm2 => write!(f, "iterm2"),
        }
    }
}

/// Detect the best image protocol for the current terminal.
///
/// Check order:
/// 1. `SHORE_IMAGES` env var: `kitty`, `iterm2`/`iterm`, or `off`
/// 2. `TERM_PROGRAM` env var: `iTerm.app` → iTerm2, contains `kitty` → Kitty
/// 3. `TERM` env var: contains `kitty` → Kitty
/// 4. None (unsupported)
pub fn detect_protocol() -> Option<ImageProtocol> {
    detect_protocol_from_env(
        std::env::var("SHORE_IMAGES").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// Testable version that takes env values directly.
pub fn detect_protocol_from_env(
    shore_images: Option<&str>,
    term_program: Option<&str>,
    term: Option<&str>,
) -> Option<ImageProtocol> {
    // Manual override takes priority.
    if let Some(val) = shore_images {
        return match val.to_lowercase().as_str() {
            "kitty" => Some(ImageProtocol::Kitty),
            "iterm2" | "iterm" => Some(ImageProtocol::Iterm2),
            "off" => None,
            _ => None,
        };
    }

    // Auto-detect from TERM_PROGRAM.
    if let Some(prog) = term_program {
        let lower = prog.to_lowercase();
        if lower.contains("iterm") {
            return Some(ImageProtocol::Iterm2);
        }
        if lower.contains("kitty") {
            return Some(ImageProtocol::Kitty);
        }
    }

    // Auto-detect from TERM.
    if let Some(t) = term {
        if t.to_lowercase().contains("kitty") {
            return Some(ImageProtocol::Kitty);
        }
    }

    None
}
