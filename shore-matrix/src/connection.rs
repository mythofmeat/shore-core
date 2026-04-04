//! Re-exports the shared connection manager from shore-client.

pub use shore_client::conn_manager::{ConnCommand, ConnEvent};

/// Spawn the Matrix bridge connection manager.
pub fn spawn_connection(
    socket: Option<String>,
    config: Option<String>,
) -> (
    tokio::sync::mpsc::Sender<ConnCommand>,
    tokio::sync::mpsc::Receiver<ConnEvent>,
) {
    shore_client::spawn_connection(socket, config, "bridge", "shore-matrix", None)
}
