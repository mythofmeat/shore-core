use godot::prelude::*;
use shore_client::conn_manager::{ConnCommand, ConnEvent, spawn_connection};
use shore_protocol::client_msg::{Cancel, ClientMessage, ClientMessageBody, Command};
use shore_protocol::server_msg::ServerMessage;
use tokio::sync::mpsc;

/// Godot node that bridges shore-client (SWP) into the scene tree via signals.
///
/// Add this as a child node, call `connect_to_daemon()`, then react to signals.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct ShoreBridge {
    runtime: Option<tokio::runtime::Runtime>,
    cmd_tx: Option<mpsc::Sender<ConnCommand>>,
    event_rx: Option<mpsc::Receiver<ConnEvent>>,
    base: Base<Node>,
}

#[godot_api]
impl INode for ShoreBridge {
    fn init(base: Base<Node>) -> Self {
        Self {
            runtime: None,
            cmd_tx: None,
            event_rx: None,
            base,
        }
    }

    fn process(&mut self, _delta: f64) {
        // Drain events into a local vec to avoid borrow conflict with self
        let events: Vec<ConnEvent> = {
            let Some(rx) = &mut self.event_rx else { return };
            let mut buf = Vec::new();
            while let Ok(event) = rx.try_recv() {
                buf.push(event);
            }
            buf
        };

        for event in events {
            match event {
                ConnEvent::Connected {
                    server_name,
                    characters,
                    history,
                    config,
                } => {
                    let char_names: PackedStringArray = characters
                        .iter()
                        .map(|c| GString::from(&c.name))
                        .collect();

                    let history_json =
                        serde_json::to_string(&history).unwrap_or_else(|_| "[]".into());
                    let config_json =
                        serde_json::to_string(&config).unwrap_or_else(|_| "{}".into());

                    self.base_mut().emit_signal(
                        "connected",
                        &[
                            GString::from(&server_name).to_variant(),
                            char_names.to_variant(),
                            GString::from(&*history_json).to_variant(),
                            GString::from(&*config_json).to_variant(),
                        ],
                    );
                }
                ConnEvent::Message(msg) => self.dispatch_server_message(msg),
                ConnEvent::Disconnected(reason) => {
                    self.base_mut()
                        .emit_signal("disconnected", &[GString::from(&reason).to_variant()]);
                }
            }
        }
    }

    fn exit_tree(&mut self) {
        // Send shutdown, then drop everything
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.try_send(ConnCommand::Shutdown);
        }
        self.event_rx.take();
        self.runtime.take();
    }
}

#[godot_api]
impl ShoreBridge {
    // ── Signals ──────────────────────────────────────────────────────

    #[signal]
    fn connected(server_name: GString, characters: Array<GString>, history_json: GString, config_json: GString);

    #[signal]
    fn disconnected(reason: GString);

    #[signal]
    fn stream_start(is_regen: bool);

    #[signal]
    fn stream_chunk(text: GString, content_type: GString);

    #[signal]
    fn stream_end(content: GString, metadata_json: GString);

    #[signal]
    fn message_received(message_json: GString);

    #[signal]
    fn tool_call(tool_id: GString, tool_name: GString, input_json: GString);

    #[signal]
    fn tool_result(tool_id: GString, tool_name: GString, output: GString, is_error: bool);

    #[signal]
    fn phase_changed(phase: GString, model: GString);

    #[signal]
    fn error_received(message: GString);

    #[signal]
    fn command_output(name: GString, data_json: GString);

    // ── Methods ──────────────────────────────────────────────────────

    #[func]
    fn connect_to_daemon(&mut self, socket: GString, character: GString) {
        // Tear down previous connection if any
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.try_send(ConnCommand::Shutdown);
        }
        self.event_rx.take();
        self.runtime.take();

        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        let _guard = rt.enter();

        let socket_opt = if socket.is_empty() {
            None
        } else {
            Some(socket.to_string())
        };
        let char_opt = if character.is_empty() {
            None
        } else {
            Some(character.to_string())
        };

        let (cmd_tx, event_rx) =
            spawn_connection(socket_opt, None, "bridge", "shore-gui", char_opt);

        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);
        self.runtime = Some(rt);
    }

    #[func]
    fn send_message(&self, text: GString) {
        let Some(tx) = &self.cmd_tx else { return };
        let msg = ClientMessage::Message(ClientMessageBody {
            rid: None,
            forensic_character: None,
            text: text.to_string(),
            stream: true,
            images: vec![],
            image_data: vec![],
            absence_seconds: None,
            overrides: None,
        });
        let _ = tx.try_send(ConnCommand::Send(msg));
    }

    #[func]
    fn send_command(&self, name: GString, args_json: GString) {
        let Some(tx) = &self.cmd_tx else { return };
        let args: serde_json::Value = serde_json::from_str(&args_json.to_string())
            .unwrap_or(serde_json::json!({}));
        let msg = ClientMessage::Command(Command {
            rid: None,
            forensic_character: None,
            name: name.to_string(),
            args,
        });
        let _ = tx.try_send(ConnCommand::Send(msg));
    }

    #[func]
    fn cancel_generation(&self) {
        let Some(tx) = &self.cmd_tx else { return };
        let _ = tx.try_send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})));
    }

    #[func]
    fn is_connected(&self) -> bool {
        self.cmd_tx.is_some()
    }
}

impl ShoreBridge {
    fn dispatch_server_message(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::StreamStart(s) => {
                self.base_mut()
                    .emit_signal("stream_start", &[s.regen.to_variant()]);
            }
            ServerMessage::StreamChunk(c) => {
                self.base_mut().emit_signal(
                    "stream_chunk",
                    &[c.text.to_variant(), c.content_type.to_variant()],
                );
            }
            ServerMessage::StreamEnd(e) => {
                let meta_json =
                    serde_json::to_string(&e.metadata).unwrap_or_else(|_| "{}".into());
                self.base_mut().emit_signal(
                    "stream_end",
                    &[e.content.to_variant(), meta_json.to_variant()],
                );
            }
            ServerMessage::Error(e) => {
                self.base_mut()
                    .emit_signal("error_received", &[e.message.to_variant()]);
            }
            ServerMessage::Phase(p) => {
                let model = p.model.unwrap_or_default();
                self.base_mut().emit_signal(
                    "phase_changed",
                    &[p.phase.to_variant(), model.to_variant()],
                );
            }
            ServerMessage::ToolCall(t) => {
                let input_json =
                    serde_json::to_string(&t.input).unwrap_or_else(|_| "{}".into());
                self.base_mut().emit_signal(
                    "tool_call",
                    &[
                        t.tool_id.to_variant(),
                        t.tool_name.to_variant(),
                        input_json.to_variant(),
                    ],
                );
            }
            ServerMessage::ToolResult(t) => {
                self.base_mut().emit_signal(
                    "tool_result",
                    &[
                        t.tool_id.to_variant(),
                        t.tool_name.to_variant(),
                        t.output.to_variant(),
                        t.is_error.to_variant(),
                    ],
                );
            }
            ServerMessage::NewMessage(m) => {
                let json =
                    serde_json::to_string(&m.message).unwrap_or_else(|_| "{}".into());
                self.base_mut()
                    .emit_signal("message_received", &[json.to_variant()]);
            }
            ServerMessage::CommandOutput(co) => {
                let data_json =
                    serde_json::to_string(&co.data).unwrap_or_else(|_| "{}".into());
                self.base_mut().emit_signal(
                    "command_output",
                    &[
                        GString::from(&co.name).to_variant(),
                        GString::from(&*data_json).to_variant(),
                    ],
                );
            }
            // Pings and other messages are handled by conn_manager or ignored
            _ => {}
        }
    }
}
