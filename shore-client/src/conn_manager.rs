//! Shared connection manager for SWP clients.
//!
//! Provides a `spawn_connection()` function that spawns a background task
//! managing the daemon connection with automatic reconnect and exponential
//! backoff. Used by shore-tui and shore-matrix.

use crate::{discover_or_default, SWPConnection, ServerAddr};
use shore_protocol::client_msg::ClientMessage;
use shore_protocol::server_msg::ServerMessage;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

/// Events sent from the connection task to the application loop.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ConnEvent {
    Connected {
        server_name: String,
        characters: Vec<shore_protocol::types::CharacterInfo>,
        history: Vec<shore_protocol::types::Message>,
        config: serde_json::Value,
    },
    Message(ServerMessage),
    Disconnected(String),
}

/// Commands sent from the application loop to the connection task.
#[derive(Debug)]
pub enum ConnCommand {
    Send(ClientMessage),
    Shutdown,
}

/// Spawn a connection manager task.
///
/// Returns channels for bidirectional communication. The task automatically
/// reconnects on disconnection with exponential backoff (500ms → 15s).
///
/// - `socket`: optional explicit socket path or address
/// - `config`: optional config path for service discovery
/// - `client_id`: protocol-level client type (e.g. `"tui"`, `"bridge"`)
/// - `app_name`: human-readable application name (e.g. `"shore-tui"`, `"shore-matrix"`)
/// - `character`: optional character to select on connect
pub fn spawn_connection(
    socket: Option<String>,
    config: Option<String>,
    client_id: &str,
    app_name: &str,
    character: Option<String>,
) -> (mpsc::Sender<ConnCommand>, mpsc::Receiver<ConnEvent>) {
    let (event_tx, event_rx) = mpsc::channel(256);
    let (cmd_tx, cmd_rx) = mpsc::channel(64);

    let client_id = client_id.to_string();
    let app_name = app_name.to_string();

    tokio::spawn(connection_loop(
        socket, config, client_id, app_name, character, event_tx, cmd_rx,
    ));

    (cmd_tx, event_rx)
}

fn resolve_addr(socket: &Option<String>, config: &Option<String>) -> ServerAddr {
    if let Some(socket) = socket {
        if crate::connection::is_unix_path(socket) {
            return ServerAddr::Unix(socket.clone());
        }
        return ServerAddr::Tcp(socket.clone());
    }
    discover_or_default(config.as_deref())
}

async fn connection_loop(
    socket: Option<String>,
    config: Option<String>,
    client_id: String,
    app_name: String,
    character: Option<String>,
    event_tx: mpsc::Sender<ConnEvent>,
    mut cmd_rx: mpsc::Receiver<ConnCommand>,
) {
    let mut backoff = Duration::from_millis(500);
    let max_backoff = Duration::from_secs(15);

    loop {
        let addr = resolve_addr(&socket, &config);

        match SWPConnection::connect(&addr, &client_id, &app_name, character.clone()).await {
            Ok((mut conn, hello, history)) => {
                backoff = Duration::from_millis(500);

                let _ = event_tx
                    .send(ConnEvent::Connected {
                        server_name: hello.server_name,
                        characters: hello.characters,
                        history: history.messages,
                        config: history.config,
                    })
                    .await;

                // Main receive/send loop
                loop {
                    tokio::select! {
                        biased;
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                Some(ConnCommand::Send(msg)) => {
                                    if conn.send(&msg).await.is_err() {
                                        let _ = event_tx.send(ConnEvent::Disconnected(
                                            "send failed".into()
                                        )).await;
                                        break;
                                    }
                                }
                                Some(ConnCommand::Shutdown) | None => return,
                            }
                        }
                        msg = conn.recv() => {
                            match msg {
                                Ok(ServerMessage::Shutdown(_)) => {
                                    let _ = event_tx.send(ConnEvent::Disconnected(
                                        "server shutdown".into()
                                    )).await;
                                    break;
                                }
                                Ok(ServerMessage::Ping(_)) => {
                                    // Keepalive — ignore
                                }
                                Ok(msg) => {
                                    if event_tx.send(ConnEvent::Message(msg)).await.is_err() {
                                        return; // Application dropped receiver
                                    }
                                }
                                Err(_) => {
                                    let _ = event_tx.send(ConnEvent::Disconnected(
                                        "connection lost".into()
                                    )).await;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let _ = event_tx
                    .send(ConnEvent::Disconnected(format!("connect failed: {e}")))
                    .await;
            }
        }

        // Exponential backoff before reconnect
        sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}
