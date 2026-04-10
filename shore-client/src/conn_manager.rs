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
use tracing::{debug, error, info, warn};

/// Events sent from the connection task to the application loop.
#[derive(Debug)]
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
/// - `addr`: optional explicit TCP address (`host:port`)
/// - `config`: optional config path for service discovery
/// - `client_id`: protocol-level client type (e.g. `"tui"`, `"bridge"`)
/// - `app_name`: human-readable application name (e.g. `"shore-tui"`, `"shore-matrix"`)
/// - `character`: optional character to select on connect
pub fn spawn_connection(
    addr: Option<String>,
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
        addr, config, client_id, app_name, character, event_tx, cmd_rx,
    ));

    (cmd_tx, event_rx)
}

/// Compute next backoff duration, doubling each time up to the cap.
fn next_backoff(current: Duration, max: Duration) -> Duration {
    (current * 2).min(max)
}

fn resolve_addr(addr: &Option<String>, config: &Option<String>) -> ServerAddr {
    if let Some(addr) = addr {
        return ServerAddr(addr.clone());
    }
    discover_or_default(config.as_deref())
}

async fn connection_loop(
    addr: Option<String>,
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
        let addr = resolve_addr(&addr, &config);
        info!(addr = ?addr, client = %app_name, "attempting connection");

        match SWPConnection::connect(&addr, &client_id, &app_name, character.clone()).await {
            Ok((mut conn, hello, history)) => {
                info!(
                    server = %hello.server_name,
                    characters = hello.characters.len(),
                    history_len = history.messages.len(),
                    "connected to daemon"
                );
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
                                    if let Err(e) = conn.send(&msg).await {
                                        error!(error = %e, "send failed, disconnecting");
                                        let _ = event_tx.send(ConnEvent::Disconnected(
                                            "send failed".into()
                                        )).await;
                                        break;
                                    }
                                }
                                Some(ConnCommand::Shutdown) => {
                                    info!("shutdown requested, closing connection");
                                    return;
                                }
                                None => {
                                    info!("command channel closed, exiting connection loop");
                                    return;
                                }
                            }
                        }
                        msg = conn.recv() => {
                            match msg {
                                Ok(ServerMessage::Shutdown(_)) => {
                                    info!("server sent shutdown");
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
                                        debug!("event receiver dropped, exiting connection loop");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "connection lost");
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
                warn!(error = %e, "connect failed");
                let _ = event_tx
                    .send(ConnEvent::Disconnected(format!("connect failed: {e}")))
                    .await;
            }
        }

        // Exponential backoff before reconnect
        info!(backoff_ms = backoff.as_millis(), "reconnecting after backoff");
        sleep(backoff).await;
        backoff = next_backoff(backoff, max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_backoff_doubles() {
        let max = Duration::from_secs(15);
        assert_eq!(
            next_backoff(Duration::from_millis(500), max),
            Duration::from_millis(1000)
        );
        assert_eq!(
            next_backoff(Duration::from_millis(1000), max),
            Duration::from_millis(2000)
        );
        assert_eq!(
            next_backoff(Duration::from_millis(2000), max),
            Duration::from_millis(4000)
        );
    }

    #[test]
    fn test_next_backoff_caps_at_max() {
        let max = Duration::from_secs(15);
        assert_eq!(next_backoff(Duration::from_secs(8), max), max);
        assert_eq!(next_backoff(Duration::from_secs(15), max), max);
        assert_eq!(next_backoff(Duration::from_secs(30), max), max);
    }

    #[test]
    fn test_next_backoff_full_sequence() {
        let max = Duration::from_secs(15);
        let mut b = Duration::from_millis(500);
        let expected = [1000, 2000, 4000, 8000, 15000, 15000];
        for &ms in &expected {
            b = next_backoff(b, max);
            assert_eq!(b, Duration::from_millis(ms));
        }
    }

    #[test]
    fn test_resolve_addr_explicit_tcp() {
        let addr = resolve_addr(&Some("127.0.0.1:9090".into()), &None);
        assert_eq!(addr.0, "127.0.0.1:9090");
    }
}
