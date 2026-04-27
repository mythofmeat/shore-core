mod config;
mod memory;
mod models;
mod status;

pub use config::{config, config_check, config_reset};
pub use memory::{compact, memory, memory_changelog, memory_dream, resolve_compaction_model};
pub use models::{
    list_models, model_info, model_settings, reset_model, set_model_setting, set_reasoning_effort,
    switch_model,
};
pub use status::{
    diagnostics, heartbeat_log, heartbeat_set_active, heartbeat_set_dormant, heartbeat_tick_now,
    status,
};

#[cfg(test)]
mod tests;
