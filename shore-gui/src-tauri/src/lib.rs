use serde::Serialize;
use shore_client::{spawn_connection, ConnCommand, ConnEvent};
use shore_protocol::client_msg::{Cancel, ClientMessage, ClientMessageBody};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

const CLIENT_TYPE: &str = "gui";
const CLIENT_NAME: &str = "shore-gui";

struct AppState {
    connection: Mutex<Option<mpsc::Sender<ConnCommand>>>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConnectionStatus {
    Connected {
        server_name: String,
        characters: Vec<shore_protocol::types::CharacterInfo>,
        selected_character: Option<String>,
        history: Vec<shore_protocol::types::Message>,
        config: serde_json::Value,
    },
    Disconnected {
        reason: String,
    },
}

#[tauri::command]
async fn connect(
    addr: Option<String>,
    character: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.connection.lock().await;
    if let Some(old_tx) = guard.take() {
        let _ = old_tx.send(ConnCommand::Shutdown).await;
    }

    let (cmd_tx, mut event_rx) =
        spawn_connection(addr, None, CLIENT_TYPE, CLIENT_NAME, character);
    *guard = Some(cmd_tx);
    drop(guard);

    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                ConnEvent::Connected {
                    server_name,
                    characters,
                    history,
                    config,
                    selected_character,
                } => {
                    debug!(%server_name, chars = characters.len(), history = history.len(), "connected");
                    emit(
                        &app,
                        "connection-status",
                        ConnectionStatus::Connected {
                            server_name,
                            characters,
                            selected_character,
                            history,
                            config,
                        },
                    );
                }
                ConnEvent::Message(msg) => {
                    emit(&app, "server-message", msg);
                }
                ConnEvent::Disconnected(reason) => {
                    debug!(%reason, "disconnected");
                    emit(
                        &app,
                        "connection-status",
                        ConnectionStatus::Disconnected { reason },
                    );
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn send_message(text: String, state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.connection.lock().await;
    let tx = guard.as_ref().ok_or("not connected")?;

    let msg = ClientMessage::Message(ClientMessageBody {
        rid: None,
        text,
        stream: true,
        images: vec![],
        image_data: vec![],
        absence_seconds: None,
        overrides: None,
    });

    tx.send(ConnCommand::Send(msg))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel(state: State<'_, AppState>) -> Result<(), String> {
    let guard = state.connection.lock().await;
    let tx = guard.as_ref().ok_or("not connected")?;
    tx.send(ConnCommand::Send(ClientMessage::Cancel(Cancel {})))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.connection.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(ConnCommand::Shutdown).await;
    }
    Ok(())
}

fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        warn!(%event, error = %e, "failed to emit Tauri event");
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,shore_gui_lib=debug")
                }),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            app.manage(AppState {
                connection: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            send_message,
            cancel,
            disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
