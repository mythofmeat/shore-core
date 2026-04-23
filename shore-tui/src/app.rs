use shore_client::audio::AudioPlayer;
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{CharacterInfo, ImageRef, StreamMetadata, TokenCounts};

use crate::images::ImageCache;

/// A single entry in the conversation log.
#[derive(Clone, Debug)]
pub enum ConversationEntry {
    User {
        content: String,
        images: Vec<ImageRef>,
        #[allow(dead_code)] // captured from daemon; TUI does not render per-entry timestamps
        timestamp: String,
    },
    Assistant {
        content: String,
        images: Vec<ImageRef>,
        #[allow(dead_code)] // captured from daemon; TUI does not render per-entry timestamps
        timestamp: String,
        metadata: Option<StreamMetadata>,
    },
    System {
        content: String,
        /// Count of consecutive identical entries collapsed into this one.
        /// Starts at 1; incremented by `set_status` when the same message
        /// arrives repeatedly (e.g. a reconnect storm).
        count: u32,
        #[allow(dead_code)] // captured from daemon; TUI does not render per-entry timestamps
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

/// A segment of streaming content, preserving interleaving order.
#[derive(Clone, Debug)]
pub enum StreamBlock {
    Thinking(String),
    Text(String),
}

/// Streaming state for in-progress responses.
#[derive(Default)]
pub struct StreamState {
    pub active: bool,
    pub regen: bool,
    pub blocks: Vec<StreamBlock>,
    pub phase: String,
    /// Name of the tool currently being called/executed.
    pub tool_name: Option<String>,
    // Accumulated across a multi-phase (tool-use) turn so the finalized
    // Assistant entry carries the full text and summed metadata rather than
    // just the first LLM call's partial slice. Cleared by reset().
    pub accumulated_text: String,
    pub accumulated_metadata: Option<StreamMetadata>,
}

impl StreamState {
    pub fn reset(&mut self) {
        self.active = false;
        self.regen = false;
        self.blocks.clear();
        self.phase.clear();
        self.tool_name = None;
        self.accumulated_text.clear();
        self.accumulated_metadata = None;
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
    /// Set when quit was triggered by a SIGINT-equivalent (Ctrl+C keybind or
    /// external SIGINT). The shutdown path exits with code 130 afterward so
    /// supervisors see the conventional interrupt exit.
    pub interrupt: bool,
    pub auto_scroll: bool,
    pub image_cache: ImageCache,
    pub show_thinking: bool,
    pub show_tools: bool,
    pub show_images: bool,
    pub show_help: bool,
    /// Images queued for attachment to the next outgoing message.
    pub pending_images: Vec<String>,
    /// Temp-file paths for paste-origin images, removed on TUI shutdown.
    pub paste_temp_paths: Vec<std::path::PathBuf>,
    /// When editing a message, holds the ref (e.g. "last", "-1") being edited.
    pub editing_ref: Option<String>,
    /// Index of all rendered images with their line positions, rebuilt each frame.
    pub image_index: Vec<ImageEntry>,
    /// When set, the fullscreen image viewer is active showing this image index.
    pub fullscreen: Option<usize>,
    /// TTS audio player; opened lazily on first AudioStart.
    pub audio_player: Option<AudioPlayer>,
    /// Whether live-speak mode is enabled in this session.
    pub live_speak: bool,
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
            interrupt: false,
            auto_scroll: true,
            image_cache: ImageCache::new(),
            show_thinking: true,
            show_tools: true,
            show_images: true,
            show_help: false,
            pending_images: Vec::new(),
            paste_temp_paths: Vec::new(),
            editing_ref: None,
            image_index: Vec::new(),
            fullscreen: None,
            audio_player: None,
            live_speak: false,
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

    /// Optimistically transition into the "regenerating" UI state before the
    /// daemon's StreamStart arrives, so the spinner and (regenerating) label
    /// appear immediately. Mirrors what StreamStart does on receipt, so it is
    /// idempotent when the real StreamStart lands.
    //
    // Truncate everything *after* the last User entry — not from the last
    // Assistant entry. When the user deletes the last assistant and then
    // regenerates, there is a trailing User entry with no Assistant after it;
    // a last-Assistant truncation would wipe that user message. Keeping up to
    // and including the last User correctly scrubs prior assistant/tool/
    // thinking output regardless of whether a trailing assistant exists.
    pub fn begin_regen_optimistic(&mut self) {
        self.stream.reset();
        self.stream.active = true;
        self.stream.regen = true;
        if let Some(pos) = self
            .entries
            .iter()
            .rposition(|e| matches!(e, ConversationEntry::User { .. }))
        {
            self.entries.truncate(pos + 1);
        }
        self.scroll_to_bottom();
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

    /// Dispatch an audio-related server message into the TTS playback pipeline.
    pub fn handle_audio_message(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::AudioStart(start) => {
                if self.audio_player.is_none() {
                    match AudioPlayer::new() {
                        Ok(p) => self.audio_player = Some(p),
                        Err(e) => {
                            self.set_status(format!("audio unavailable: {e}"));
                            return;
                        }
                    }
                }
                if let Some(ref mut player) = self.audio_player {
                    player.start(start.sample_rate, start.channels);
                }
            }
            ServerMessage::AudioChunk(chunk) => {
                if let Some(ref player) = self.audio_player {
                    player.feed(&chunk.data);
                }
            }
            ServerMessage::AudioEnd(_) => {
                if let Some(ref player) = self.audio_player {
                    player.finish();
                }
            }
            ServerMessage::AudioError(err) => {
                self.set_status(format!("TTS error: {}", err.message));
            }
            _ => {}
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        // Dedupe consecutive identical system messages (e.g. a reconnect
        // storm emitting the same reason over and over). If the tail entry
        // is already a matching System, bump its count in place instead of
        // appending a new entry.
        if let Some(ConversationEntry::System { content, count, .. }) = self.entries.last_mut() {
            if *content == msg {
                *count = count.saturating_add(1);
                if self.auto_scroll {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
        self.entries.push(ConversationEntry::System {
            content: msg,
            count: 1,
            timestamp: String::new(),
        });
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Remove every System entry from the conversation log. Invoked by
    /// `:clear` and automatically at the moment the user sends a new
    /// message (fresh turn = clean slate).
    pub fn clear_system_entries(&mut self) {
        self.entries
            .retain(|e| !matches!(e, ConversationEntry::System { .. }));
    }

    /// Static command names for completion.
    const COMMANDS: &'static [&'static str] = &[
        "cancel",
        "character",
        "clear",
        "compact",
        "delete",
        "edit",
        "help",
        "image",
        "memory",
        "model",
        "quit",
        "regen",
        "speak",
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
                "image" => {
                    self.completion.candidates = ["clear"]
                        .iter()
                        .filter(|s| s.starts_with(&arg.to_lowercase()))
                        .map(|s| format!("image {s}"))
                        .collect();
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

    #[test]
    fn set_status_dedupes_consecutive_identical() {
        let mut app = App::default();
        app.set_status("reconnecting: connection lost");
        app.set_status("reconnecting: connection lost");
        app.set_status("reconnecting: connection lost");
        assert_eq!(app.entries.len(), 1);
        match &app.entries[0] {
            ConversationEntry::System { content, count, .. } => {
                assert_eq!(content, "reconnecting: connection lost");
                assert_eq!(*count, 3);
            }
            _ => panic!("expected a single System entry"),
        }
    }

    #[test]
    fn set_status_does_not_dedupe_when_interrupted() {
        let mut app = App::default();
        app.set_status("x");
        app.entries.push(ConversationEntry::User {
            content: "hi".into(),
            images: vec![],
            timestamp: String::new(),
        });
        app.set_status("x");
        let system_entries: Vec<_> = app
            .entries
            .iter()
            .filter(|e| matches!(e, ConversationEntry::System { .. }))
            .collect();
        assert_eq!(system_entries.len(), 2);
        for e in system_entries {
            if let ConversationEntry::System { count, .. } = e {
                assert_eq!(*count, 1);
            }
        }
    }

    #[test]
    fn set_status_does_not_dedupe_different_content() {
        let mut app = App::default();
        app.set_status("x");
        app.set_status("y");
        assert_eq!(app.entries.len(), 2);
    }

    #[test]
    fn clear_system_entries_preserves_other_entries() {
        let mut app = App::default();
        app.entries.push(ConversationEntry::User {
            content: "hello".into(),
            images: vec![],
            timestamp: String::new(),
        });
        app.set_status("reconnecting");
        app.entries.push(ConversationEntry::Assistant {
            content: "hi".into(),
            images: vec![],
            timestamp: String::new(),
            metadata: None,
        });
        app.set_status("cache warning");
        app.clear_system_entries();
        assert_eq!(app.entries.len(), 2);
        assert!(matches!(app.entries[0], ConversationEntry::User { .. }));
        assert!(matches!(
            app.entries[1],
            ConversationEntry::Assistant { .. }
        ));
    }
}
