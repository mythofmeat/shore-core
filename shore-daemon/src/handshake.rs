use std::sync::Arc;

use serde_json::json;
use shore_config::LoadedConfig;
use shore_daemon_server::{HandshakeProvider, HelloSnapshot, HistorySnapshot};
use shore_protocol::server_msg::History;
use shore_protocol::types::CharacterInfo;
use tokio::sync::Mutex;

use crate::characters::CharacterRegistry;

pub fn build_handshake_provider(registry: Arc<Mutex<CharacterRegistry>>) -> HandshakeProvider {
    HandshakeProvider {
        hello: {
            let registry = registry.clone();
            Arc::new(move || {
                let registry = registry.clone();
                Box::pin(async move {
                    let registry = registry.lock().await;
                    HelloSnapshot {
                        characters: registry
                            .available_characters()
                            .iter()
                            .map(|name| CharacterInfo { name: name.clone() })
                            .collect(),
                    }
                })
            })
        },
        history: {
            Arc::new(move |selected_character| {
                let registry = registry.clone();
                Box::pin(async move {
                    build_session_history_snapshot(registry, selected_character, None).await
                })
            })
        },
    }
}

pub async fn build_session_history_snapshot(
    registry: Arc<Mutex<CharacterRegistry>>,
    selected_character: Option<String>,
    active_model: Option<String>,
) -> HistorySnapshot {
    let (engine, config) = {
        let mut registry = registry.lock().await;
        let effective_config = selected_character
            .as_deref()
            .map(|name| registry.effective_config(name).clone())
            .unwrap_or_else(|| registry.global_config().clone());
        let engine = selected_character
            .as_deref()
            .and_then(|name| registry.get_or_create(name).ok());
        (
            engine,
            history_config_snapshot(&effective_config, active_model.clone()),
        )
    };

    match engine {
        Some(engine) => {
            let engine = engine.lock().await;
            let History {
                messages,
                config: _,
                selected_character,
            } = engine.history_snapshot(serde_json::json!({}));
            HistorySnapshot {
                messages,
                config,
                selected_character,
            }
        }
        None => HistorySnapshot {
            messages: Vec::new(),
            config,
            selected_character: None,
        },
    }
}

fn history_config_snapshot(
    config: &LoadedConfig,
    active_model: Option<String>,
) -> serde_json::Value {
    json!({
        "active_model": active_model.or_else(|| config.app.defaults.model.clone()),
        "private": false,
    })
}
