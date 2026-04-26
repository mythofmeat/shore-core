use shore_mcp::gating::{check, GateContext, GateDecision};

fn test_ctx(is_test: bool, allow_main_writes: bool) -> GateContext {
    GateContext {
        profile_is_test: is_test,
        allow_main_writes,
    }
}

#[test]
fn read_only_tools_always_allowed() {
    let ctx = test_ctx(false, false);
    for tool in &[
        "status",
        "status_diagnostics",
        "log_tail",
        "log_show",
        "log_heartbeat",
        "usage",
        "config_get",
        "config_check",
        "character_list",
        "character_info",
        "model_list",
        "model_info",
        "memory_query",
    ] {
        assert_eq!(
            check(tool, &ctx),
            GateDecision::Allow,
            "read-only tool {tool} should always be allowed"
        );
    }
}

#[test]
fn mutating_tools_allowed_on_test_profile() {
    let ctx = test_ctx(true, false);
    for tool in &[
        "send",
        "regen",
        "config_set",
        "character_switch",
        "model_switch",
        "log_delete",
    ] {
        assert_eq!(
            check(tool, &ctx),
            GateDecision::Allow,
            "mutating tool {tool} should be allowed on test profile"
        );
    }
}

#[test]
fn mutating_tools_refused_on_main_profile_without_allow_writes() {
    let ctx = test_ctx(false, false);
    for tool in &[
        "send",
        "regen",
        "config_set",
        "character_switch",
        "model_switch",
        "log_delete",
    ] {
        match check(tool, &ctx) {
            GateDecision::Refuse(_) => {}
            other => panic!("expected Refuse for {tool}, got {other:?}"),
        }
    }
}

#[test]
fn mutating_tools_allowed_on_main_with_explicit_opt_in() {
    let ctx = test_ctx(false, true);
    assert_eq!(check("send", &ctx), GateDecision::Allow);
    assert_eq!(check("config_set", &ctx), GateDecision::Allow);
}

#[test]
fn unknown_tools_are_refused() {
    let ctx = test_ctx(true, false);
    match check("nonexistent_tool", &ctx) {
        GateDecision::Refuse(msg) => assert!(msg.contains("unknown")),
        other => panic!("expected Refuse for unknown tool, got {other:?}"),
    }
}
