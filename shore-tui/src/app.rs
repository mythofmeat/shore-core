use shore_protocol::types::{CharacterInfo, ImageRef, StreamMetadata, TokenCounts};

use crate::images::ImageCache;

/// A single entry in the conversation log.
#[derive(Clone, Debug)]
pub enum ConversationEntry {
    User {
        content: String,
        images: Vec<ImageRef>,
        timestamp: String,
    },
    Assistant {
        content: String,
        images: Vec<ImageRef>,
        timestamp: String,
        metadata: Option<StreamMetadata>,
    },
    System {
        content: String,
        timestamp: String,
    },
    Thinking {
        content: String,
    },
    ToolCall {
        #[allow(dead_code)] // stored for protocol fidelity; TUI renders by tool_name
        tool_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
        #[allow(dead_code)] // stored for protocol fidelity; TUI renders by tool_name
        tool_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
}

/// Streaming state for in-progress responses.
#[derive(Default)]
pub struct StreamState {
    pub active: bool,
    pub regen: bool,
    pub text: String,
    pub thinking: String,
    pub thinking_collapsed: bool,
    pub phase: String,
    /// Name of the tool currently being called/executed.
    pub tool_name: Option<String>,
}

impl StreamState {
    pub fn reset(&mut self) {
        self.active = false;
        self.regen = false;
        self.text.clear();
        self.thinking.clear();
        self.thinking_collapsed = false;
        self.phase.clear();
        self.tool_name = None;
    }
}

/// Input editor mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Insert,
    Command,
}

/// Input editor state.
pub struct InputState {
    pub text: String,
    pub cursor: usize,
    pub mode: InputMode,
    /// Separate buffer for command palette input.
    pub cmd_text: String,
    pub cmd_cursor: usize,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            mode: InputMode::Insert,
            cmd_text: String::new(),
            cmd_cursor: 0,
        }
    }
}

impl InputState {
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Insert a string at the cursor position (used for paste).
    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.text.drain(self.cursor..next);
        }
    }

    pub fn backspace_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.text[..self.cursor];
        // Skip trailing whitespace, then skip the word
        let after_ws = before.trim_end_matches(|c: char| c.is_whitespace());
        let after_word = after_ws.trim_end_matches(|c: char| !c.is_whitespace());
        let new_cursor = after_word.len();
        self.text.drain(new_cursor..self.cursor);
        self.cursor = new_cursor;
    }

    pub fn delete_word(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let after = &self.text[self.cursor..];
        // Skip leading whitespace, then skip the word
        let after_ws = after.trim_start_matches(|c: char| c.is_whitespace());
        let after_word = after_ws.trim_start_matches(|c: char| !c.is_whitespace());
        let delete_len = after.len() - after_word.len();
        self.text.drain(self.cursor..self.cursor + delete_len);
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
        }
    }

    pub fn move_home(&mut self) {
        // Move to start of current line
        let before = &self.text[..self.cursor];
        self.cursor = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    pub fn move_end(&mut self) {
        // Move to end of current line
        let after = &self.text[self.cursor..];
        self.cursor = after
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    pub fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        text
    }

    pub fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    /// Visual line count accounting for word-wrap at the given content width.
    pub fn visual_line_count(&self, content_width: usize) -> usize {
        let starts = word_wrap_offsets(&self.text, content_width);
        let count = starts.len();

        // Add an extra line when the last visual line fills the width entirely,
        // so the cursor has room to sit on the next line at the boundary.
        if content_width > 0 && count > 0 {
            let last_start = starts[count - 1];
            let last_width: usize = self.text[last_start..]
                .chars()
                .take_while(|&c| c != '\n')
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            if last_width >= content_width {
                return count + 1;
            }
        }

        count.max(1)
    }

    pub fn enter_command_mode(&mut self) {
        self.mode = InputMode::Command;
        self.cmd_text.clear();
        self.cmd_cursor = 0;
    }

    pub fn exit_command_mode(&mut self) {
        self.mode = InputMode::Normal;
        self.cmd_text.clear();
        self.cmd_cursor = 0;
    }

    pub fn cmd_insert_char(&mut self, c: char) {
        self.cmd_text.insert(self.cmd_cursor, c);
        self.cmd_cursor += c.len_utf8();
    }

    pub fn cmd_backspace(&mut self) {
        if self.cmd_cursor > 0 {
            let prev = self.cmd_text[..self.cmd_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cmd_text.drain(prev..self.cmd_cursor);
            self.cmd_cursor = prev;
        }
    }

    pub fn take_cmd_text(&mut self) -> String {
        let text = std::mem::take(&mut self.cmd_text);
        self.cmd_cursor = 0;
        self.mode = InputMode::Normal;
        text
    }
}

/// Compute visual line start byte-offsets for word-wrapped text.
///
/// Returns a `Vec<usize>` where each entry is the byte index where a visual
/// line begins. The first entry is always `0`. Breaks happen at word
/// boundaries (spaces) when possible; falls back to character wrapping for
/// words longer than `max_width`.
pub fn word_wrap_offsets(text: &str, max_width: usize) -> Vec<usize> {
    let mut starts = vec![0usize];

    if max_width == 0 {
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                starts.push(i + ch.len_utf8());
            }
        }
        return starts;
    }

    let mut col: usize = 0;
    // Byte offset AFTER the last space on the current visual line.
    let mut last_space_after: Option<usize> = None;
    // Column value at the byte after that space.
    let mut col_at_space_after: usize = 0;

    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(i + ch.len_utf8());
            col = 0;
            last_space_after = None;
            continue;
        }

        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);

        if col + w > max_width {
            if ch == ' ' {
                // Space at the overflow point — consume it as a line break.
                starts.push(i + ch.len_utf8());
                col = 0;
                last_space_after = None;
            } else if let Some(brk) = last_space_after {
                // Break at the previous word boundary.
                starts.push(brk);
                col = col - col_at_space_after + w;
                // Rescan for spaces between `brk` and `i` on the new line.
                last_space_after = None;
                for (j, c) in text[brk..i].char_indices() {
                    if c == ' ' {
                        let after = brk + j + c.len_utf8();
                        last_space_after = Some(after);
                        col_at_space_after = text[brk..after]
                            .chars()
                            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0))
                            .sum();
                    }
                }
            } else {
                // No space on this line — fall back to character wrap.
                starts.push(i);
                col = w;
                last_space_after = None;
            }
        } else {
            if ch == ' ' {
                last_space_after = Some(i + ch.len_utf8());
                col_at_space_after = col + w;
            }
            col += w;
        }
    }

    starts
}

/// Connection status for the status bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
}

/// Completion state for the command palette.
#[derive(Default)]
pub struct CompletionState {
    /// Filtered candidates matching current input.
    pub candidates: Vec<String>,
    /// Currently selected index (None = no selection).
    pub selected: Option<usize>,
}

/// An image in the conversation, with its position in the rendered line list.
#[derive(Clone, Debug)]
pub struct ImageEntry {
    /// Cache key (image path).
    pub path: String,
    /// Display name for the status bar.
    pub display_name: String,
    /// Line index in the conversation lines vec where this image starts.
    pub line: usize,
}

/// Main application state.
pub struct App {
    pub entries: Vec<ConversationEntry>,
    pub stream: StreamState,
    pub input: InputState,
    pub completion: CompletionState,
    pub scroll_offset: u16,
    pub connection_status: ConnectionStatus,
    pub character_name: String,
    pub characters: Vec<CharacterInfo>,
    pub model_names: Vec<String>,
    pub show_model_list: bool,
    pub model: String,
    pub tokens: TokenCounts,
    pub is_private: bool,
    pub should_quit: bool,
    pub auto_scroll: bool,
    pub image_cache: ImageCache,
    pub show_thinking: bool,
    pub show_tools: bool,
    pub show_images: bool,
    pub show_help: bool,
    /// Images queued for attachment to the next outgoing message.
    pub pending_images: Vec<String>,
    /// When editing a message, holds the ref (e.g. "last", "-1") being edited.
    pub editing_ref: Option<String>,
    /// Index of all rendered images with their line positions, rebuilt each frame.
    pub image_index: Vec<ImageEntry>,
    /// When set, the fullscreen image viewer is active showing this image index.
    pub fullscreen: Option<usize>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            stream: StreamState::default(),
            input: InputState::default(),
            completion: CompletionState::default(),
            scroll_offset: 0,
            connection_status: ConnectionStatus::Disconnected,
            character_name: String::new(),
            characters: Vec::new(),
            model_names: Vec::new(),
            show_model_list: false,
            model: String::new(),
            tokens: TokenCounts {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            is_private: false,
            should_quit: false,
            auto_scroll: true,
            image_cache: ImageCache::new(),
            show_thinking: true,
            show_tools: true,
            show_images: true,
            show_help: false,
            pending_images: Vec::new(),
            editing_ref: None,
            image_index: Vec::new(),
            fullscreen: None,
        }
    }
}

impl App {
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Resolve a ref (e.g. "last", "-1", "-2") to the content of a
    /// User or Assistant entry for local editing preview.
    pub fn resolve_ref_content(&self, raw_ref: &str) -> Option<String> {
        // Filter to only User/Assistant entries (what the daemon considers messages).
        let messages: Vec<&ConversationEntry> = self
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    ConversationEntry::User { .. } | ConversationEntry::Assistant { .. }
                )
            })
            .collect();

        let entry = match raw_ref {
            "last" => messages.last().copied(),
            s if s.starts_with('-') => {
                let n: usize = s[1..].parse().ok()?;
                if n == 0 || n > messages.len() {
                    return None;
                }
                Some(messages[messages.len() - n])
            }
            _ => None,
        };

        entry.and_then(|e| match e {
            ConversationEntry::User { content, .. }
            | ConversationEntry::Assistant { content, .. } => Some(content.clone()),
            _ => None,
        })
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.entries.push(ConversationEntry::System {
            content: msg.into(),
            timestamp: String::new(),
        });
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Static command names for completion.
    const COMMANDS: &'static [&'static str] = &[
        "character",
        "compact",
        "delete",
        "edit",
        "help",
        "image",
        "memory",
        "model",
        "quit",
        "regen",
        "status",
        "sys",
    ];

    /// Update completion candidates based on current command input.
    pub fn update_completions(&mut self) {
        let input = &self.input.cmd_text;
        self.completion.selected = None;

        if input.is_empty() {
            // Show all commands
            self.completion.candidates = Self::COMMANDS.iter().map(|s| s.to_string()).collect();
            return;
        }

        let mut parts = input.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let has_space = parts.next().is_some();

        if !has_space {
            // Completing the command name
            self.completion.candidates = Self::COMMANDS
                .iter()
                .filter(|c| c.starts_with(cmd))
                .map(|s| s.to_string())
                .collect();
        } else {
            // Completing arguments
            let arg = input.split_once(' ').map(|x| x.1).unwrap_or("").trim();
            match cmd {
                "character" => {
                    self.completion.candidates = self
                        .characters
                        .iter()
                        .map(|c| c.name.clone())
                        .filter(|n| {
                            arg.is_empty() || n.to_lowercase().starts_with(&arg.to_lowercase())
                        })
                        .map(|n| format!("character {n}"))
                        .collect();
                }
                "model" => {
                    let mut candidates: Vec<String> = self
                        .model_names
                        .iter()
                        .filter(|n| {
                            arg.is_empty() || n.to_lowercase().starts_with(&arg.to_lowercase())
                        })
                        .map(|n| format!("model {n}"))
                        .collect();
                    if "reset".starts_with(&arg.to_lowercase()) {
                        candidates.push("model reset".into());
                    }
                    self.completion.candidates = candidates;
                }
                _ => {
                    self.completion.candidates.clear();
                }
            }
        }
    }

    /// Apply the currently selected completion to the command input.
    pub fn apply_completion(&mut self) {
        if let Some(idx) = self.completion.selected {
            if let Some(text) = self.completion.candidates.get(idx) {
                self.input.cmd_text = text.clone();
                self.input.cmd_cursor = text.len();
                // If completing a command name (no space), add a space
                if !text.contains(' ') {
                    self.input.cmd_text.push(' ');
                    self.input.cmd_cursor += 1;
                }
            }
        }
    }

    /// Cycle to the next completion candidate.
    pub fn next_completion(&mut self) {
        if self.completion.candidates.is_empty() {
            return;
        }
        self.completion.selected = Some(match self.completion.selected {
            Some(i) => (i + 1) % self.completion.candidates.len(),
            None => 0,
        });
        self.apply_completion();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_insert_and_backspace() {
        let mut input = InputState::default();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.text, "hi");
        assert_eq!(input.cursor, 2);
        input.backspace();
        assert_eq!(input.text, "h");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn input_newline() {
        let mut input = InputState::default();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        assert_eq!(input.text, "a\nb");
        assert_eq!(input.line_count(), 2);
    }

    #[test]
    fn input_navigation() {
        let mut input = InputState::default();
        for c in "hello".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.cursor, 5);
        input.move_left();
        assert_eq!(input.cursor, 4);
        input.move_right();
        assert_eq!(input.cursor, 5);
        input.move_home();
        assert_eq!(input.cursor, 0);
        input.move_end();
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn input_take_text() {
        let mut input = InputState::default();
        for c in "message".chars() {
            input.insert_char(c);
        }
        let text = input.take_text();
        assert_eq!(text, "message");
        assert_eq!(input.text, "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn input_delete() {
        let mut input = InputState::default();
        for c in "abc".chars() {
            input.insert_char(c);
        }
        input.move_home();
        input.delete();
        assert_eq!(input.text, "bc");
    }

    #[test]
    fn scroll_up_down() {
        let mut app = App::default();
        assert!(app.auto_scroll);
        app.scroll_up(5);
        assert_eq!(app.scroll_offset, 5);
        assert!(!app.auto_scroll);
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 2);
        app.scroll_down(10);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.auto_scroll);
    }
}
