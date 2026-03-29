use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, ConnectionStatus, ConversationEntry, InputMode};
use crate::images;
use crate::markdown;

/// Render the full TUI layout.
pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Main layout: conversation | thinking (optional) | input | status
    let input_content_width = size.width.saturating_sub(2) as usize; // borders
    let input_height = (app.input.visual_line_count(input_content_width) as u16 + 2).min(8);
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

    // Draw completion popup over conversation area when in command mode
    if app.input.mode == InputMode::Command && !app.completion.candidates.is_empty() {
        draw_completions(frame, app, chunks[2]);
    }
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
                lines.push(Line::from(""));
            }
        }
    }

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

    // Bottom-anchor: pad short conversations so content sits near the input
    let visible_height = area.height.saturating_sub(2); // account for borders
    let content_lines = lines.len() as u16;
    if content_lines < visible_height {
        let padding = (visible_height - content_lines) as usize;
        let mut padded = vec![Line::from(""); padding];
        padded.append(&mut lines);
        lines = padded;
    }

    // Calculate scroll
    let total_lines = lines.len() as u16;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        max_scroll.saturating_sub(app.scroll_offset)
    };

    // Dynamic title: character name + scroll indicator
    let title = if !app.auto_scroll {
        if !app.character_name.is_empty() {
            format!(" {} ── ↑ scrolled (G to return) ", app.character_name)
        } else {
            " Conversation ── ↑ scrolled (G to return) ".to_string()
        }
    } else if !app.character_name.is_empty() {
        format!(" {} ", app.character_name)
    } else {
        " Conversation ".to_string()
    };

    let title_style = if !app.auto_scroll {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title, title_style)),
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

    // Calculate cursor visual position (needed for both scrolling and placement)
    let content_width = area.width.saturating_sub(2) as usize;
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

    // Scroll input so cursor line is always visible
    let content_height = area.height.saturating_sub(2);
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

    let paragraph = Paragraph::new(input_content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(mode_label)
                .border_style(Style::default().fg(border_color)),
        )
        .wrap(Wrap { trim: false })
        .scroll((input_scroll, 0));

    frame.render_widget(paragraph, area);

    // Show cursor in insert mode
    if app.input.mode == InputMode::Insert {
        frame.set_cursor_position((
            area.x + 1 + cx as u16,
            area.y + 1 + cy - input_scroll,
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

    // Streaming / phase indicator
    if app.stream.active {
        let label = if !app.stream.phase.is_empty() {
            format!(" [{}]", app.stream.phase)
        } else {
            " [streaming...]".to_string()
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::Rgb(30, 30, 30)));

    frame.render_widget(bar, area);
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
                .draw(|frame| draw(frame, &self.app))
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

        /// Get the content of a specific row in the last frame.
        fn row(&self, idx: usize) -> &str {
            self.frames.last().unwrap().lines().nth(idx).unwrap_or("")
        }

        /// The last rendered frame as &str.
        fn last_frame(&self) -> &str {
            self.frames.last().unwrap()
        }
    }

    // ── UX checks ───────────────────────────────────────────────────────────

    /// Count how many lines changed between two frames, restricted to a row range.
    fn count_changes_in_region(
        prev: &str,
        curr: &str,
        row_start: usize,
        row_end: usize,
    ) -> usize {
        let prev: Vec<&str> = prev.lines().collect();
        let curr: Vec<&str> = curr.lines().collect();
        (row_start..row_end.min(prev.len()).min(curr.len()))
            .filter(|&i| prev.get(i) != curr.get(i))
            .count()
    }

    // ── Scenario: empty state ───────────────────────────────────────────────

    #[test]
    fn scenario_empty_state() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.model = "gpt-4".into();
        h.app.character_name = "Alice".into();

        let f = h.render("empty state: connected, no messages");

        // Status bar (last row)
        let status = h.row(H as usize - 1);
        assert!(status.contains('●'), "should show connected dot");
        assert!(status.contains("Alice"), "should show character name");
        assert!(status.contains("gpt-4"), "should show model name");

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
            "streaming indicator in status bar"
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
        assert!(
            f1.contains("[streaming...]"),
            "streaming indicator present"
        );

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
        assert!(f.contains("Command"), "command mode title visible");

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
        assert!(
            !f.contains("Command"),
            "command palette hidden after escape"
        );
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
        let f = h.render("chunk while scrolled up");
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
        // Border should be cyan (we can't check color in text, but we can
        // check the title)

        // Esc → Normal
        h.press(KeyCode::Esc);
        let f = h.render("normal mode");
        assert!(f.contains("[NORMAL]"), "shows NORMAL after Esc");
        assert!(!f.contains("[INSERT]"), "INSERT label gone");

        // i → back to Insert
        h.press(KeyCode::Char('i'));
        let f = h.render("back to insert");
        assert!(f.contains("[INSERT]"), "shows INSERT after 'i'");

        // Check that ONLY the input area changed (no conversation area flicker)
        let diffs = h.changed_lines();
        let conversation_changes = diffs
            .iter()
            .filter(|(line, _, _)| *line < (H as usize - 4))
            .count();
        assert_eq!(
            conversation_changes, 0,
            "mode switch should not change conversation area"
        );
    }

    // ── Scenario: connection status changes ─────────────────────────────────

    #[test]
    fn scenario_connection_states() {
        let mut h = Harness::new();

        // Disconnected
        h.app.connection_status = ConnectionStatus::Disconnected;
        let f = h.render("disconnected");
        let status = h.row(H as usize - 1);
        assert!(status.contains('○'), "disconnected shows empty circle");

        // Connecting
        h.app.connection_status = ConnectionStatus::Connecting;
        let f = h.render("connecting");
        let status = h.row(H as usize - 1);
        assert!(status.contains('◌'), "connecting shows dotted circle");

        // Connected
        h.app.connection_status = ConnectionStatus::Connected;
        let f = h.render("connected");
        let status = h.row(H as usize - 1);
        assert!(status.contains('●'), "connected shows filled circle");
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
        let f = h.render("long input");
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
        assert!(
            f_sent.contains("···"),
            "typing indicator should appear immediately after send"
        );
        assert!(
            f_sent.contains("[streaming...]"),
            "streaming indicator should appear immediately after send"
        );

        // Stream starts (but no text yet)
        h.stream_start();
        let f_started = h.render("stream started, no text yet");
        // The [streaming...] indicator should be visible
        assert!(
            f_started.contains("[streaming...]"),
            "streaming indicator visible even before first chunk"
        );

        // First chunk arrives
        h.stream_chunk("The answer is...");
        let f_first = h.render("first chunk arrives");

        // Check the transition from "stream started, no text" to "first chunk"
        let diffs = h.changed_lines();
        eprintln!("Lines changed on first chunk arrival: {}", diffs.len());
        for (i, prev, curr) in &diffs {
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
        // Check that conversation area is still functional
        assert!(
            f.contains("Conversation"),
            "conversation still has title with multi-line input"
        );

        // Keep adding lines up to the max (8 - 2 borders = 6 content lines)
        for i in 4..=7 {
            h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
            h.type_str(&format!("line {i}"));
        }
        let f = h.render("7 line input (near max)");

        // Add one more — should cap at 8 total height
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 8");
        let f = h.render("8 line input (at max)");

        // And another — shouldn't grow past 8
        h.press_mod(KeyModifiers::SHIFT, KeyCode::Enter);
        h.type_str("line 9");
        let f = h.render("9 line input (past max)");

        // The input area should be capped at 8 rows total
        // Conversation area must still have at least 3 rows (Min constraint)
        assert!(
            f.contains("Conversation"),
            "conversation still visible at max input height"
        );
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
        assert!(
            f.contains(" Alice "),
            "title should show character name when at bottom"
        );
        assert!(
            !f.contains("scrolled"),
            "no scroll indicator when at bottom"
        );

        // Scroll up
        h.app.scroll_up(5);
        let f = h.render("scrolled up");
        assert!(
            f.contains("↑ scrolled"),
            "should show scroll indicator when scrolled up"
        );
        assert!(
            f.contains("G to return"),
            "should hint how to get back"
        );

        // Scroll back to bottom
        h.app.scroll_to_bottom();
        let f = h.render("back at bottom");
        assert!(
            !f.contains("scrolled"),
            "scroll indicator gone when back at bottom"
        );
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
        assert!(f.contains("[streaming...]"), "default streaming indicator");

        // Set phase
        h.app.stream.phase = "thinking".into();
        let f = h.render("thinking phase");
        assert!(f.contains("[thinking]"), "should show phase name");
        assert!(
            !f.contains("[streaming...]"),
            "should replace generic indicator with phase"
        );

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
        assert!(f.contains("Bob"), "title or message should be visible");
        assert!(f.contains("[INSERT]"), "input still functional");

        // Streaming in short terminal
        h.stream_start();
        h.stream_chunk("Response text");
        let f = h.render("streaming in short terminal");
        assert!(f.contains("[streaming...]") || f.contains("Response"),
            "should show either status or content");
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

        // The text should visually wrap across multiple lines
        // With 28 chars content width, 34 chars should wrap to 2 lines
        let input_lines: Vec<&str> = f.lines()
            .filter(|l| l.contains("abcdef") || l.contains("345678"))
            .collect();
        assert!(
            input_lines.len() >= 2,
            "long input should wrap to multiple visual lines, got {} lines: {:?}",
            input_lines.len(), input_lines
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

    // ── Scenario: status bar overflow ────────────────────────────────────────

    #[test]
    fn scenario_status_bar_populated() {
        let mut h = Harness::new();
        h.app.connection_status = ConnectionStatus::Connected;
        h.app.character_name = "Alice".into();
        h.app.model = "claude-sonnet-4-20250514".into();
        h.app.tokens.input = 15000;
        h.app.tokens.output = 2000;
        h.app.tokens.cache_read = 12000;
        h.app.is_private = true;
        h.app.set_status("conversation loaded");

        let f = h.render("full status bar");
        let status = h.row(H as usize - 1);
        eprintln!("Status bar: {status:?}");

        assert!(status.contains('●'), "connection indicator");
        assert!(status.contains("Alice"), "character name");
        assert!(status.contains("claude"), "model name (possibly truncated)");
        assert!(status.contains("tok"), "token count");
        assert!(status.contains("cache:"), "cache ratio");
        assert!(status.contains("[private]"), "private indicator");

        // Now in a narrow terminal
        let mut h2 = Harness::with_size(50, 20);
        h2.app = App {
            connection_status: ConnectionStatus::Connected,
            character_name: "Alice".into(),
            model: "claude-sonnet-4-20250514".into(),
            is_private: true,
            ..App::default()
        };
        h2.app.set_status("loaded");

        let f = h2.render("narrow status bar");
        // Should not panic even if elements overflow
        let status = h2.row(19);
        eprintln!("Narrow status: {status:?}");
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
        assert!(
            f.contains(" Conversation "),
            "generic title when no character set"
        );

        // With character
        h.app.character_name = "Luna".into();
        let f = h.render("with character");
        assert!(
            f.contains(" Luna "),
            "title should show character name"
        );
        assert!(
            !f.contains("Conversation"),
            "generic title should be replaced by character name"
        );
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
        assert!(
            !f.contains("[streaming...]"),
            "streaming indicator should be gone after error"
        );
        assert!(
            f.contains("rate_limit"),
            "error should be visible in status bar"
        );
        // The partial response is lost — this is the current behavior
        assert!(
            !f.contains("Starting to respond"),
            "partial stream text should be gone after reset"
        );
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
        let status = h.row(H as usize - 1);
        assert!(status.contains('◌'), "should show connecting indicator");
        assert!(
            f.contains("reconnecting"),
            "should show reconnection status"
        );
        // Streaming indicator should be gone (stream was reset)
        assert!(
            !f.contains("[streaming...]"),
            "streaming indicator should be cleared on disconnect"
        );
        // Partial stream text is lost on disconnect
        assert!(
            !f.contains("Partial response"),
            "partial stream text should be cleared on disconnect"
        );
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
        let f2 = h.render("same state re-render");
        let diffs = h.changed_lines();
        assert_eq!(
            diffs.len(), 0,
            "re-rendering same state should produce identical frame"
        );
    }
}
