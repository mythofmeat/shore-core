//! CharacterRegistry — multi-character engine management.
//!
//! The daemon discovers available characters from the config directory
//! (`characters/<name>/character.md`) and lazily creates ConversationEngine
//! instances per character on first request.

use std::collections::HashMap;
use std::path::PathBuf;

use shore_protocol::server_msg::ServerMessage;
use tokio::sync::broadcast;
use tracing::info;

use crate::config::{discover_characters, load_character_definition, resolve_user_definition};
use crate::engine::{ConversationEngine, EngineError};

/// Manages multiple character engines with lazy initialization.
pub struct CharacterRegistry {
    /// Config directory for discovering character definitions.
    config_dir: PathBuf,
    /// Data directory root (`$XDG_DATA_HOME/shore/`).
    data_dir: PathBuf,
    /// Broadcast sender passed to new engines.
    push_tx: broadcast::Sender<ServerMessage>,
    /// Lazily-created engines keyed by character name.
    engines: HashMap<String, ConversationEngine>,
    /// Cached list of available character names (from filesystem scan).
    available: Vec<String>,
}

impl CharacterRegistry {
    /// Create a new registry by scanning the config directory for characters.
    pub fn new(
        config_dir: PathBuf,
        data_dir: PathBuf,
        push_tx: broadcast::Sender<ServerMessage>,
    ) -> Self {
        let available = discover_characters(&config_dir);
        info!(
            characters = ?available,
            "Discovered {} character(s)",
            available.len()
        );

        Self {
            config_dir,
            data_dir,
            push_tx,
            engines: HashMap::new(),
            available,
        }
    }

    /// List the names of all available characters.
    pub fn available_characters(&self) -> &[String] {
        &self.available
    }

    /// Re-scan the characters directory for changes.
    pub fn refresh(&mut self) {
        self.available = discover_characters(&self.config_dir);
    }

    /// Check whether a character exists in the available list.
    pub fn has_character(&self, name: &str) -> bool {
        self.available.iter().any(|n| n == name)
    }

    /// Get the engine for a character, creating it lazily if needed.
    ///
    /// Returns an error if the character is not in the available list or if
    /// engine creation fails.
    pub fn get_or_create(
        &mut self,
        name: &str,
    ) -> Result<&mut ConversationEngine, EngineError> {
        if !self.has_character(name) {
            return Err(EngineError::CharacterNotFound(name.to_string()));
        }

        // Entry API can't be used directly with fallible init, so check + insert.
        if !self.engines.contains_key(name) {
            let engine = ConversationEngine::new(
                name.to_string(),
                self.data_dir.clone(),
                self.push_tx.clone(),
            )?;
            info!(character = name, "Created engine for character");
            self.engines.insert(name.to_string(), engine);
        }

        Ok(self.engines.get_mut(name).unwrap())
    }

    /// Load the character definition (system prompt) for a given character.
    pub fn character_definition(&self, name: &str) -> Option<String> {
        load_character_definition(&self.config_dir, name)
    }

    /// Load the user definition for a given character (character-specific → global fallback).
    pub fn user_definition(&self, name: &str) -> Option<String> {
        resolve_user_definition(&self.config_dir, name)
    }

    /// Select a character by name or auto-select if there's only one.
    ///
    /// - If `requested` is `Some`, validates it exists.
    /// - If `None` and exactly one character is available, auto-selects it.
    /// - Otherwise returns an error.
    pub fn resolve_character(
        &self,
        requested: Option<&str>,
    ) -> Result<String, CharacterError> {
        match requested {
            Some(name) => {
                if self.has_character(name) {
                    Ok(name.to_string())
                } else {
                    Err(CharacterError::NotFound {
                        name: name.to_string(),
                        available: self.available.clone(),
                    })
                }
            }
            None => match self.available.len() {
                0 => Err(CharacterError::NoneAvailable),
                1 => Ok(self.available[0].clone()),
                _ => Err(CharacterError::AmbiguousSelection {
                    available: self.available.clone(),
                }),
            },
        }
    }
}

/// Errors from character resolution.
#[derive(Debug, thiserror::Error)]
pub enum CharacterError {
    #[error("character \"{name}\" not found (available: {available:?})")]
    NotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("no characters available — create one at characters/<name>/character.md")]
    NoneAvailable,
    #[error("multiple characters available ({available:?}) — specify one with --character or SHORE_CHARACTER")]
    AmbiguousSelection { available: Vec<String> },
}
