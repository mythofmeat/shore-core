#[cfg(test)]
pub mod test_support;

pub mod auto_discovery;
pub mod autonomy;
pub mod cache_keepalive;
pub mod characters;
pub mod commands;
pub mod content_util;
pub(crate) mod convert;
pub mod effective_catalog;
pub mod engine;
pub mod handler;
pub mod handshake;
pub mod hot_reload;
pub mod memory;
pub mod notifications;
pub mod preferences;
pub mod prompts;
pub mod runtime_state;
mod sync;
pub mod templates;
pub mod tools;
