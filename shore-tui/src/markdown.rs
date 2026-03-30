use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Convert markdown text into styled ratatui Lines.
///
/// Supports: **bold**, *italic*, `inline code`, ```code blocks```,
/// # headings, and > blockquotes. This is a lightweight parser
/// designed for chat display, not a full CommonMark implementation.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.lines() {
        if raw_line.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                // Opening fence — show language hint if present
                let lang = raw_line.trim_start_matches('`').trim();
                if !lang.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("── {lang} ──"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                raw_line.to_string(),
                Style::default().fg(Color::Green),
            )));
            continue;
        }

        // Headings
        if let Some(heading) = raw_line.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(heading) = raw_line.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(heading) = raw_line.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }

        // Blockquotes
        if let Some(quoted) = raw_line.strip_prefix("> ") {
            let mut spans = vec![Span::styled(
                "▎ ".to_string(),
                Style::default().fg(Color::DarkGray),
            )];
            spans.extend(parse_inline(quoted, Style::default().fg(Color::DarkGray)));
            lines.push(Line::from(spans));
            continue;
        }

        // Regular line with inline formatting
        let spans = parse_inline(raw_line, Style::default());
        lines.push(Line::from(spans));
    }

    // Handle unclosed code block

    // Indent the first content line
    if let Some(line) = lines.first_mut() {
        let mut spans = vec![Span::raw("  ")];
        spans.append(&mut line.spans);
        line.spans = spans;
    }

    lines
}

/// Parse inline markdown formatting: **bold**, *italic*, `code`.
fn parse_inline(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Inline code
        if let Some(pos) = remaining.find('`') {
            if pos > 0 {
                spans.push(Span::styled(remaining[..pos].to_string(), base_style));
            }
            remaining = &remaining[pos + 1..];
            if let Some(end) = remaining.find('`') {
                spans.push(Span::styled(
                    remaining[..end].to_string(),
                    base_style
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::default()),
                ));
                remaining = &remaining[end + 1..];
                continue;
            }
            // Unmatched backtick
            spans.push(Span::styled("`".to_string(), base_style));
            continue;
        }

        // Bold **text**
        if let Some(pos) = remaining.find("**") {
            if pos > 0 {
                spans.push(Span::styled(remaining[..pos].to_string(), base_style));
            }
            remaining = &remaining[pos + 2..];
            if let Some(end) = remaining.find("**") {
                spans.push(Span::styled(
                    remaining[..end].to_string(),
                    base_style.add_modifier(Modifier::BOLD),
                ));
                remaining = &remaining[end + 2..];
                continue;
            }
            // Unmatched **
            spans.push(Span::styled("**".to_string(), base_style));
            continue;
        }

        // Italic *text*
        if let Some(pos) = remaining.find('*') {
            if pos > 0 {
                spans.push(Span::styled(remaining[..pos].to_string(), base_style));
            }
            remaining = &remaining[pos + 1..];
            if let Some(end) = remaining.find('*') {
                spans.push(Span::styled(
                    remaining[..end].to_string(),
                    base_style.add_modifier(Modifier::ITALIC),
                ));
                remaining = &remaining[end + 1..];
                continue;
            }
            // Unmatched *
            spans.push(Span::styled("*".to_string(), base_style));
            continue;
        }

        // Plain text (no more markers)
        spans.push(Span::styled(remaining.to_string(), base_style));
        break;
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        let lines = render_markdown("hello world");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn code_block() {
        let text = "```rust\nfn main() {}\n```";
        let lines = render_markdown(text);
        // Should have: language hint + code line (fences consumed)
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn heading() {
        let lines = render_markdown("# Title\n## Subtitle\nBody");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn blockquote() {
        let lines = render_markdown("> quoted text");
        assert_eq!(lines.len(), 1);
        // First span is paragraph indent, second is the bar character
        assert!(lines[0].spans[1].content.contains('▎'));
    }

    #[test]
    fn inline_code() {
        let spans = parse_inline("use `foo` here", Style::default());
        assert!(spans.len() >= 3);
    }

    #[test]
    fn inline_bold() {
        let spans = parse_inline("this is **bold** text", Style::default());
        assert!(spans.len() >= 3);
    }

    #[test]
    fn empty_text() {
        let lines = render_markdown("");
        assert_eq!(lines.len(), 0);
    }
}
