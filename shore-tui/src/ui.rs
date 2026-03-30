use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, ConversationEntry, InputMode};
use crate::images;
use crate::markdown;

/// Render the full TUI layout.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    // Main layout: conversation | thinking (optional) | input
    let input_content_width = size.width as usize;
    let input_height = (app.input.visual_line_count(input_content_width) as u16 + 1).min(8);
    let has_thinking = app.stream.active && !app.stream.thinking.is_empty();
    let thinking_height = if has_thinking && !app.stream.thinking_collapsed {
        6
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),                  // conversation
            Constraint::Length(thinking_height), // thinking panel
            Constraint::Length(input_height),    // input
        ])
        .split(size);

    draw_conversation(frame, &mut *app, chunks[0]);

    if thinking_height > 0 {
        draw_thinking(frame, app, chunks[1]);
    }

    draw_input(frame, app, chunks[2]);

    // Draw completion popup over conversation area when in command mode
    if app.input.mode == InputMode::Command && !app.completion.candidates.is_empty() {
        draw_completions(frame, app, chunks[2]);
    }

    if app.show_help {
        draw_help(frame, size);
    }
}

/// Render accumulated thinking blocks as dimmed text under the character name.
fn flush_thinking(lines: &mut Vec<Line<'static>>, pending: &mut Vec<String>, show: bool) {
    if pending.is_empty() {
        return;
    }
    if !show {
        pending.clear();
        return;
    }
    let header_style = Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD);
    let content_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
    lines.push(Line::from(Span::styled("  ◆ thinking", header_style)));
    for thought in pending.drain(..) {
        for tline in thought.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {tline}"),
                content_style,
            )));
        }
    }
}

fn flush_tools(lines: &mut Vec<Line<'static>>, pending: &mut Vec<&ConversationEntry>, show: bool) {
    if !show {
        pending.clear();
        return;
    }
    for entry in pending.drain(..) {
        match entry {
            ConversationEntry::ToolCall { tool_name, input, .. } => {
                lines.push(Line::from(vec![
                    Span::styled("  ▶ ", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        tool_name.clone(),
                        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                    ),
                ]));
                let json = serde_json::to_string_pretty(input).unwrap_or_default();
                for jline in json.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("    {jline}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            ConversationEntry::ToolResult { tool_name, output, is_error, .. } => {
                let header_color = if *is_error { Color::Red } else { Color::Cyan };
                lines.push(Line::from(vec![
                    Span::styled("  ◀ ", Style::default().fg(header_color)),
                    Span::styled(
                        tool_name.clone(),
                        Style::default().fg(header_color).add_modifier(Modifier::BOLD),
                    ),
                ]));
                for oline in output.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("    {oline}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(""));
            }
            _ => {}
        }
    }
}

/// Squeeze runs of >1 consecutive blank lines down to at most 1.
fn squeeze_blank_lines(lines: &mut Vec<Line<'static>>) {
    let mut i = 0;
    let mut consecutive_blanks = 0u32;
    while i < lines.len() {
        if lines[i].width() == 0 {
            consecutive_blanks += 1;
            if consecutive_blanks > 1 {
                lines.remove(i);
                continue;
            }
        } else {
            consecutive_blanks = 0;
        }
        i += 1;
    }
}

/// Render the scrollable conversation log.
fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    // Build the border title
    let char_label = if app.character_name.is_empty() {
        " Conversation ".to_string()
    } else {
        format!(" {} ", app.character_name)
    };
    let base_title = if let Some(msg) = &app.status_message {
        format!(" {msg} ")
    } else if app.stream.active {
        if app.stream.phase.is_empty() {
            format!("{char_label} [streaming...]")
        } else {
            format!("{char_label} [{}]", app.stream.phase)
        }
    } else {
        char_label
    };
    let title = if !app.auto_scroll {
        format!("{base_title}  ↑ scrolled · G to return ")
    } else {
        base_title
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // When streaming, skip trailing Thinking entries — they're already shown
    // in the dedicated thinking panel below the conversation.
    let entry_count = if app.stream.active {
        let trailing_thinking = app
            .entries
            .iter()
            .rev()
            .take_while(|e| matches!(e, ConversationEntry::Thinking { .. }))
            .count();
        app.entries.len() - trailing_thinking
    } else {
        app.entries.len()
    };

    // Thinking and tool entries are deferred so they render under the assistant
    // name, not floating above it as if they're part of the user's message.
    let mut pending_thinking: Vec<String> = Vec::new();
    let mut pending_tools: Vec<&ConversationEntry> = Vec::new();

    for entry in app.entries[..entry_count].iter() {
        match entry {
            ConversationEntry::Thinking { content } => {
                pending_thinking.push(content.clone());
                continue;
            }
            ConversationEntry::ToolCall { .. } | ConversationEntry::ToolResult { .. } => {
                pending_tools.push(entry);
                continue;
            }
            _ => {}
        }

        match entry {
            ConversationEntry::User {
                content, images, ..
            } => {
                flush_thinking(&mut lines, &mut pending_thinking, app.show_thinking);
                flush_tools(&mut lines, &mut pending_tools, app.show_tools);
                lines.push(Line::from(Span::styled(
                    "You",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(markdown::render_markdown(content));
                render_images(&mut lines, images, &app.image_cache);
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
                // Render thinking and tool calls under the character name
                flush_thinking(&mut lines, &mut pending_thinking, app.show_thinking);
                flush_tools(&mut lines, &mut pending_tools, app.show_tools);
                lines.extend(markdown::render_markdown(content));
                render_images(&mut lines, images, &app.image_cache);
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
                flush_thinking(&mut lines, &mut pending_thinking, app.show_thinking);
                flush_tools(&mut lines, &mut pending_tools, app.show_tools);
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
            ConversationEntry::Thinking { .. }
            | ConversationEntry::ToolCall { .. }
            | ConversationEntry::ToolResult { .. } => unreachable!(),
        }
    }

    // Flush orphaned pending entries (e.g. tools mid-stream before response starts)
    flush_thinking(&mut lines, &mut pending_thinking, app.show_thinking);
    flush_tools(&mut lines, &mut pending_tools, app.show_tools);

    // Append in-progress streaming text (or typing indicator)
    if app.stream.active {
        let name = if app.character_name.is_empty() {
            "Assistant"
        } else {
            &app.character_name
        };
        if !app.stream.text.is_empty() {
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
            lines.push(Line::from("")); // match trailing blank of finalized entries
        } else {
            // Typing indicator — stream started but no text yet
            lines.push(Line::from(Span::styled(
                name.to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "···",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
            lines.push(Line::from("")); // match trailing blank of finalized entries
        }
    }

    // Empty state: show a welcome hint
    if lines.is_empty() && !app.stream.active {
        let hint_style = Style::default().fg(Color::DarkGray);
        lines.push(Line::from(Span::styled(
            "Press i to start typing, Enter to send",
            hint_style,
        )));
        lines.push(Line::from(Span::styled(
            "Esc for normal mode · : for commands",
            hint_style,
        )));
    }

    // Squeeze runs of blank lines (max 2 consecutive)
    squeeze_blank_lines(&mut lines);

    let content_width = area.width.saturating_sub(4) as u16; // 2-char padding each side
    let visible_height = area.height.saturating_sub(1); // account for TOP border

    // Use Paragraph::line_count for accurate visual line count that accounts
    // for ratatui's word-wrap algorithm (manual char-width division undershoots).
    let content_visual = Paragraph::new(Text::from(lines.clone()))
        .wrap(Wrap { trim: false })
        .line_count(content_width) as u16;

    // Bottom-anchor: pad short conversations so content sits near the input
    if content_visual < visible_height {
        let padding = (visible_height - content_visual) as usize;
        let mut padded = vec![Line::from(""); padding];
        padded.append(&mut lines);
        lines = padded;
    }

    // After padding, total visual = max(content_visual, visible_height)
    let total_visual = content_visual.max(visible_height);
    let max_scroll = total_visual.saturating_sub(visible_height);
    // Clamp scroll_offset so it never drifts past max_scroll (e.g. after
    // toggling thinking/tool blocks reduces content height).
    if app.scroll_offset > max_scroll {
        app.scroll_offset = max_scroll;
        if max_scroll == 0 {
            app.auto_scroll = true;
        }
    }
    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        max_scroll.saturating_sub(app.scroll_offset)
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(title)
                .border_style(Style::default().fg(Color::DarkGray))
                .padding(Padding::horizontal(2)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

/// Render image entries — kitty placeholders when available, text fallback otherwise.
fn render_images(
    lines: &mut Vec<Line<'static>>,
    img_refs: &[shore_protocol::types::ImageRef],
    cache: &images::ImageCache,
) {
    for img in img_refs {
        if let Some(transmitted) = cache.get(&img.path) {
            if let Some(cap) = &img.caption {
                lines.push(Line::from(Span::styled(
                    format!("  {cap}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.extend(images::placeholder_lines(transmitted));
        } else {
            lines.push(Line::from(Span::styled(
                format!(
                    "  [img: {}]",
                    img.caption.as_deref().unwrap_or(&img.path)
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
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
                    .borders(Borders::TOP)
                    .title(" [COMMAND] ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);

        // Cursor after the ":" prefix + cmd_cursor
        let cursor_x = 1 + unicode_width::UnicodeWidthStr::width(
            &app.input.cmd_text[..app.input.cmd_cursor],
        ) as u16;
        frame.set_cursor_position((area.x + cursor_x, area.y + 1));
        return;
    }

    // Calculate cursor visual position (needed for both scrolling and placement)
    let content_width = area.width as usize;
    let text_before_cursor = &app.input.text[..app.input.cursor];
    let mut cx: usize = 0;
    let mut cy: u16 = 0;
    for ch in text_before_cursor.chars() {
        if ch == '\n' {
            cx = 0;
            cy += 1;
        } else {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if content_width > 0 && cx + w > content_width {
                cy += 1;
                cx = w;
            } else {
                cx += w;
            }
        }
    }

    // If cursor lands exactly at the right edge, wrap to next line
    if content_width > 0 && cx >= content_width {
        cy += 1;
        cx = 0;
    }

    // Scroll input so cursor line is always visible
    let content_height = area.height.saturating_sub(1);
    let input_scroll = if cy >= content_height {
        cy - content_height + 1
    } else {
        0
    };

    // Show placeholder when input is empty in insert mode
    let show_placeholder = app.input.text.is_empty() && app.input.mode == InputMode::Insert;
    let input_content: Text = if show_placeholder {
        Text::from(Line::from(Span::styled(
            "Type a message...",
            Style::default().fg(Color::DarkGray),
        )))
    } else {
        Text::from(app.input.text.as_str())
    };

    let (mode_label, border_color) = match app.input.mode {
        InputMode::Insert => (" [INSERT] ", Color::Cyan),
        InputMode::Normal => (" [NORMAL] ", Color::DarkGray),
        InputMode::Command => unreachable!(),
    };
    let paragraph = Paragraph::new(input_content)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(mode_label)
                .border_style(Style::default().fg(border_color)),
        )
        .wrap(Wrap { trim: false })
        .scroll((input_scroll, 0));

    frame.render_widget(paragraph, area);

    // Show cursor in insert mode
    if app.input.mode == InputMode::Insert {
        frame.set_cursor_position((
            area.x + cx as u16,
            area.y + 1 + cy - input_scroll,
        ));
    }
}

/// Render the keyboard shortcuts help overlay.
fn draw_help(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("    j / k           scroll down / up", Style::default().fg(Color::White))),
        Line::from(Span::styled("    d / u           scroll down / up (10 lines)", Style::default().fg(Color::White))),
        Line::from(Span::styled("    G               jump to bottom", Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Input", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("    i / a / I / A   enter insert mode", Style::default().fg(Color::White))),
        Line::from(Span::styled("    Enter           send message", Style::default().fg(Color::White))),
        Line::from(Span::styled("    Shift+Enter     newline", Style::default().fg(Color::White))),
        Line::from(Span::styled("    Ctrl+G          open input in $EDITOR", Style::default().fg(Color::White))),
        Line::from(Span::styled("    Esc             normal mode", Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Toggles", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("    Tab             toggle live thinking panel", Style::default().fg(Color::White))),
        Line::from(Span::styled("    t               toggle thinking blocks", Style::default().fg(Color::White))),
        Line::from(Span::styled("    T               toggle tool-use blocks", Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Commands  ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("(press : to open)", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled("    :help           this screen", Style::default().fg(Color::White))),
        Line::from(Span::styled("    :character      switch character", Style::default().fg(Color::White))),
        Line::from(Span::styled("    :model          switch model", Style::default().fg(Color::White))),
        Line::from(Span::styled("    :quit           exit", Style::default().fg(Color::White))),
        Line::from(Span::styled("    :log  :memory  :compact  :config  :diag", Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from(Span::styled("  Press any key to close", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC))),
        Line::from(""),
    ];

    let height = lines.len() as u16 + 2;
    let width = 56u16.min(area.width);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width, height);

    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Keyboard Shortcuts ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .style(Style::default().bg(Color::Rgb(20, 20, 30)));

    frame.render_widget(ratatui::widgets::Clear, popup_area);
    frame.render_widget(popup, popup_area);
}

/// Render completion candidates as a popup above the input area.
fn draw_completions(frame: &mut Frame, app: &App, input_area: Rect) {
    let candidates = &app.completion.candidates;
    let max_visible = 8u16;
    let count = (candidates.len() as u16).min(max_visible);
    if count == 0 {
        return;
    }

    // Calculate max width from candidates
    let max_width = candidates
        .iter()
        .take(max_visible as usize)
        .map(|c| c.len() as u16)
        .max()
        .unwrap_or(10)
        + 4; // padding + borders
    let width = max_width.min(input_area.width);
    let height = count + 2; // borders

    // Position above the input area
    let y = input_area.y.saturating_sub(height);
    let popup_area = Rect::new(input_area.x, y, width, height);

    // Build lines with highlighting
    let lines: Vec<Line<'static>> = candidates
        .iter()
        .take(max_visible as usize)
        .enumerate()
        .map(|(i, c)| {
            let selected = app.completion.selected == Some(i);
            if selected {
                Line::from(Span::styled(
                    format!(" {c} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow),
                ))
            } else {
                Line::from(Span::styled(
                    format!(" {c} "),
                    Style::default().fg(Color::White),
                ))
            }
        })
        .collect();

    // Clear background and render
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().bg(Color::Rgb(40, 40, 40)));

    frame.render_widget(ratatui::widgets::Clear, popup_area);
    frame.render_widget(popup, popup_area);
}

// ── Test harness ────────────────────────────────────────────────────────────

#[cfg(test)]
mod scenario_tests {
    use super::*;
    use crate::app::{App, ConnectionStatus, ConversationEntry, InputMode};
    use crate::input;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const W: u16 = 80;
    const H: u16 = 30;

    // ── Harness ─────────────────────────────────────────────────────────────

    struct Harness {
        terminal: Terminal<TestBackend>,
        app: App,
        frames: Vec<String>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_size(W, H)
        }

        fn with_size(w: u16, h: u16) -> Self {
            let backend = TestBackend::new(w, h);
            let terminal = Terminal::new(backend).unwrap();
            Self {
                terminal,
                app: App::default(),
                frames: Vec::new(),
            }
        }

        /// Render current app state and return the frame as text.
        fn render(&mut self, label: &str) -> String {
            self.terminal
                .draw(|frame| draw(frame, &mut self.app))
                .unwrap();
            let buf = self.terminal.backend().buffer();
            let area = buf.area;
            let mut text = String::new();
            for y in 0..area.height {
                for x in 0..area.width {
                    let cell = &buf[(x, y)];
                    text.push_str(cell.symbol());
                }
                // trim trailing whitespace per line for readability
                let trimmed_len = text.trim_end().len();
                text.truncate(trimmed_len);
                text.push('\n');
            }
            self.frames.push(text.clone());
            eprintln!("═══ {label} ═══\n{text}");
            text
        }

        /// Press a key with no modifiers.
        fn press(&mut self, code: KeyCode) {
            self.press_mod(KeyModifiers::NONE, code);
        }

        /// Press a key with modifiers.
        fn press_mod(&mut self, mods: KeyModifiers, code: KeyCode) {
            let ev = Event::Key(KeyEvent {
                code,
                modifiers: mods,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            });
            input::handle_event(&mut self.app, ev);
        }

        /// Type a string (handles shift for uppercase automatically).
        fn type_str(&mut self, s: &str) {
            for c in s.chars() {
                let mods = if c.is_ascii_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                };
                self.press_mod(mods, KeyCode::Char(c));
            }
        }

        /// Simulate StreamStart.
        fn stream_start(&mut self) {
            self.app.stream.reset();
            self.app.stream.active = true;
        }

        /// Simulate a text StreamChunk.
        fn stream_chunk(&mut self, text: &str) {
            self.app.stream.text.push_str(text);
            if self.app.auto_scroll {
                self.app.scroll_to_bottom();
            }
        }

        /// Simulate a thinking StreamChunk.
        fn thinking_chunk(&mut self, text: &str) {
            self.app.stream.thinking.push_str(text);
        }

        /// Simulate StreamEnd (finalise response into entries).
        fn stream_end(&mut self, content: &str) {
            self.app.entries.push(ConversationEntry::Assistant {
                content: content.to_string(),
                images: vec![],
                timestamp: String::new(),
                metadata: None,
            });
            self.app.stream.reset();
        }

        /// Lines changed between the last two frames.
        fn changed_lines(&self) -> Vec<(usize, String, String)> {
            if self.frames.len() < 2 {
                return vec![];
            }
            let prev: Vec<&str> = self.frames[self.frames.len() - 2].lines().collect();
            let curr: Vec<&str> = self.frames[self.frames.len() - 1].lines().collect();
            prev.iter()
                .zip(curr.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| (i, a.to_string(), b.to_string()))
                .collect()
        }

        /// Get a horizontal slice of the last rendered frame (row range).
        fn rows(&self, from: usize, to: usize) -> String {
            self.frames
                .last()
                .unwrap()
                .lines()
                .skip(from)
                .take(to - from)
                .collect::<Vec<_>>()
                .join("\n")
        }

    }

    // ── Scenario: empty state ───────────────────────────────────────────────

    #[test]
    fn scenario_empty_state() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.model = "gpt-4".into();
        h.app.character_name = "Alice".into();

        let f = h.render("empty state: connected, no messages");

        // Input area shows INSERT mode
        assert!(f.contains("[INSERT]"), "default mode is INSERT");

        // Conversation area shows character name in title
        assert!(f.contains(" Alice "), "conversation title shows character name");
    }

    // ── Scenario: type, send, stream, complete ──────────────────────────────

    #[test]
    fn scenario_full_message_cycle() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Narrator".into();
        h.app.model = "claude-3".into();

        // 1. Initial
        h.render("initial");

        // 2. Type a message
        h.type_str("Hello, world!");
        let f = h.render("after typing");
        assert!(f.contains("Hello, world!"), "typed text visible in input");

        // 3. Send (Enter)
        h.press(KeyCode::Enter);
        // Manually add the user entry (normally the daemon echoes it back)
        h.app.entries.push(ConversationEntry::User {
            content: "Hello, world!".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        let f = h.render("after send");
        // Input should be cleared
        assert!(
            !h.rows(H as usize - 4, H as usize - 1).contains("Hello, world!"),
            "input area should be cleared after send"
        );
        // User message should appear in conversation
        assert!(f.contains("You"), "user label visible");
        assert!(f.contains("Hello, world!"), "user message in conversation");

        // 4. Stream starts
        h.stream_start();
        let f = h.render("stream started");
        assert!(
            f.contains("[streaming...]"),
            "streaming indicator in conversation title"
        );

        // 5. First chunk
        h.stream_chunk("Hi there");
        let f = h.render("first chunk");
        assert!(f.contains("Hi there"), "streamed text visible");
        assert!(f.contains("Narrator"), "assistant name visible");

        // 6. More chunks
        h.stream_chunk(", how are you today?");
        let f = h.render("more chunks");
        assert!(
            f.contains("Hi there, how are you today?"),
            "accumulated text visible"
        );

        // Check layout stability: only conversation content should change,
        // not the input area or status bar structure
        let diffs = h.changed_lines();
        eprintln!("Lines changed from chunk 1→2: {}", diffs.len());
        for (i, prev, curr) in &diffs {
            eprintln!("  L{i}: {prev:?} → {curr:?}");
        }

        // 7. Stream ends
        h.stream_end("Hi there, how are you today?");
        let f = h.render("stream ended");
        assert!(
            !f.contains("[streaming...]"),
            "streaming indicator gone after end"
        );
        assert!(
            f.contains("Hi there, how are you today?"),
            "final response visible"
        );
    }

    // ── Scenario: thinking panel toggle ─────────────────────────────────────

    #[test]
    fn scenario_thinking_toggle() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Start a stream with thinking
        h.app.entries.push(ConversationEntry::User {
            content: "Think about this".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        h.stream_start();
        h.thinking_chunk("Let me consider...\nFirst, I need to...\nThen...");
        h.stream_chunk("Here's my answer");

        // Render with thinking visible
        let f1 = h.render("thinking visible");
        assert!(f1.contains("Thinking"), "thinking panel header visible");
        assert!(f1.contains("[streaming...]"), "streaming indicator in conversation title");

        // Toggle thinking off (Tab)
        h.press(KeyCode::Tab);
        let f2 = h.render("thinking collapsed");
        assert!(
            !f2.contains("Thinking (Tab to toggle)"),
            "thinking panel hidden after toggle"
        );

        // Check: conversation content should still be visible (not pushed off screen)
        assert!(
            f2.contains("Here's my answer"),
            "streaming text still visible after collapse"
        );
        assert!(f2.contains("You"), "user message still visible after collapse");

        // Toggle back
        h.press(KeyCode::Tab);
        let f3 = h.render("thinking re-expanded");
        assert!(
            f3.contains("Thinking"),
            "thinking panel back after re-toggle"
        );
    }

    // ── Scenario: command palette ───────────────────────────────────────────

    #[test]
    fn scenario_command_palette() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.input.mode = InputMode::Normal;

        h.render("normal mode");

        // Open command palette with ':'
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Char(':'));
        let f = h.render("command palette open");
        assert!(f.contains("COMMAND"), "command mode title visible");

        // Type a partial command
        h.type_str("mod");
        let f = h.render("typing 'mod'");
        assert!(f.contains(":mod"), "command text visible");
        // Should show completion for 'model'
        assert!(f.contains("model"), "model completion visible");

        // Tab to select
        h.press(KeyCode::Tab);
        let f = h.render("after tab completion");
        assert!(f.contains(":model"), "completion applied");

        // Escape to cancel
        h.press(KeyCode::Esc);
        let f = h.render("after escape");
        assert!(!f.contains("COMMAND"), "command palette hidden after escape");
    }

    // ── Scenario: scroll during stream ──────────────────────────────────────

    #[test]
    fn scenario_scroll_during_stream() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Fill with enough messages to require scrolling
        for i in 0..20 {
            h.app.entries.push(ConversationEntry::User {
                content: format!("Message {i}"),
                images: vec![],
                timestamp: format!("t{i}"),
            });
            h.app.entries.push(ConversationEntry::Assistant {
                content: format!("Reply {i}"),
                images: vec![],
                timestamp: format!("r{i}"),
                metadata: None,
            });
        }

        h.render("many messages - auto scroll");

        // Start streaming
        h.stream_start();
        h.stream_chunk("New streaming response...");
        let f = h.render("streaming with auto_scroll");
        assert!(
            f.contains("New streaming response"),
            "latest content visible with auto_scroll"
        );

        // Scroll up (exit auto_scroll)
        h.press_mod(KeyModifiers::CONTROL, KeyCode::Char('u'));
        h.render("scrolled up");
        assert!(!h.app.auto_scroll, "auto_scroll disabled after scroll up");

        // New chunk arrives while scrolled up
        h.stream_chunk(" More text arrives.");
        let _f = h.render("chunk while scrolled up");
        // The viewport should NOT jump — the user scrolled away intentionally
        // (The content is still being buffered, just not forced into view)

        // Shift+G to go back to bottom
        h.app.input.mode = InputMode::Normal;
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Char('G'));
        let f = h.render("back to bottom");
        assert!(h.app.auto_scroll, "auto_scroll re-enabled");
        assert!(
            f.contains("More text arrives"),
            "latest content visible after re-scroll"
        );
    }

    // ── Scenario: mode switching ────────────────────────────────────────────

    #[test]
    fn scenario_mode_switching() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Default is Insert
        let f = h.render("insert mode");
        assert!(f.contains("[INSERT]"), "starts in INSERT mode");

        // Esc → Normal
        h.press(KeyCode::Esc);
        let f = h.render("normal mode");
        assert!(f.contains("[NORMAL]"), "shows NORMAL after Esc");
        assert!(!f.contains("[INSERT]"), "INSERT label gone");

        // i → back to Insert
        h.press(KeyCode::Char('i'));
        let f = h.render("back to insert");
        assert!(f.contains("[INSERT]"), "shows INSERT after 'i'");

        // The only changes should be the mode label and placeholder.
        let diffs = h.changed_lines();
        assert!(
            diffs.iter().all(|(_, _, curr)| curr.contains("INSERT") || curr.contains("Type a message")),
            "mode switch should only change the input area"
        );
    }

    // ── Scenario: connection status changes ─────────────────────────────────

    #[test]
    fn scenario_connection_states() {
        // Connection status indicators were in the removed status bar.
        // Just verify the layout renders without panic across all states.
        let mut h = Harness::new();

        h.app.connection_status = ConnectionStatus::Disconnected;
        h.render("disconnected");

        h.app.connection_status = ConnectionStatus::Connecting;
        h.render("connecting");

        h.app.connection_status = ConnectionStatus::Connected;
        h.render("connected");
    }

    // ── Scenario: long message wrapping ─────────────────────────────────────

    #[test]
    fn scenario_long_message_wrapping() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Add a message longer than terminal width
        let long_msg = "This is a very long message that should wrap properly across multiple lines in the conversation area without clipping or causing layout issues.";
        h.app.entries.push(ConversationEntry::User {
            content: long_msg.into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        let f = h.render("long message");
        // The message should be present (may be split across lines)
        assert!(f.contains("This is a very long message"), "start of message visible");

        // Type a long input too
        h.type_str("Another really long input message that should cause the input area to grow taller as the text wraps to accommodate");
        let _f = h.render("long input");
        // Input area should have grown
        // The input constraint is (line_count + 2).min(8)
    }

    // ── Scenario: tool call display ─────────────────────────────────────────

    #[test]
    fn scenario_tool_calls() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::User {
            content: "Search for foo".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        h.app.entries.push(ConversationEntry::ToolCall {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            input: serde_json::json!({"query": "foo bar baz"}),
        });

        h.app.entries.push(ConversationEntry::ToolResult {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            output: "Found 3 results for foo bar baz".into(),
            is_error: false,
        });

        let f = h.render("tool call + result");
        assert!(f.contains("▶"), "tool call arrow present");
        assert!(f.contains("web_search"), "tool name present");
        assert!(f.contains("◀"), "tool result arrow present");
    }

    // ── Scenario: tool calls render under assistant name ─────────────────────

    #[test]
    fn scenario_tool_calls_under_assistant_name() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();

        h.app.entries.push(ConversationEntry::User {
            content: "Search for foo".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        // Tool entries come before the Assistant entry (as expand_msg produces them)
        h.app.entries.push(ConversationEntry::ToolCall {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            input: serde_json::json!({"query": "foo"}),
        });
        h.app.entries.push(ConversationEntry::ToolResult {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            output: "Result: foo page".into(),
            is_error: false,
        });
        h.app.entries.push(ConversationEntry::Assistant {
            content: "I found foo.".into(),
            images: vec![],
            timestamp: "t2".into(),
            metadata: None,
        });

        let f = h.render("tools under assistant name");

        // Find line positions
        let lines: Vec<&str> = f.lines().collect();
        let alice_line = lines.iter().position(|l| l.contains("Alice"))
            .expect("Alice name must appear");
        let tool_line = lines.iter().position(|l| l.contains("▶"))
            .expect("tool call arrow must appear");
        let result_line = lines.iter().position(|l| l.contains("◀"))
            .expect("tool result arrow must appear");
        let content_line = lines.iter().position(|l| l.contains("I found foo"))
            .expect("assistant content must appear");

        assert!(tool_line > alice_line, "tool call must appear after assistant name");
        assert!(result_line > tool_line, "tool result must appear after tool call");
        assert!(content_line > result_line, "assistant text must appear after tool result");
    }

    // ── Scenario: narrow terminal ───────────────────────────────────────────

    #[test]
    fn scenario_narrow_terminal() {
        let mut h = Harness::with_size(40, 20);
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();
        h.app.model = "claude-3-opus".into();

        h.app.entries.push(ConversationEntry::User {
            content: "Hi there!".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        let f = h.render("narrow terminal");
        // Everything should still be visible, just tighter
        assert!(f.contains("You"), "user label visible in narrow");
        assert!(f.contains("Hi there!"), "message visible in narrow");
        // Status bar might truncate but shouldn't crash
    }

    // ── Scenario: stream→end content consistency ────────────────────────────

    #[test]
    fn scenario_stream_to_final_transition() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::User {
            content: "Tell me a story".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        h.stream_start();
        h.stream_chunk("Once upon a time, there was a brave knight.");
        let f_streaming = h.render("during stream");

        // End stream with same content
        h.stream_end("Once upon a time, there was a brave knight.");
        let f_final = h.render("after stream end");

        // The conversation content should be visually identical
        // (minus the [streaming...] indicator)
        // Check that the story text is in the same position
        let story_line_streaming = f_streaming
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("Once upon a time"));
        let story_line_final = f_final
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("Once upon a time"));

        if let (Some((ls, _)), Some((lf, _))) = (story_line_streaming, story_line_final) {
            let jump = (ls as i32 - lf as i32).unsigned_abs();
            eprintln!("Story line position: streaming=L{ls}, final=L{lf}, jump={jump}");
            assert!(
                jump <= 1,
                "content should not jump more than 1 line during stream→final transition (jumped {jump})"
            );
        }
    }

    // ── Scenario: rapid send + stream (the "popin" feel) ────────────────────

    #[test]
    fn scenario_send_to_stream_latency() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Type and send
        h.type_str("Quick question");
        h.press(KeyCode::Enter);
        h.app.entries.push(ConversationEntry::User {
            content: "Quick question".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        let f_sent = h.render("just sent");

        // The typing indicator (···) should appear immediately after send,
        // before StreamStart arrives from the daemon.
        assert!(f_sent.contains("···"), "typing indicator should appear immediately after send");
        assert!(f_sent.contains("[streaming...]"), "streaming indicator in conversation title after send");

        // Stream starts (but no text yet)
        h.stream_start();
        let f_started = h.render("stream started, no text yet");
        assert!(f_started.contains("[streaming...]"), "streaming indicator visible even before first chunk");

        // First chunk arrives
        h.stream_chunk("The answer is...");
        let _f_first = h.render("first chunk arrives");

        // Check the transition from "stream started, no text" to "first chunk"
        let diffs = h.changed_lines();
        eprintln!("Lines changed on first chunk arrival: {}", diffs.len());
        for (i, _prev, curr) in &diffs {
            eprintln!("  L{i}: → {curr:?}");
        }
    }

    // ── Scenario: multi-line input growth ───────────────────────────────────

    #[test]
    fn scenario_input_growth() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Single line
        h.type_str("line 1");
        h.render("1 line input");

        // Add lines
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 2");
        h.render("2 line input");

        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 3");
        let f = h.render("3 line input");

        // The input area should have grown, eating into conversation space
        // Check that conversation area still has its title
        assert!(f.contains("Conversation"), "conversation title still present with multi-line input");

        // Keep adding lines up to the max (8 - 2 borders = 6 content lines)
        for i in 4..=7 {
            h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
            h.type_str(&format!("line {i}"));
        }
        let _f = h.render("7 line input (near max)");

        // Add one more — should cap at 8 total height
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 8");
        let _f = h.render("8 line input (at max)");

        // And another — shouldn't grow past 8
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 9");
        let f = h.render("9 line input (past max)");

        // The input area should be capped at 8 rows total
        // Conversation area must still have at least 3 rows (Min constraint)
        assert!(f.contains("Conversation"), "conversation still visible at max input height");
    }

    // ── Scenario: empty state welcome ───────────────────────────────────────

    #[test]
    fn scenario_empty_state_welcome() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        let f = h.render("empty with welcome");
        assert!(
            f.contains("Press i to start typing"),
            "welcome hint should appear when no messages"
        );
        assert!(
            f.contains("for commands"),
            "command hint should appear"
        );

        // Hint should disappear once we have messages
        h.app.entries.push(ConversationEntry::User {
            content: "Hello".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        let f = h.render("with message");
        assert!(
            !f.contains("Press i to start typing"),
            "welcome hint should disappear once there are messages"
        );
    }

    // ── Scenario: scroll-up indicator ───────────────────────────────────────

    #[test]
    fn scenario_scroll_indicator() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();

        // Fill conversation
        for i in 0..20 {
            h.app.entries.push(ConversationEntry::User {
                content: format!("Msg {i}"),
                images: vec![],
                timestamp: format!("t{i}"),
            });
        }

        let f = h.render("at bottom");
        assert!(f.contains(" Alice "), "title shows character name when at bottom");
        assert!(!f.contains("scrolled"), "no scroll indicator when at bottom");

        // Scroll up
        h.app.scroll_up(5);
        let f = h.render("scrolled up");
        assert!(f.contains("↑ scrolled"), "scroll indicator in conversation title");
        assert!(f.contains("G to return"), "hint how to get back");

        // Scroll back to bottom
        h.app.scroll_to_bottom();
        let f = h.render("back at bottom");
        assert!(!f.contains("scrolled"), "scroll indicator gone when back at bottom");
    }

    // ── Scenario: input placeholder ─────────────────────────────────────────

    #[test]
    fn scenario_input_placeholder() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Empty insert mode shows placeholder
        let f = h.render("empty insert mode");
        assert!(
            f.contains("Type a message"),
            "placeholder should show when input is empty"
        );

        // Typing removes placeholder
        h.type_str("h");
        let f = h.render("after typing one char");
        assert!(
            !f.contains("Type a message"),
            "placeholder should disappear when typing"
        );

        // Normal mode with empty input — no placeholder
        h.press(KeyCode::Backspace);
        h.press(KeyCode::Esc);
        let f = h.render("normal mode empty");
        assert!(
            !f.contains("Type a message"),
            "placeholder should not show in normal mode"
        );
    }

    // ── Scenario: phase display ─────────────────────────────────────────────

    #[test]
    fn scenario_phase_display() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Stream with no phase
        h.stream_start();
        let f = h.render("streaming, no phase");
        assert!(f.contains("[streaming...]"), "default streaming indicator in conversation title");

        // Set phase
        h.app.stream.phase = "thinking".into();
        let f = h.render("thinking phase");
        assert!(f.contains("[thinking]"), "should show phase name in title");
        assert!(!f.contains("[streaming...]"), "should replace generic indicator with phase");

        // Phase changes
        h.app.stream.phase = "responding".into();
        let f = h.render("responding phase");
        assert!(f.contains("[responding]"), "should update to new phase");
    }

    // ── Scenario: very short terminal ───────────────────────────────────────

    #[test]
    fn scenario_very_short_terminal() {
        let mut h = Harness::with_size(60, 10);
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Bob".into();

        h.app.entries.push(ConversationEntry::User {
            content: "Hello".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        h.app.entries.push(ConversationEntry::Assistant {
            content: "Hi there!".into(),
            images: vec![],
            timestamp: "t2".into(),
            metadata: None,
        });

        let f = h.render("short terminal with messages");
        // Should not panic and should show something useful
        assert!(f.contains("Bob"), "conversation title shows character name");
        assert!(f.contains("[INSERT]"), "input mode indicator visible");

        // Streaming in short terminal
        h.stream_start();
        h.stream_chunk("Response text");
        let f = h.render("streaming in short terminal");
        assert!(f.contains("[streaming...]") || f.contains("Response"),
            "should show either streaming indicator or content");
    }

    // ── Scenario: multiple tool calls ───────────────────────────────────────

    #[test]
    fn scenario_multiple_tool_calls() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::User {
            content: "Search and summarize".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        // First tool call + result
        h.app.entries.push(ConversationEntry::ToolCall {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            input: serde_json::json!({"query": "rust tui frameworks"}),
        });
        h.app.entries.push(ConversationEntry::ToolResult {
            tool_id: "tc1".into(),
            tool_name: "web_search".into(),
            output: "Found: ratatui, cursive, tui-rs".into(),
            is_error: false,
        });

        // Second tool call + result
        h.app.entries.push(ConversationEntry::ToolCall {
            tool_id: "tc2".into(),
            tool_name: "read_page".into(),
            input: serde_json::json!({"url": "https://ratatui.rs"}),
        });
        h.app.entries.push(ConversationEntry::ToolResult {
            tool_id: "tc2".into(),
            tool_name: "read_page".into(),
            output: "Ratatui is a Rust library for building terminal UIs".into(),
            is_error: false,
        });

        // Error result
        h.app.entries.push(ConversationEntry::ToolCall {
            tool_id: "tc3".into(),
            tool_name: "read_page".into(),
            input: serde_json::json!({"url": "https://404.example.com"}),
        });
        h.app.entries.push(ConversationEntry::ToolResult {
            tool_id: "tc3".into(),
            tool_name: "read_page".into(),
            output: "404 Not Found".into(),
            is_error: true,
        });

        let f = h.render("multiple tool calls");
        // All tool calls should be visible
        assert!(f.contains("web_search"), "first tool call visible");
        assert!(f.contains("read_page"), "second tool call visible");
        // Error should be distinguishable (we can't check color, but content is there)
        assert!(f.contains("404 Not Found"), "error result visible");
        // Tool calls should have the arrows
        let arrow_count = f.matches('▶').count();
        assert_eq!(arrow_count, 3, "should have 3 tool call arrows");
        let result_count = f.matches('◀').count();
        assert_eq!(result_count, 3, "should have 3 result arrows");
    }

    // ── Scenario: cursor position with wrapping ─────────────────────────────

    #[test]
    fn scenario_cursor_wrapping() {
        // Use a narrow terminal to force wrapping
        let mut h = Harness::with_size(30, 15);
        h.app.connection_status = ConnectionStatus::Connected;

        // Type enough text to cause wrapping (28 content chars per line)
        h.type_str("abcdefghijklmnopqrstuvwxyz12345678");
        let f = h.render("wrapped input text");

        // The text should visually wrap across multiple lines.
        // With 30-wide terminal and no side borders, 34 chars wraps after col 30:
        //   line 1: "abcdefghijklmnopqrstuvwxyz1234" (30 chars)
        //   line 2: "5678" (4 chars)
        let input_lines: Vec<&str> = f.lines()
            .filter(|l| l.contains("abcdef") || l.contains("5678"))
            .collect();
        assert!(
            input_lines.len() >= 2,
            "long input should wrap to multiple visual lines, got {} lines: {:?}",
            input_lines.len(), input_lines
        );
    }

    // ── Scenario: cursor at exact boundary ────────────────────────────────

    #[test]
    fn scenario_cursor_at_exact_boundary() {
        // Use 30-wide terminal → input content_width = 30 (Borders::TOP has no side borders)
        let mut h = Harness::with_size(30, 15);
        h.app.connection_status = ConnectionStatus::Connected;

        // Type exactly 30 characters to fill the first line
        let exact_line = "a".repeat(30);
        h.type_str(&exact_line);
        let _f = h.render("cursor at exact boundary");

        // Type one more character — it should appear on its own wrapped line
        h.type_str("x");
        let f = h.render("one char past boundary");
        let has_wrapped_x = f.lines().any(|l| l.starts_with('x'));
        assert!(has_wrapped_x, "character after boundary should appear on new wrapped line");
    }

    // ── Scenario: optimistic user message echo ──────────────────────────────

    #[test]
    fn scenario_optimistic_user_echo() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Type and send a message
        h.type_str("hello world");
        h.press(KeyCode::Enter);
        let f = h.render("after send");

        // User's message should appear immediately in conversation
        assert!(
            f.contains("hello world"),
            "user's message should be visible immediately after send"
        );
        assert!(f.contains("You"), "user label should be visible");
        // Typing indicator should also show
        assert!(
            f.contains("···"),
            "typing indicator should show alongside user message"
        );
    }

    // ── Scenario: thinking deduplication during streaming ───────────────────

    #[test]
    fn scenario_thinking_not_duplicated() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();

        // Add a user message
        h.app.entries.push(ConversationEntry::User {
            content: "hi".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        // Add thinking entry (as if from History rebuild during streaming)
        h.app.entries.push(ConversationEntry::Thinking {
            content: "thinking about response".into(),
        });

        // Start streaming with same thinking text
        h.stream_start();
        h.thinking_chunk("thinking about response");

        let f = h.render("streaming with thinking entries");

        // Thinking text should appear ONLY in the thinking panel, not in conversation
        let thinking_occurrences = f.matches("thinking about response").count();
        assert_eq!(
            thinking_occurrences, 1,
            "thinking text should appear exactly once (in thinking panel), not duplicated in conversation. Found {} occurrences",
            thinking_occurrences
        );
    }

    // ── Scenario: regeneration flow ─────────────────────────────────────────

    #[test]
    fn scenario_regeneration() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // Set up a conversation
        h.app.entries.push(ConversationEntry::User {
            content: "Tell me a joke".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        h.app.entries.push(ConversationEntry::Assistant {
            content: "Why did the chicken cross the road?".into(),
            images: vec![],
            timestamp: "t2".into(),
            metadata: None,
        });

        let f = h.render("before regen");
        assert!(f.contains("chicken"), "original response visible");

        // Simulate regeneration: remove last assistant, start stream
        h.app.stream.reset();
        h.app.stream.active = true;
        h.app.stream.regen = true;
        // Remove last assistant entry (as StreamStart handler does)
        if let Some(pos) = h.app.entries.iter()
            .rposition(|e| matches!(e, ConversationEntry::Assistant { .. }))
        {
            h.app.entries.truncate(pos);
        }

        let f = h.render("regen started");
        assert!(
            !f.contains("chicken"),
            "original response should be removed during regen"
        );

        // New response streams in
        h.stream_chunk("A better joke: ");
        let f = h.render("regen streaming");
        assert!(f.contains("(regenerating)"), "should show regen indicator");
        assert!(f.contains("A better joke"), "new response streaming");

        // Complete regen
        h.stream_end("A better joke: Why do programmers prefer dark mode?");
        let f = h.render("regen complete");
        assert!(
            f.contains("dark mode"),
            "regenerated response visible"
        );
        assert!(
            !f.contains("regenerating"),
            "regen indicator gone after completion"
        );
    }

    // ── Scenario: markdown code block in conversation ───────────────────────

    #[test]
    fn scenario_code_blocks() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::Assistant {
            content: "Here's some code:\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nThat should work.".into(),
            images: vec![],
            timestamp: "t1".into(),
            metadata: None,
        });

        let f = h.render("code block");
        assert!(f.contains("fn main()"), "code content visible");
        assert!(f.contains("rust"), "language hint visible");
        assert!(f.contains("That should work"), "text after code block visible");
    }

    // ── Scenario: status message in conversation title ────────────────────────

    #[test]
    fn scenario_status_bar_populated() {
        // The status bar was removed. Status messages now appear in the
        // conversation title. This test verifies the layout renders without panic
        // and status messages are visible.
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();
        h.app.set_status("conversation loaded");

        let f = h.render("with status message");
        assert!(f.contains("conversation loaded"), "status message visible in title");

        // Clearing status restores character name
        h.app.status_message = None;
        let f = h.render("status cleared");
        assert!(f.contains(" Alice "), "character name visible after status cleared");

        // Narrow terminal — should not panic
        let mut h2 = Harness::with_size(50, 20);
        h2.app = App {
            connection_status: ConnectionStatus::Connected,
            character_name: "Alice".into(),
            ..App::default()
        };
        h2.app.set_status("loaded");
        h2.render("narrow terminal with status");
    }

    // ── Scenario: conversation title shows character name ────────────────────

    #[test]
    fn scenario_dynamic_title() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        // No character — generic title
        h.app.entries.push(ConversationEntry::User {
            content: "Hi".into(),
            images: vec![],
            timestamp: "t1".into(),
        });
        let f = h.render("no character");
        assert!(f.contains(" Conversation "), "generic title when no character set");

        // With character
        h.app.character_name = "Luna".into();
        let f = h.render("with character");
        assert!(f.contains(" Luna "), "title should show character name");
        // "Conversation" welcome hints still appear in empty state, but we have a
        // User entry so only the title-based "Conversation" string is affected.
        // Check specifically that the border title changed:
        let title_line = f.lines().next().unwrap_or("");
        assert!(!title_line.contains("Conversation"), "border title replaced by character name");
    }

    // ── Scenario: system messages ───────────────────────────────────────────

    #[test]
    fn scenario_system_messages() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::System {
            content: "Memory updated: user prefers dark themes".into(),
            timestamp: "t1".into(),
        });
        h.app.entries.push(ConversationEntry::User {
            content: "Thanks".into(),
            images: vec![],
            timestamp: "t2".into(),
        });

        let f = h.render("system message");
        assert!(f.contains("System"), "system label visible");
        assert!(f.contains("Memory updated"), "system content visible");
        assert!(f.contains("You"), "user message after system");
    }

    // ── Scenario: error in streaming ────────────────────────────────────────

    #[test]
    fn scenario_error_during_stream() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::User {
            content: "Do something".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        // Stream starts
        h.stream_start();
        h.stream_chunk("Starting to respond...");
        h.render("streaming");

        // Error arrives — stream resets, error in status
        h.app.stream.reset();
        h.app.set_status("error: rate_limit - Too many requests");

        let f = h.render("after error");
        assert!(!f.contains("[streaming...]"), "streaming indicator gone after error");
        assert!(f.contains("rate_limit"), "error visible in conversation title");
        // The partial response is lost — this is the current behavior
        assert!(!f.contains("Starting to respond"), "partial stream text gone after reset");
    }

    // ── Scenario: reconnection during streaming ─────────────────────────────

    #[test]
    fn scenario_reconnect_during_stream() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;

        h.app.entries.push(ConversationEntry::User {
            content: "Long question".into(),
            images: vec![],
            timestamp: "t1".into(),
        });

        h.stream_start();
        h.stream_chunk("Partial response that gets cut off because");
        h.render("streaming before disconnect");

        // Connection drops — stream state is cleared by disconnect handler
        h.app.connection_status = ConnectionStatus::Connecting;
        h.app.stream.reset();
        h.app.set_status("reconnecting: connection lost");

        let f = h.render("disconnected while streaming");
        assert!(f.contains("reconnecting"), "reconnection status in conversation title");
        // Streaming indicator should be gone (stream was reset)
        assert!(!f.contains("[streaming...]"), "streaming indicator cleared on disconnect");
        // Partial stream text is lost on disconnect
        assert!(!f.contains("Partial response"), "partial stream text cleared on disconnect");
    }

    // ── Scenario: rapid message exchange ────────────────────────────────────

    #[test]
    fn scenario_rapid_exchange() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Bot".into();

        // Simulate rapid back-and-forth
        for i in 0..5 {
            h.app.entries.push(ConversationEntry::User {
                content: format!("Q{i}: What about this?"),
                images: vec![],
                timestamp: format!("u{i}"),
            });
            h.app.entries.push(ConversationEntry::Assistant {
                content: format!("A{i}: Here's my answer to that particular question."),
                images: vec![],
                timestamp: format!("a{i}"),
                metadata: None,
            });
        }

        let f = h.render("rapid exchange");
        // Most recent messages should be visible (bottom-anchored)
        assert!(f.contains("Q4"), "most recent user message visible");
        assert!(f.contains("A4"), "most recent response visible");

        // Check layout stability: render again, nothing should change
        let _f2 = h.render("same state re-render");
        let diffs = h.changed_lines();
        assert_eq!(
            diffs.len(), 0,
            "re-rendering same state should produce identical frame"
        );
    }
}
