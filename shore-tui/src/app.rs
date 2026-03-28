use shore_protocol::types::{
    CharacterInfo, ImageRef, StreamMetadata, TokenCounts,
};

/// A single entry in the conversation log.
#[derive(Clone, Debug)]
#[allow(dead_code)]
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
    ToolCall {
        tool_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
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
}

impl StreamState {
    pub fn reset(&mut self) {
        self.active = false;
        self.regen = false;
        self.text.clear();
        self.thinking.clear();
        self.thinking_collapsed = false;
        self.phase.clear();
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

    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
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

/// Connection status for the status bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
}

/// Main application state.
pub struct App {
    pub entries: Vec<ConversationEntry>,
    pub stream: StreamState,
    pub input: InputState,
    pub scroll_offset: u16,
    pub connection_status: ConnectionStatus,
    pub character_name: String,
    pub characters: Vec<CharacterInfo>,
    pub model: String,
    pub tokens: TokenCounts,
    pub is_private: bool,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub auto_scroll: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            stream: StreamState::default(),
            input: InputState::default(),
            scroll_offset: 0,
            connection_status: ConnectionStatus::Disconnected,
            character_name: String::new(),
            characters: Vec::new(),
            model: String::new(),
            tokens: TokenCounts {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            is_private: false,
            should_quit: false,
            status_message: None,
            auto_scroll: true,
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

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let total = self.tokens.input + self.tokens.cache_read;
        if total == 0 {
            return None;
        }
        Some(self.tokens.cache_read as f64 / total as f64)
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
    fn cache_hit_ratio() {
        let mut app = App::default();
        assert_eq!(app.cache_hit_ratio(), None);
        app.tokens.input = 100;
        app.tokens.cache_read = 400;
        let ratio = app.cache_hit_ratio().unwrap();
        assert!((ratio - 0.8).abs() < 0.001);
    }
}
