use std::sync::Arc;

use serde_json::json;
use shore_config::LoadedConfig;
use shore_protocol::server_msg::History;
use shore_swp_server::{HandshakeProvider, HelloSnapshot, HistorySnapshot};
use tokio::sync::Mutex;

use crate::characters::CharacterRegistry;
use crate::commands::navigation::character_metadata;
use crate::preferences;
use crate::runtime_state::load_active_model;

pub fn build_handshake_provider(registry: Arc<Mutex<CharacterRegistry>>) -> HandshakeProvider {
    HandshakeProvider {
        hello: {
            let registry = registry.clone();
            Arc::new(move || {
                let registry = registry.clone();
                Box::pin(async move {
                    let registry = registry.lock().await;
                    let config_dir = registry.global_config().dirs.config.clone();
                    HelloSnapshot {
                        characters: registry
                            .available_characters()
                            .iter()
                            .map(|name| character_metadata(&config_dir, name))
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
        let effective_config = if let Some(name) = selected_character.as_deref() {
            registry.effective_config(name).clone()
        } else {
            registry.global_config().clone()
        };
        let active_model = resolve_snapshot_active_model(
            &effective_config,
            selected_character.as_deref(),
            active_model,
        );
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
                active_start,
                config: _,
                selected_character,
                revision,
                ..
            } = engine.history_snapshot(serde_json::json!({}));
            HistorySnapshot {
                messages,
                active_start,
                config,
                selected_character,
                revision,
            }
        }
        None => HistorySnapshot {
            messages: Vec::new(),
            active_start: 0,
            config,
            selected_character: None,
            revision: 0,
        },
    }
}

fn history_config_snapshot(
    config: &LoadedConfig,
    active_model: Option<String>,
) -> serde_json::Value {
    json!({
        "active_model": active_model
            .or_else(|| config.app.defaults.model.clone())
            .or_else(|| config.models.first_chat_model().map(|m| m.qualified_name.clone())),
        "private": false,
    })
}

fn resolve_snapshot_active_model(
    config: &LoadedConfig,
    selected_character: Option<&str>,
    active_model: Option<String>,
) -> Option<String> {
    if active_model.is_some() {
        return active_model;
    }

    let character = selected_character?;

    let (global_prefs, char_prefs) = preferences::load_for_character(&config.dirs.data, character)
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                character,
                "Failed to load preferences for handshake snapshot; using defaults"
            );
            (
                preferences::ModelPreferences::default(),
                preferences::ModelPreferences::default(),
            )
        });
    let legacy = load_active_model(&config.dirs.data.join(character));

    preferences::resolve_active_for_character(
        config,
        &config.dirs.data,
        &global_prefs,
        &char_prefs,
        legacy.as_deref(),
        config.app.defaults.model.as_deref(),
    )
    .map(|m| m.qualified_name)
}
