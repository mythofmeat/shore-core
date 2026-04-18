use super::*;
use crate::commands::CommandContext;
use crate::engine::ConversationEngine;
use crate::memory::db::MemoryDB;
use serde_json::json;
use shore_config::models::ModelCatalog;
use shore_protocol::server_msg::ServerMessage;
use shore_protocol::types::{Message, Role};
use std::panic::{catch_unwind, AssertUnwindSafe};
use tempfile::TempDir;
use tokio::sync::broadcast;

fn make_ctx(
    tmp: &TempDir,
) -> (
    ConversationEngine,
    CommandContext,
    broadcast::Receiver<ServerMessage>,
) {
    make_ctx_with_models(tmp, ModelCatalog::default())
}

fn make_ctx_with_models(
    tmp: &TempDir,
    models: ModelCatalog,
) -> (
    ConversationEngine,
    CommandContext,
    broadcast::Receiver<ServerMessage>,
) {
    let (push_tx, push_rx) = broadcast::channel(16);
    let data_dir = tmp.path().to_path_buf();
    let engine =
        ConversationEngine::new("TestChar".to_string(), data_dir.clone(), push_tx.clone()).unwrap();

    let config = shore_config::LoadedConfig::new_for_test(
        shore_config::app::AppConfig::default(),
        models,
        shore_config::ShoreDirs {
            config: tmp.path().join("config"),
            data: data_dir.clone(),
            runtime: tmp.path().join("runtime"),
            cache: tmp.path().join("cache"),
        },
    );

    let (_tx, rx) = tokio::sync::watch::channel(());
    let autonomy = crate::autonomy::manager::AutonomyManager::new(
        Default::default(),
        Default::default(),
        data_dir.clone(),
        rx,
    );

    let ctx = CommandContext {
        config,
        push_tx,
        data_dir: data_dir.clone(),
        active_model: None,
        reasoning_effort_override: None,
        session_tokens: std::sync::Arc::new(std::sync::Mutex::new(
            crate::commands::SessionTokens::default(),
        )),
        autonomy,
        llm_client: shore_ledger::LedgerClient::new(
            shore_llm_client::LlmClient::new(),
            &data_dir.join("ledger.db"),
        )
        .unwrap(),
        diagnostics: std::sync::Arc::new(std::sync::Mutex::new(
            shore_diagnostics::Diagnostics::default(),
        )),
        memory_shell_sessions: std::collections::HashMap::new(),
    };
    (engine, ctx, push_rx)
}

fn sample_models() -> ModelCatalog {
    let toml_str = r#"
[anthropic.claude-sonnet]
model_id = "claude-sonnet-4-20250514"

[openrouter.gpt-4o]
model_id = "gpt-4o"
"#;
    let table: toml::Table = toml_str.parse().unwrap();
    ModelCatalog::from_sections(Some(&table), None, None, None).unwrap()
}

fn make_msg(id: &str, role: Role, content: &str) -> Message {
    Message {
        msg_id: id.to_string(),
        role,
        content: content.to_string(),
        images: vec![],
        content_blocks: vec![],
        alt_index: None,
        alt_count: None,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
    }
}

#[test]
fn status_returns_state() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let tmp = TempDir::new().unwrap();

    rt.block_on(async {
        let (engine, mut ctx, _rx) = make_ctx(&tmp);
        ctx.active_model = Some("claude-sonnet".into());
        ctx.autonomy.ensure_state(engine.character_name(), None);

        let result = status(&engine, &ctx).unwrap();
        assert_eq!(result["character"], "TestChar");
        assert_eq!(result["message_count"], 0);
        assert_eq!(result["active_model"], "claude-sonnet");
        assert_eq!(result["autonomy"]["interiority_state"], "Active");
    });
}

#[test]
fn status_reports_dormant_interiority() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let tmp = TempDir::new().unwrap();

    rt.block_on(async {
        let (engine, ctx, _rx) = make_ctx(&tmp);

        ctx.autonomy.ensure_state(engine.character_name(), None);
        assert!(ctx
            .autonomy
            .interiority_set_dormant(engine.character_name()));

        let result = status(&engine, &ctx).unwrap();
        assert_eq!(result["autonomy"]["interiority_state"], "Dormant");
    });
}

#[test]
fn status_with_messages() {
    let tmp = TempDir::new().unwrap();
    let (mut engine, ctx, _rx) = make_ctx(&tmp);
    engine
        .append_message(make_msg("m1", Role::User, "Hi"))
        .unwrap();

    let result = status(&engine, &ctx).unwrap();
    assert_eq!(result["message_count"], 1);
}

#[test]
fn list_models_empty() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx(&tmp);

    let result = list_models(&ctx).unwrap();
    assert!(result["models"].as_array().unwrap().is_empty());
    assert!(result["active"].is_null());
}

#[test]
fn list_models_excludes_tool_models() {
    // Regression: list_models previously merged chat AND tools into one
    // flat list. Tool-only profiles (e.g. a function-calling sidecar model
    // used for memory collation) are not meant to be user-selectable chat
    // targets, and they pollute the UI in `shore models list` /
    // auto-completions.
    let tmp = TempDir::new().unwrap();
    let toml_str = r#"
[chat.anthropic.claude-sonnet]
model_id = "claude-sonnet-4-20250514"

[tools.openai.collator]
model_id = "gpt-4o-mini"
"#;
    let table: toml::Table = toml_str.parse().unwrap();
    let chat = table.get("chat").and_then(|v| v.as_table());
    let tools = table.get("tools").and_then(|v| v.as_table());
    let catalog = ModelCatalog::from_sections(chat, tools, None, None).unwrap();
    assert_eq!(catalog.chat.len(), 1, "sanity: one chat model");
    assert_eq!(catalog.tools.len(), 1, "sanity: one tool model");

    let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, catalog);
    let result = list_models(&ctx).unwrap();
    let models = result["models"].as_array().unwrap();
    assert_eq!(
        models.len(),
        1,
        "list_models must only return chat models; tool models are not user-selectable"
    );
    assert_eq!(models[0]["name"], "claude-sonnet");
    assert!(
        models.iter().all(|m| m["name"] != "collator"),
        "tool model should not appear in list"
    );
}

#[test]
fn list_models_with_profiles() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = list_models(&ctx).unwrap();
    let models = result["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["name"], "claude-sonnet");
    assert_eq!(models[1]["name"], "gpt-4o");
}

#[test]
fn switch_model_show_current() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);
    ctx.active_model = Some("claude-sonnet".into());

    let result = switch_model(&mut ctx, &json!({})).unwrap();
    assert_eq!(result["active"], "claude-sonnet");
}

#[test]
fn switch_model_valid() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = switch_model(&mut ctx, &json!({"name": "gpt-4o"})).unwrap();
    assert_eq!(result["active"], "gpt-4o");
    assert_eq!(result["changed"], true);
    assert_eq!(ctx.active_model.as_deref(), Some("gpt-4o"));
}

#[test]
fn switch_model_not_found() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = switch_model(&mut ctx, &json!({"name": "nonexistent"}));
    assert!(result.is_err());
    let (code, _msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::NotFound);
}

#[test]
fn model_info_by_name() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = model_info(&ctx, &json!({"name": "claude-sonnet"})).unwrap();
    assert_eq!(result["name"], "claude-sonnet");
    assert_eq!(result["model_id"], "claude-sonnet-4-20250514");
    assert!(result["sdk"].is_string());
}

#[test]
fn model_info_uses_active_model() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
    ctx.active_model = Some("gpt-4o".into());

    let result = model_info(&ctx, &json!({})).unwrap();
    assert_eq!(result["name"], "gpt-4o");
}

#[test]
fn model_info_no_model() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx(&tmp);

    let result = model_info(&ctx, &json!({}));
    assert!(result.is_err());
    let (code, _msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::InvalidRequest);
}

#[test]
fn model_info_not_found() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = model_info(&ctx, &json!({"name": "nonexistent"}));
    assert!(result.is_err());
    let (code, _msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::NotFound);
}

#[tokio::test]
async fn memory_status_no_db() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let result = memory(&engine, &ctx, &json!({})).await.unwrap();
    assert_eq!(result["character"], "TestChar");
    assert_eq!(result["entries"], 0);
    assert_eq!(result["entities"], 0);
    assert_eq!(result["active_entries"], 0);
}

#[tokio::test]
async fn memory_status_with_db() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let db_dir = ctx.data_dir.join("TestChar").join("memory");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();
    db.create_entry(&crate::memory::db::Entry {
        id: "test_1".into(),
        memory_type: "semantic".into(),
        source: "test".into(),
        reason: "test".into(),
        status: "active".into(),
        confidence: 0.9,
        summary_text: "Test entry".into(),
        topic_tags: "test".into(),
        topic_key: "test".into(),
        start_timestamp: String::new(),
        end_timestamp: String::new(),
        message_count: 0,
        source_entry_ids: String::new(),
        related_entry_ids: String::new(),
        superseded_by: String::new(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        entry_type: String::new(),
        image_path: String::new(),
        collated_at: String::new(),
    })
    .unwrap();
    drop(db);

    let result = memory(&engine, &ctx, &json!({})).await.unwrap();
    assert_eq!(result["character"], "TestChar");
    assert_eq!(result["entries"], 1);
    assert_eq!(result["active_entries"], 1);
}

#[tokio::test]
async fn memory_status_null_query_is_status() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let result = memory(&engine, &ctx, &json!({"query": null}))
        .await
        .unwrap();
    assert_eq!(result["character"], "TestChar");
    assert!(result.get("entries").is_some());
}

#[tokio::test]
async fn compact_empty_conversation_returns_error() {
    let tmp = TempDir::new().unwrap();
    let (mut engine, ctx, _rx) = make_ctx(&tmp);

    let result = compact(&mut engine, &ctx, &json!({})).await;
    assert!(result.is_err());
    let (code, msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::InvalidRequest);
    assert!(msg.contains("No messages"));
}

#[tokio::test]
async fn collate_no_model_returns_error() {
    let tmp = TempDir::new().unwrap();
    let (mut engine, ctx, _rx) = make_ctx(&tmp);

    let db_dir = ctx.data_dir.join("TestChar").join("memory");
    std::fs::create_dir_all(&db_dir).unwrap();
    let _db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

    let result = collate(&mut engine, &ctx, &json!({})).await;
    assert!(result.is_err());
    let (code, _msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::InternalError);
}

#[test]
fn config_check_empty_catalog() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx(&tmp);

    let result = config_check(&ctx).unwrap();
    assert!(!result["valid"].as_bool().unwrap());
    let warnings = result["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("No chat models")));
    assert_eq!(result["chat_models"], 0);
}

#[test]
fn config_check_with_models() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = config_check(&ctx).unwrap();
    assert_eq!(result["chat_models"], 2);
    let info = result["info"].as_array().unwrap();
    assert!(info
        .iter()
        .any(|i| i.as_str().unwrap().contains("2 chat model")));
}

#[test]
fn config_full() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);

    let result = config(&mut ctx, &json!({})).unwrap();
    assert!(result["config"].is_object());
}

#[test]
fn config_section() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);

    let result = config(&mut ctx, &json!({"key": "defaults"})).unwrap();
    assert_eq!(result["key"], "defaults");
}

#[test]
fn config_section_not_found() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);

    let result = config(&mut ctx, &json!({"key": "nonexistent"}));
    assert!(result.is_err());
    let (code, _msg) = result.unwrap_err();
    assert_eq!(code, shore_protocol::error::ErrorCode::NotFound);
}

#[test]
fn test_diagnostics_empty() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx(&tmp);

    let result = diagnostics(&ctx, &json!({})).unwrap();
    assert_eq!(result["api_calls"]["count"], 0);
    assert_eq!(result["tool_calls"]["count"], 0);
    assert_eq!(result["errors"]["count"], 0);
    assert!(result["api_calls"]["recent"].as_array().unwrap().is_empty());
}

#[test]
fn status_recovers_from_poisoned_session_tokens_mutex() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = ctx.session_tokens.lock().unwrap();
        panic!("poison command session tokens");
    }));
    assert!(result.is_err());

    let status_json = status(&engine, &ctx).unwrap();
    assert_eq!(status_json["tokens"]["input"], 0);
    assert_eq!(status_json["tokens"]["output"], 0);
}

#[test]
fn diagnostics_recovers_from_poisoned_mutex() {
    let tmp = TempDir::new().unwrap();
    let (_engine, ctx, _rx) = make_ctx(&tmp);

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = ctx.diagnostics.lock().unwrap();
        panic!("poison command diagnostics");
    }));
    assert!(result.is_err());

    let diag_json = diagnostics(&ctx, &json!({})).unwrap();
    assert_eq!(diag_json["errors"]["count"], 0);
}

#[test]
fn test_heartbeat_log_empty() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let result = heartbeat_log(&engine, &ctx, &json!({})).unwrap();
    assert!(result["events"].as_array().unwrap().is_empty());
}

#[test]
fn test_reset_model_clears_override() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);

    ctx.active_model = Some("custom-override".to_string());
    let result = reset_model(&mut ctx).unwrap();

    assert_eq!(result["previous"], "custom-override");
    assert_eq!(result["reset_to"], "config default");
    assert!(ctx.active_model.is_none());
}

#[tokio::test]
async fn memory_purge_deletes_old_superseded_entries() {
    use crate::memory::db::Entry;

    let tmp = TempDir::new().unwrap();
    let (mut engine, ctx, _rx) = make_ctx(&tmp);

    let mem_dir = ctx.data_dir.join("TestChar").join("memory");
    std::fs::create_dir_all(&mem_dir).unwrap();
    let db = MemoryDB::open(&mem_dir.join("memory.db")).unwrap();

    let old_ts = "2020-01-01T00:00:00Z".to_string();
    let recent_ts = chrono::Local::now().to_rfc3339();
    let empty = String::new();

    let make_entry =
        |id: &str, status: &str, superseded_by: &str, image_path: &str, ts: &str| Entry {
            id: id.into(),
            memory_type: "observation".into(),
            source: "test".into(),
            reason: empty.clone(),
            status: status.into(),
            confidence: 1.0,
            summary_text: format!("Entry {id}"),
            topic_tags: empty.clone(),
            topic_key: empty.clone(),
            start_timestamp: empty.clone(),
            end_timestamp: empty.clone(),
            message_count: 0,
            source_entry_ids: empty.clone(),
            related_entry_ids: empty.clone(),
            superseded_by: superseded_by.into(),
            created_at: ts.into(),
            updated_at: ts.into(),
            entry_type: empty.clone(),
            image_path: image_path.into(),
            collated_at: empty.clone(),
        };

    db.create_entry(&make_entry("active-1", "active", "", "", &old_ts))
        .unwrap();
    db.create_entry(&make_entry(
        "old-superseded",
        "superseded",
        "active-1",
        "",
        &old_ts,
    ))
    .unwrap();
    db.create_entry(&make_entry(
        "recent-superseded",
        "superseded",
        "active-1",
        "",
        &recent_ts,
    ))
    .unwrap();
    db.create_entry(&make_entry(
        "image-superseded",
        "superseded",
        "active-1",
        "/some/image.png",
        &old_ts,
    ))
    .unwrap();
    db.create_entry(&make_entry("no-replacement", "superseded", "", "", &old_ts))
        .unwrap();

    drop(db);

    let result = memory_purge(&mut engine, &ctx, &json!({"older_than": "1d"}))
        .await
        .unwrap();

    assert_eq!(result["deleted"], 1);
    assert_eq!(result["skipped_image"], 1);
    assert_eq!(result["skipped_no_replacement"], 1);

    let db = MemoryDB::open(&mem_dir.join("memory.db")).unwrap();
    assert!(db.get_entry("old-superseded").unwrap().is_none());
    assert!(db.get_entry("recent-superseded").unwrap().is_some());
    assert!(db.get_entry("image-superseded").unwrap().is_some());
    assert!(db.get_entry("no-replacement").unwrap().is_some());
}

#[test]
fn config_reset_clears_active_model_and_reloads() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx(&tmp);

    std::fs::create_dir_all(tmp.path().join("config")).unwrap();
    std::fs::write(
        tmp.path().join("config").join("config.toml"),
        "[defaults]\nstream = false\n",
    )
    .unwrap();

    ctx.active_model = Some("custom-override".into());
    ctx.config.app.defaults.stream = true;
    ctx.memory_shell_sessions.insert(
        "shell-1".into(),
        crate::commands::MemoryShellSession {
            agent: crate::memory::agent::MemoryAgent::interactive(
                crate::memory::agent::CallerIdentity::User,
                "TestChar",
                "User",
            ),
            history: vec![],
            character: "TestChar".into(),
            model: sample_models().first_chat_model().cloned().unwrap(),
        },
    );

    let result = config_reset(&mut ctx).unwrap();

    assert_eq!(result["reset"], true);
    assert!(ctx.active_model.is_none(), "active_model should be cleared");
    assert!(
        !ctx.config.app.defaults.stream,
        "config should be reloaded from ctx.config.dirs.config"
    );
    assert!(
        ctx.memory_shell_sessions.is_empty(),
        "memory shell sessions should be cleared because they hold stale runtime state"
    );
}

#[tokio::test]
async fn memory_reindex_empty_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let (engine, ctx, _rx) = make_ctx(&tmp);

    let db_dir = ctx.data_dir.join("TestChar").join("memory");
    std::fs::create_dir_all(&db_dir).unwrap();
    let _db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

    let result = memory_reindex(&engine, &ctx).await.unwrap();
    assert_eq!(result["reindexed"], 0);
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("No active entries"));
}

#[tokio::test]
#[ignore = "Requires OPENROUTER_SHORE_EMBEDDING"]
async fn memory_reindex_rebuilds_fts_and_vectors() {
    if std::env::var("OPENROUTER_SHORE_EMBEDDING").is_err() {
        panic!("OPENROUTER_SHORE_EMBEDDING not set");
    }

    let tmp = TempDir::new().unwrap();

    let embed_toml: toml::Table = r#"
[text-embed]
model_id = "openai/text-embedding-3-small"
provider = "openai"
api_key_env = "OPENROUTER_SHORE_EMBEDDING"
base_url = "https://openrouter.ai/api/v1"
dimensions = 1536
"#
    .parse()
    .unwrap();
    let models = ModelCatalog::from_sections(None, None, Some(&embed_toml), None).unwrap();
    let (engine, ctx, _rx) = make_ctx_with_models(&tmp, models);

    let db_dir = ctx.data_dir.join("TestChar").join("memory");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = MemoryDB::open(&db_dir.join("memory.db")).unwrap();

    for i in 0..3 {
        db.create_entry(&crate::memory::db::Entry {
            id: format!("entry_{i}"),
            memory_type: "semantic".into(),
            source: "test".into(),
            reason: "reindex test".into(),
            status: "active".into(),
            confidence: 0.9,
            summary_text: format!("Test memory entry number {i} about various topics"),
            topic_tags: "test".into(),
            topic_key: "test".into(),
            start_timestamp: String::new(),
            end_timestamp: String::new(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        })
        .unwrap();
    }
    drop(db);

    let result = memory_reindex(&engine, &ctx).await.unwrap();
    assert_eq!(result["reindexed"], 3);
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("Reindexed 3 entries"));
}

#[test]
fn rfc3339_comparison_handles_mixed_timezones() {
    let utc = "2026-01-01T00:00:00Z";
    let tokyo = "2026-01-01T09:00:00+09:00";

    let utc_dt = chrono::DateTime::parse_from_rfc3339(utc).unwrap();
    let tokyo_dt = chrono::DateTime::parse_from_rfc3339(tokyo).unwrap();

    assert_eq!(utc_dt, tokyo_dt, "Same instant in different timezones");

    assert_ne!(
        utc.cmp(tokyo),
        utc_dt.cmp(&tokyo_dt),
        "String comparison should disagree with chronological - this confirms string comparison is unreliable for RFC 3339"
    );

    let before = "2025-12-31T23:59:59Z";
    let before_dt = chrono::DateTime::parse_from_rfc3339(before).unwrap();
    assert!(
        before_dt < tokyo_dt,
        "An entry from before the cutoff should be < cutoff chronologically"
    );
}

// ── set_reasoning_effort ─────────────────────────────────────────────────

#[test]
fn set_reasoning_effort_bare_read_shows_state() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
    ctx.active_model = Some("claude-sonnet".into());

    let result = set_reasoning_effort(&mut ctx, &json!({})).unwrap();
    assert!(result.get("changed").is_none(), "bare read must not mark changed");
    assert!(result["override"].is_null(), "no override by default");
    assert!(result["effective"].is_null(), "no config default, no override → null");
    assert!(ctx.reasoning_effort_override.is_none());
}

#[test]
fn set_reasoning_effort_sets_string_value() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result =
        set_reasoning_effort(&mut ctx, &json!({ "value": "high" })).unwrap();
    assert_eq!(result["changed"], true);
    assert_eq!(result["effective"], "high");
    assert_eq!(ctx.reasoning_effort_override, Some(Some("high".into())));
}

#[test]
fn set_reasoning_effort_null_forces_off() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let result = set_reasoning_effort(
        &mut ctx,
        &json!({ "value": serde_json::Value::Null }),
    )
    .unwrap();
    assert_eq!(result["changed"], true);
    assert!(result["effective"].is_null(), "forced off");
    assert_eq!(ctx.reasoning_effort_override, Some(None));
}

#[test]
fn set_reasoning_effort_off_sentinel_forces_off() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    // Each of these should collapse to "force off" (override = Some(None)).
    for sentinel in ["off", "OFF", "none", "disable", "disabled", "unset"] {
        ctx.reasoning_effort_override = Some(Some("high".into())); // reset
        let result =
            set_reasoning_effort(&mut ctx, &json!({ "value": sentinel })).unwrap();
        assert_eq!(
            ctx.reasoning_effort_override,
            Some(None),
            "sentinel {sentinel:?} should force off"
        );
        assert!(result["effective"].is_null());
    }
}

#[test]
fn set_reasoning_effort_clear_flag_removes_override() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
    ctx.reasoning_effort_override = Some(Some("high".into()));

    let result = set_reasoning_effort(&mut ctx, &json!({ "clear": true })).unwrap();
    assert_eq!(result["changed"], true);
    assert!(result["override"].is_null(), "cleared → no override");
    assert!(ctx.reasoning_effort_override.is_none());
}

#[test]
fn set_reasoning_effort_empty_string_clears() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());
    ctx.reasoning_effort_override = Some(Some("medium".into()));

    let _ = set_reasoning_effort(&mut ctx, &json!({ "value": "" })).unwrap();
    assert!(
        ctx.reasoning_effort_override.is_none(),
        "empty string should clear the override, not store an empty value"
    );
}

#[test]
fn set_reasoning_effort_rejects_non_string_non_null_value() {
    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, sample_models());

    let err = set_reasoning_effort(&mut ctx, &json!({ "value": 7 })).unwrap_err();
    assert_eq!(err.0, shore_protocol::error::ErrorCode::InvalidRequest);
    assert!(ctx.reasoning_effort_override.is_none(), "rejected input must not mutate state");
}

#[test]
fn set_reasoning_effort_reports_config_default_when_model_has_one() {
    // Confirm that the `config_default` field reflects the resolved model's
    // reasoning_effort from the catalog. This is what lets the CLI tell the
    // user "we'd inherit X if you cleared the override."
    let toml_str = r#"
[anthropic.claude-sonnet]
model_id = "claude-sonnet-4-20250514"
reasoning_effort = "medium"
"#;
    let table: toml::Table = toml_str.parse().unwrap();
    let catalog = ModelCatalog::from_sections(Some(&table), None, None, None).unwrap();

    let tmp = TempDir::new().unwrap();
    let (_engine, mut ctx, _rx) = make_ctx_with_models(&tmp, catalog);
    ctx.active_model = Some("claude-sonnet".into());

    let result = set_reasoning_effort(&mut ctx, &json!({})).unwrap();
    assert_eq!(result["config_default"], "medium");
    assert_eq!(
        result["effective"], "medium",
        "no override → effective = config default"
    );
}
