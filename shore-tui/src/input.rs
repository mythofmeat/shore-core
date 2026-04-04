use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use shore_protocol::client_msg::{Cancel, ClientMessage, ClientMessageBody, Command, Regen};

use crate::app::{App, InputMode};
use crate::connection::ConnCommand;

/// Action resulting from a key press.
pub enum Action {
    None,
    Send(ConnCommand),
    /// Send multiple commands at once.
    SendMulti(Vec<ConnCommand>),
    Quit,
    Redraw,
    OpenInEditor,
    /// Open external file picker to select an image.
    PickImage(Option<String>),
}

/// Handle a crossterm input event and return the resulting action.
pub fn handle_event(app: &mut App, event: Event) -> Action {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Paste(text) => handle_paste(app, text),
        Event::Resize(_, _) => Action::Redraw,
        _ => Action::None,
    }
}

/// Handle a bracketed paste event — insert all text without triggering send.
fn handle_paste(app: &mut App, text: String) -> Action {
    if app.input.mode != InputMode::Insert {
        app.input.mode = InputMode::Insert;
    }
    app.input.insert_str(&text);
    Action::Redraw
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // Close help overlay on any keypress
    if app.show_help {
        app.show_help = false;
        return Action::Redraw;
    }

    // Global shortcuts (work in any mode)
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if app.stream.active {
                app.stream.reset();
                return Action::Send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})));
            }
            return Action::Quit;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => return Action::Quit,
        _ => {}
    }

    match app.input.mode {
        InputMode::Normal => handle_normal_mode(app, key),
        InputMode::Insert => handle_insert_mode(app, key),
        InputMode::Command => handle_command_mode(app, key),
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
        (KeyModifiers::NONE, KeyCode::Char('u')) => {
            app.scroll_up(10);
            Action::Redraw
        }
        (KeyModifiers::NONE, KeyCode::Char('d')) => {
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

        // Toggle thinking blocks in history
        (KeyModifiers::NONE, KeyCode::Char('t')) => {
            app.show_thinking = !app.show_thinking;
            Action::Redraw
        }

        // Toggle tool-use blocks in history
        (KeyModifiers::SHIFT, KeyCode::Char('T')) => {
            app.show_tools = !app.show_tools;
            Action::Redraw
        }

        // Toggle inline images in history
        (KeyModifiers::NONE, KeyCode::Char('p')) => {
            app.show_images = !app.show_images;
            Action::Redraw
        }

        // Open input in $EDITOR
        (KeyModifiers::CONTROL, KeyCode::Char('g')) => Action::OpenInEditor,

        // Regen
        (KeyModifiers::NONE, KeyCode::Char('r')) => {
            let msg = ClientMessage::Regen(Regen {
                rid: None,
                stream: true,
                guidance: None,
            });
            Action::Send(ConnCommand::Send(msg))
        }

        // Command palette
        (KeyModifiers::SHIFT, KeyCode::Char(':')) | (KeyModifiers::NONE, KeyCode::Char(':')) => {
            app.input.enter_command_mode();
            app.update_completions();
            Action::Redraw
        }

        _ => Action::None,
    }
}

fn handle_insert_mode(app: &mut App, key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Exit insert mode (cancel edit or generation if active)
        (KeyModifiers::NONE, KeyCode::Esc) => {
            app.input.mode = InputMode::Normal;
            if app.editing_ref.take().is_some() {
                app.input.text.clear();
                app.input.cursor = 0;
                app.set_status("edit cancelled");
            } else if app.stream.active {
                app.stream.reset();
                return Action::Send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})));
            }
            Action::Redraw
        }

        // Send message (or submit edit): Enter (without Shift)
        (KeyModifiers::NONE, KeyCode::Enter) => {
            let text = app.input.take_text();
            if text.trim().is_empty() && app.pending_images.is_empty() {
                return Action::None;
            }

            // If editing, send an edit command instead of a new message.
            if let Some(edit_ref) = app.editing_ref.take() {
                app.set_status(format!("edited message ({edit_ref})"));
                return Action::SendMulti(vec![
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "edit".into(),
                        args: serde_json::json!({ "ref": edit_ref, "content": text }),
                    })),
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "log".into(),
                        args: serde_json::json!({}),
                    })),
                ]);
            }

            let images = std::mem::take(&mut app.pending_images);
            // Read and base64-encode images for wire transfer.
            let mut image_uploads: Vec<shore_protocol::client_msg::ImageUpload> = Vec::new();
            let mut image_refs: Vec<shore_protocol::types::ImageRef> = Vec::new();
            for p in &images {
                match std::fs::read(p) {
                    Ok(bytes) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let filename = std::path::Path::new(p)
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| "image".to_string());
                        image_refs.push(shore_protocol::types::ImageRef {
                            path: p.clone(),
                            caption: None,
                            data: Some(b64.clone()),
                        });
                        image_uploads.push(shore_protocol::client_msg::ImageUpload {
                            filename,
                            data: b64,
                        });
                    }
                    Err(e) => {
                        app.set_status(format!("failed to read image: {e}"));
                    }
                }
            }
            // Optimistic: show user's message in conversation immediately
            app.entries.push(crate::app::ConversationEntry::User {
                content: text.clone(),
                images: image_refs,
                timestamp: String::new(),
            });
            app.scroll_to_bottom();
            // Show typing indicator immediately (don't wait for StreamStart)
            app.stream.active = true;
            let msg = ClientMessage::Message(ClientMessageBody {
                rid: None,
                text,
                stream: true,
                images,
                image_data: image_uploads,
                absence_seconds: None,
                overrides: None,
            });
            Action::Send(ConnCommand::Send(msg))
        }

        // Newline: Shift+Enter or Alt+Enter
        (KeyModifiers::SHIFT, KeyCode::Enter) | (KeyModifiers::ALT, KeyCode::Enter) => {
            app.input.insert_newline();
            Action::Redraw
        }

        // Word deletion
        (KeyModifiers::ALT, KeyCode::Backspace) => {
            app.input.backspace_word();
            Action::Redraw
        }
        (KeyModifiers::ALT, KeyCode::Delete) => {
            app.input.delete_word();
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

        // Open input in $EDITOR
        (KeyModifiers::CONTROL, KeyCode::Char('g')) => Action::OpenInEditor,

        // Regular character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            app.input.insert_char(c);
            Action::Redraw
        }

        _ => Action::None,
    }
}

fn handle_command_mode(app: &mut App, key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Cancel
        (KeyModifiers::NONE, KeyCode::Esc) => {
            app.input.exit_command_mode();
            app.completion.candidates.clear();
            app.completion.selected = None;
            Action::Redraw
        }

        // Tab — cycle completions
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.next_completion();
            Action::Redraw
        }

        // Execute command
        (KeyModifiers::NONE, KeyCode::Enter) => {
            app.completion.candidates.clear();
            app.completion.selected = None;
            let text = app.input.take_cmd_text();
            parse_command(app, &text)
        }

        // Backspace — if empty, cancel
        (_, KeyCode::Backspace) => {
            if app.input.cmd_text.is_empty() {
                app.input.exit_command_mode();
                app.completion.candidates.clear();
                app.completion.selected = None;
            } else {
                app.input.cmd_backspace();
                app.update_completions();
            }
            Action::Redraw
        }

        // Character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            app.input.cmd_insert_char(c);
            app.update_completions();
            Action::Redraw
        }

        _ => Action::None,
    }
}

/// Parse a command string and return the appropriate action.
fn parse_command(app: &mut App, input: &str) -> Action {
    let input = input.trim();
    if input.is_empty() {
        return Action::Redraw;
    }

    let mut parts = input.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "q" | "quit" => {
            app.should_quit = true;
            Action::Quit
        }

        "help" => {
            app.show_help = true;
            Action::Redraw
        }

        "character" => {
            if arg.is_empty() {
                // List characters
                Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "list_characters".into(),
                    args: serde_json::json!({}),
                })))
            } else {
                // Switch character, then re-fetch log and status
                Action::SendMulti(vec![
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "switch_character".into(),
                        args: serde_json::json!({ "name": arg }),
                    })),
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "log".into(),
                        args: serde_json::json!({}),
                    })),
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "status".into(),
                        args: serde_json::json!({}),
                    })),
                ])
            }
        }

        "model" => {
            if arg.is_empty() {
                app.show_model_list = true;
                Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "list_models".into(),
                    args: serde_json::json!({}),
                })))
            } else if arg == "reset" {
                Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "reset_model".into(),
                    args: serde_json::json!({}),
                })))
            } else {
                Action::SendMulti(vec![
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "switch_model".into(),
                        args: serde_json::json!({ "name": arg }),
                    })),
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "status".into(),
                        args: serde_json::json!({}),
                    })),
                ])
            }
        }

        "status" => Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
            rid: None,
            name: "status".into(),
            args: serde_json::json!({}),
        }))),

        "memory" => {
            if arg.is_empty() {
                app.set_status("usage: :memory <query>");
                Action::Redraw
            } else {
                Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "memory".into(),
                    args: serde_json::json!({ "query": arg }),
                })))
            }
        }

        "compact" => Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
            rid: None,
            name: "compact".into(),
            args: serde_json::json!({}),
        }))),

        "delete" => {
            if arg.is_empty() {
                app.set_status("usage: :delete <ref>  (e.g. last, -1, -2)");
                Action::Redraw
            } else {
                // Support space-separated refs or a single ref
                let refs: Vec<&str> = arg.split_whitespace().collect();
                let args = if refs.len() == 1 {
                    serde_json::json!({ "refs": refs[0] })
                } else {
                    serde_json::json!({ "refs": refs })
                };
                Action::SendMulti(vec![
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "delete".into(),
                        args,
                    })),
                    ConnCommand::Send(ClientMessage::Command(Command {
                        rid: None,
                        name: "log".into(),
                        args: serde_json::json!({}),
                    })),
                ])
            }
        }

        "edit" => {
            if arg.is_empty() || arg == "cancel" {
                if app.editing_ref.is_some() {
                    app.editing_ref = None;
                    app.input.text.clear();
                    app.input.cursor = 0;
                    app.set_status("edit cancelled");
                } else {
                    app.set_status("usage: :edit <ref>  (e.g. last, -1, -2)");
                }
                Action::Redraw
            } else {
                match app.resolve_ref_content(arg) {
                    Some(content) => {
                        app.editing_ref = Some(arg.to_string());
                        app.input.set_text(content);
                        app.input.mode = InputMode::Insert;
                        Action::Redraw
                    }
                    None => {
                        app.set_status(format!("message not found: {arg}"));
                        Action::Redraw
                    }
                }
            }
        }

        "regen" => {
            let msg = ClientMessage::Regen(Regen {
                rid: None,
                stream: true,
                guidance: if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_string())
                },
            });
            Action::Send(ConnCommand::Send(msg))
        }

        "image" => {
            if arg == "clear" {
                let count = app.pending_images.len();
                app.pending_images.clear();
                app.set_status(format!("cleared {count} pending image(s)"));
                Action::Redraw
            } else if arg.is_empty() {
                // Open external file picker
                Action::PickImage(None)
            } else {
                // Direct path — resolve tilde and relative paths
                let expanded = if arg.starts_with('~') {
                    if let Ok(home) = std::env::var("HOME") {
                        arg.replacen('~', &home, 1)
                    } else {
                        arg.to_string()
                    }
                } else {
                    arg.to_string()
                };
                let path = if std::path::Path::new(&expanded).is_absolute() {
                    expanded
                } else {
                    std::env::current_dir()
                        .map(|d| d.join(&expanded).to_string_lossy().to_string())
                        .unwrap_or(expanded)
                };
                if !std::path::Path::new(&path).exists() {
                    app.set_status(format!("file not found: {path}"));
                    Action::Redraw
                } else {
                    app.pending_images.push(path.clone());
                    app.set_status(format!(
                        "attached image ({} pending)",
                        app.pending_images.len()
                    ));
                    Action::Redraw
                }
            }
        }

        "sys" | "system" => {
            if arg.is_empty() {
                app.set_status("usage: :sys <instruction>");
                Action::Redraw
            } else {
                Action::Send(ConnCommand::Send(ClientMessage::Command(Command {
                    rid: None,
                    name: "inject_system".into(),
                    args: serde_json::json!({ "text": arg }),
                })))
            }
        }

        _ => {
            app.set_status(format!("unknown command: {cmd}"));
            Action::Redraw
        }
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
        let action = handle_key(
            &mut app,
            make_key(KeyModifiers::CONTROL, KeyCode::Char('c')),
        );
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
