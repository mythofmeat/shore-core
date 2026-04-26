/// Context for gate decisions.
#[derive(Debug, Clone, Copy)]
pub struct GateContext {
    /// `true` if we are on an isolated test profile, `false` on main.
    pub profile_is_test: bool,
    /// `true` if `--allow-main-writes` was passed (only meaningful when
    /// `profile_is_test == false`).
    pub allow_main_writes: bool,
}

/// Result of a gate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Refuse(String),
}

/// Classification of a tool as read-only, mutating, or unknown.
enum ToolClass {
    ReadOnly,
    Mutating,
    Unknown,
}

fn classify(tool: &str) -> ToolClass {
    match tool {
        // ── read-only ──────────────────────────────────────────────
        "status" | "status_diagnostics" | "log_tail" | "log_show" | "log_heartbeat"
        | "log_follow" | "usage" | "config_get" | "config_check" | "config_path"
        | "character_list" | "character_info" | "model_list" | "model_info" | "memory_query"
        | "memory_changelog" => ToolClass::ReadOnly,

        // ── mutating ───────────────────────────────────────────────
        "send"
        | "regen"
        | "log_delete"
        | "log_edit"
        | "config_set"
        | "config_reset"
        | "character_switch"
        | "character_new"
        | "model_switch"
        | "model_reset"
        | "memory_compact"
        | "usage_refresh_pricing"
        | "usage_recalculate"
        | "debug_tick_now"
        | "debug_status_dormant"
        | "debug_status_active" => ToolClass::Mutating,

        _ => ToolClass::Unknown,
    }
}

pub fn check(tool: &str, ctx: &GateContext) -> GateDecision {
    match classify(tool) {
        ToolClass::ReadOnly => GateDecision::Allow,
        ToolClass::Mutating => {
            if ctx.profile_is_test || ctx.allow_main_writes {
                GateDecision::Allow
            } else {
                GateDecision::Refuse(format!(
                    "refused: tool `{tool}` mutates state and cannot run \
                     against the main profile. Re-launch without \
                     --attach-main, or pass --allow-main-writes to opt in."
                ))
            }
        }
        ToolClass::Unknown => GateDecision::Refuse(format!("refused: unknown tool `{tool}`")),
    }
}
