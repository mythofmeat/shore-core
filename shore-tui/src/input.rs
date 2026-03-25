use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use shore_protocol::client_msg::{ClientMessage, ClientMessageBody, Regen};

use crate::app::{App, InputMode};
use crate::connection::ConnCommand;

/// Action resulting from a key press.
pub enum Action {
    None,
    Send(ConnCommand),
    Quit,
    Redraw,
}

/// Handle a crossterm input event and return the resulting action.
pub fn handle_event(app: &mut App, event: Event) -> Action {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Resize(_, _) => Action::Redraw,
        _ => Action::None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // Global shortcuts (work in any mode)
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => return Action::Quit,
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => return Action::Quit,
        _ => {}
    }

    match app.input.mode {
        InputMode::Normal => handle_normal_mode(app, key),
        InputMode::Insert => handle_insert_mode(app, key),
    }
}

fn handle_normal_mode(app: &mut App, key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Enter insert mode
        (KeyModifiers::NONE, KeyCode::Char('i')) => {
            app.input.mode = InputMode::Insert;
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('a')) => {
            app.input.move_right();
            app.input.mode = InputMode::Insert;
            Action::Redraw
        }
        (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
            app.input.move_end();
            app.input.mode = InputMode::Insert;
            Action::Redraw
        }
        (KeyModifiers::SHIFT, KeyCode::Char('I')) => {
            app.input.move_home();
            app.input.mode = InputMode::Insert;
            Action::Redraw
        }

        // Navigation
        (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left) => {
            app.input.move_left();
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('l') | KeyCode::Right) => {
            app.input.move_right();
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('0') | KeyCode::Home) => {
            app.input.move_home();
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('$') | KeyCode::End) => {
            app.input.move_end();
            Action::Redraw
        }

        // Scroll conversation
        (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
            app.scroll_up(1);
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
            app.scroll_down(1);
            Action::Redraw
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            app.scroll_up(10);
            Action::Redraw
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            app.scroll_down(10);
            Action::Redraw
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
            app.scroll_to_bottom();
            Action::Redraw
        }

        // Toggle thinking panel
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.stream.thinking_collapsed = !app.stream.thinking_collapsed;
            Action::Redraw
        }

        // Regen
        (KeyModifiers::NONE, KeyCode::Char('r')) => {
            let msg = ClientMessage::Regen(Regen {
                rid: None,
                stream: true,
                guidance: None,
            });
            Action::Send(ConnCommand::Send(msg))
        }

        _ => Action::None,
    }
}

fn handle_insert_mode(app: &mut App, key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Exit insert mode
        (KeyModifiers::NONE, KeyCode::Esc) => {
            app.input.mode = InputMode::Normal;
            Action::Redraw
        }

        // Send message: Enter (without Shift)
        (KeyModifiers::NONE, KeyCode::Enter) => {
            let text = app.input.take_text();
            if text.trim().is_empty() {
                return Action::None;
            }
            app.scroll_to_bottom();
            let msg = ClientMessage::Message(ClientMessageBody {
                rid: None,
                text,
                stream: true,
                images: vec![],
                absence_seconds: None,
            });
            Action::Send(ConnCommand::Send(msg))
        }

        // Newline: Shift+Enter or Alt+Enter
        (KeyModifiers::SHIFT, KeyCode::Enter)
        | (KeyModifiers::ALT, KeyCode::Enter) => {
            app.input.insert_newline();
            Action::Redraw
        }

        // Backspace
        (_, KeyCode::Backspace) => {
            app.input.backspace();
            Action::Redraw
        }

        // Delete
        (_, KeyCode::Delete) => {
            app.input.delete();
            Action::Redraw
        }

        // Navigation
        (_, KeyCode::Left) => {
            app.input.move_left();
            Action::Redraw
        }
        (_, KeyCode::Right) => {
            app.input.move_right();
            Action::Redraw
        }
        (_, KeyCode::Home) | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            app.input.move_home();
            Action::Redraw
        }
        (_, KeyCode::End) | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            app.input.move_end();
            Action::Redraw
        }

        // Scroll conversation from insert mode
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            app.scroll_up(10);
            Action::Redraw
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            app.scroll_down(10);
            Action::Redraw
        }

        // Toggle thinking panel
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.stream.thinking_collapsed = !app.stream.thinking_collapsed;
            Action::Redraw
        }

        // Regular character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            app.input.insert_char(c);
            Action::Redraw
        }

        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn make_key(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::default();
        let action = handle_key(&mut app, make_key(KeyModifiers::CONTROL, KeyCode::Char('c')));
        assert!(matches!(action, Action::Quit));
    }

    #[test]
    fn insert_mode_enter_sends() {
        let mut app = App::default();
        app.input.mode = InputMode::Insert;
        for c in "hello".chars() {
            app.input.insert_char(c);
        }
        let action = handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Enter));
        assert!(matches!(action, Action::Send(_)));
    }

    #[test]
    fn insert_mode_empty_enter_is_noop() {
        let mut app = App::default();
        app.input.mode = InputMode::Insert;
        let action = handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Enter));
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn normal_mode_i_enters_insert() {
        let mut app = App::default();
        app.input.mode = InputMode::Normal;
        let action = handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Char('i')));
        assert!(matches!(action, Action::Redraw));
        assert_eq!(app.input.mode, InputMode::Insert);
    }

    #[test]
    fn esc_returns_to_normal() {
        let mut app = App::default();
        app.input.mode = InputMode::Insert;
        let action = handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Esc));
        assert!(matches!(action, Action::Redraw));
        assert_eq!(app.input.mode, InputMode::Normal);
    }

    #[test]
    fn normal_mode_r_regens() {
        let mut app = App::default();
        app.input.mode = InputMode::Normal;
        let action = handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Char('r')));
        assert!(matches!(action, Action::Send(_)));
    }

    #[test]
    fn scroll_shortcuts() {
        let mut app = App::default();
        app.input.mode = InputMode::Normal;
        handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Char('k')));
        assert_eq!(app.scroll_offset, 1);
        handle_key(&mut app, make_key(KeyModifiers::NONE, KeyCode::Char('j')));
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut app = App::default();
        app.input.mode = InputMode::Insert;
        for c in "line1".chars() {
            app.input.insert_char(c);
        }
        handle_key(&mut app, make_key(KeyModifiers::SHIFT, KeyCode::Enter));
        assert!(app.input.text.contains('\n'));
    }
}
