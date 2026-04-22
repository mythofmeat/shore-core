mod config;
mod memory;
mod models;
mod status;

pub use config::{config, config_check, config_reset};
pub use memory::{
    compact, memory, memory_changelog, memory_migrate, memory_purge, memory_reindex,
    memory_shell_end, memory_shell_query, memory_shell_start, resolve_compaction_model,
};
pub use models::{list_models, model_info, reset_model, set_reasoning_effort, switch_model};
pub use status::{
    diagnostics, heartbeat_log, interiority_set_active, interiority_set_dormant,
    interiority_tick_now, status,
};

#[cfg(test)]
mod tests;
