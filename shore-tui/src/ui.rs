use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, ConnectionStatus, ConversationEntry, InputMode};
use crate::markdown;

/// Render the full TUI layout.
pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Main layout: conversation | thinking (optional) | input | status
    let input_height = (app.input.line_count() as u16 + 2).min(8);
    let has_thinking = app.stream.active && !app.stream.thinking.is_empty();
    let thinking_height = if has_thinking && !app.stream.thinking_collapsed {
        6
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),             // conversation
            Constraint::Length(thinking_height), // thinking panel
            Constraint::Length(input_height), // input
            Constraint::Length(1),           // status bar
        ])
        .split(size);

    draw_conversation(frame, app, chunks[0]);

    if thinking_height > 0 {
        draw_thinking(frame, app, chunks[1]);
    }

    draw_input(frame, app, chunks[2]);
    draw_status_bar(frame, app, chunks[3]);
}

/// Render the scrollable conversation log.
fn draw_conversation(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in &app.entries {
        match entry {
            ConversationEntry::User {
                content, images, ..
            } => {
                lines.push(Line::from(Span::styled(
                    "You",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(markdown::render_markdown(content));
                for img in images {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  [img: {}]",
                            img.caption.as_deref().unwrap_or(&img.path)
                        ),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(""));
            }
            ConversationEntry::Assistant {
                content,
                metadata,
                images,
                ..
            } => {
                let name = if app.character_name.is_empty() {
                    "Assistant".to_string()
                } else {
                    app.character_name.clone()
                };
                lines.push(Line::from(Span::styled(
                    name,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(markdown::render_markdown(content));
                for img in images {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  [img: {}]",
                            img.caption.as_deref().unwrap_or(&img.path)
                        ),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                if let Some(meta) = metadata {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  [{} | in:{} out:{} cache:{} | {}ms]",
                            meta.model,
                            meta.tokens.input,
                            meta.tokens.output,
                            meta.tokens.cache_read,
                            meta.timing.total_ms,
                        ),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(""));
            }
            ConversationEntry::System { content, .. } => {
                lines.push(Line::from(Span::styled(
                    "System",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    content.clone(),
                    Style::default().fg(Color::Yellow),
                )));
                lines.push(Line::from(""));
            }
            ConversationEntry::ToolCall {
                tool_name, input, ..
            } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ▶ ",
                        Style::default().fg(Color::Magenta),
                    ),
                    Span::styled(
                        tool_name.clone(),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                let json = serde_json::to_string_pretty(input).unwrap_or_default();
                for jline in json.lines().take(5) {
                    lines.push(Line::from(Span::styled(
                        format!("    {jline}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            ConversationEntry::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => {
                let color = if *is_error { Color::Red } else { Color::DarkGray };
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ◀ ",
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        tool_name.clone(),
                        Style::default().fg(color),
                    ),
                ]));
                for oline in output.lines().take(3) {
                    lines.push(Line::from(Span::styled(
                        format!("    {oline}"),
                        Style::default().fg(color),
                    )));
                }
            }
        }
    }

    // Append in-progress streaming text
    if app.stream.active && !app.stream.text.is_empty() {
        let name = if app.character_name.is_empty() {
            "Assistant"
        } else {
            &app.character_name
        };
        if app.stream.regen {
            lines.push(Line::from(Span::styled(
                format!("{name} (regenerating)"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                name.to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.extend(markdown::render_markdown(&app.stream.text));
    }

    // Calculate scroll
    let total_lines = lines.len() as u16;
    let visible_height = area.height.saturating_sub(2); // account for borders
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        max_scroll.saturating_sub(app.scroll_offset)
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Conversation "),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

/// Render the collapsible thinking panel.
fn draw_thinking(frame: &mut Frame, app: &App, area: Rect) {
    let thinking_lines: Vec<Line<'static>> = app
        .stream
        .thinking
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::DarkGray),
            ))
        })
        .collect();

    let total = thinking_lines.len() as u16;
    let visible = area.height.saturating_sub(2);
    let scroll = total.saturating_sub(visible);

    let paragraph = Paragraph::new(Text::from(thinking_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Thinking (Tab to toggle) ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

/// Render the input area.
fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    if app.input.mode == InputMode::Command {
        // Command palette mode: show ":" prefix with command text
        let display = format!(":{}", app.input.cmd_text);
        let paragraph = Paragraph::new(display.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Command ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);

        // Cursor after the ":" prefix + cmd_cursor
        let cursor_x = 1 + unicode_width::UnicodeWidthStr::width(
            &app.input.cmd_text[..app.input.cmd_cursor],
        ) as u16;
        frame.set_cursor_position((area.x + 1 + cursor_x, area.y + 1));
        return;
    }

    let mode_label = match app.input.mode {
        InputMode::Normal => " Input [NORMAL] ",
        InputMode::Insert => " Input [INSERT] ",
        InputMode::Command => unreachable!(),
    };

    let border_color = match app.input.mode {
        InputMode::Normal => Color::DarkGray,
        InputMode::Insert => Color::Cyan,
        InputMode::Command => unreachable!(),
    };

    let paragraph = Paragraph::new(app.input.text.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(mode_label)
                .border_style(Style::default().fg(border_color)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);

    // Show cursor in insert mode
    if app.input.mode == InputMode::Insert {
        // Calculate cursor position within the input area
        let text_before_cursor = &app.input.text[..app.input.cursor];
        let lines: Vec<&str> = text_before_cursor.split('\n').collect();
        let cursor_y = lines.len().saturating_sub(1) as u16;
        let cursor_x = unicode_width::UnicodeWidthStr::width(
            *lines.last().unwrap_or(&""),
        ) as u16;

        frame.set_cursor_position((
            area.x + 1 + cursor_x,
            area.y + 1 + cursor_y,
        ));
    }
}

/// Render the status bar.
fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let conn_indicator = match app.connection_status {
        ConnectionStatus::Connected => Span::styled(
            " ● ",
            Style::default().fg(Color::Green),
        ),
        ConnectionStatus::Connecting => Span::styled(
            " ◌ ",
            Style::default().fg(Color::Yellow),
        ),
        ConnectionStatus::Disconnected => Span::styled(
            " ○ ",
            Style::default().fg(Color::Red),
        ),
    };

    let mut spans = vec![conn_indicator];

    // Character name
    if !app.character_name.is_empty() {
        spans.push(Span::styled(
            app.character_name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }

    // Model
    if !app.model.is_empty() {
        spans.push(Span::styled(
            format!("[{}] ", app.model),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Token count
    let total_tokens = app.tokens.input + app.tokens.output;
    if total_tokens > 0 {
        spans.push(Span::styled(
            format!("{}tok ", total_tokens),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Cache hit indicator
    if let Some(ratio) = app.cache_hit_ratio() {
        let color = if ratio > 0.5 { Color::Green } else { Color::Yellow };
        spans.push(Span::styled(
            format!("cache:{:.0}% ", ratio * 100.0),
            Style::default().fg(color),
        ));
    }

    // Private indicator
    if app.is_private {
        spans.push(Span::styled(
            "[private] ",
            Style::default().fg(Color::Red),
        ));
    }

    // Status message (right-aligned conceptually, appended)
    if let Some(msg) = &app.status_message {
        spans.push(Span::styled(
            format!("│ {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Streaming indicator
    if app.stream.active {
        spans.push(Span::styled(
            " [streaming...]",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::Rgb(30, 30, 30)));

    frame.render_widget(bar, area);
}
