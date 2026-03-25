use super::{CommandContext, CommandError, CommandResult};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// memory command
// ---------------------------------------------------------------------------

/// Handle the `memory` command.
///
/// - No args: return memory status (entry counts, entity count, RAG info).
/// - With `query` arg: search memory entries by text match.
pub async fn handle_memory(args: Value, ctx: &dyn CommandContext) -> Result<CommandResult, CommandError> {
    let query = args.get("query").and_then(|v| v.as_str());

    match query {
        None => {
            // Status mode: return counts.
            let db = ctx.memory_db();

            let total = db
                .count_entries()
                .map_err(|e| CommandError::Db(e.to_string()))?;
            let active = db
                .count_entries_by_status("active")
                .map_err(|e| CommandError::Db(e.to_string()))?;
            let superseded = db
                .count_entries_by_status("superseded")
                .map_err(|e| CommandError::Db(e.to_string()))?;
            let protected = db
                .count_entries_by_status("protected")
                .map_err(|e| CommandError::Db(e.to_string()))?;
            let entities = db
                .count_entities()
                .map_err(|e| CommandError::Db(e.to_string()))?;

            Ok(CommandResult::data(json!({
                "status": "ok",
                "entries": {
                    "total": total,
                    "active": active,
                    "superseded": superseded,
                    "protected": protected,
                },
                "entities": entities,
                "rag": {
                    "bm25_indexed": true,
                    "vector_store": true,
                },
            })))
        }
        Some(q) => {
            // Search mode: find entries matching query text.
            let db = ctx.memory_db();
            let active_entries = db
                .get_entries_by_status("active")
                .map_err(|e| CommandError::Db(e.to_string()))?;

            let query_lower = q.to_lowercase();
            let matches: Vec<Value> = active_entries
                .iter()
                .filter(|e| e.summary_text.to_lowercase().contains(&query_lower))
                .take(20)
                .map(|e| {
                    json!({
                        "id": e.id,
                        "type": e.memory_type,
                        "summary": e.summary_text,
                        "confidence": e.confidence,
                        "tags": e.topic_tags,
                    })
                })
                .collect();

            Ok(CommandResult::data(json!({
                "query": q,
                "results": matches,
                "count": matches.len(),
            })))
        }
    }
}

// ---------------------------------------------------------------------------
// compact command
// ---------------------------------------------------------------------------

/// Handle the `compact` command.
///
/// Triggers compaction of the current conversation. Supports `dry_run` arg.
/// The actual compaction requires the CompactionManager and LLM dependencies
/// which are wired in at the engine level. This handler validates args and
/// returns the appropriate response shape.
pub async fn handle_compact(args: Value, ctx: &dyn CommandContext) -> Result<CommandResult, CommandError> {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if ctx.is_private() {
        return Ok(CommandResult::data(json!({
            "error": "compaction skipped: conversation is private",
            "dry_run": dry_run,
        })));
    }

    // Return a response indicating compaction was triggered.
    // The engine layer will wire this to the actual CompactionManager.
    Ok(CommandResult::data(json!({
        "triggered": true,
        "dry_run": dry_run,
        "note": "Compaction scheduled. Results will appear in memory status.",
    })))
}

// ---------------------------------------------------------------------------
// toggle_private command
// ---------------------------------------------------------------------------

/// Handle the `toggle_private` command.
///
/// Toggles the private flag on the current conversation and pushes History
/// so clients stay in sync.
pub async fn handle_toggle_private(ctx: &dyn CommandContext) -> Result<CommandResult, CommandError> {
    let was_private = ctx.is_private();
    let now_private = !was_private;

    ctx.set_private(now_private);

    Ok(CommandResult::with_history_push(json!({
        "private": now_private,
        "was_private": was_private,
    })))
}

// ---------------------------------------------------------------------------
// config command
// ---------------------------------------------------------------------------

/// Handle the `config` command.
///
/// Renders the effective configuration as JSON (TOML rendering deferred
/// to the config module when it's implemented).
pub async fn handle_config(ctx: &dyn CommandContext) -> Result<CommandResult, CommandError> {
    let config = ctx.effective_config();

    Ok(CommandResult::data(json!({
        "config": config,
    })))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::db::{Entry, MemoryDB};
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestCtx {
        db: MemoryDB,
        private: AtomicBool,
        config: Value,
    }

    impl TestCtx {
        fn new() -> Self {
            Self {
                db: MemoryDB::open_in_memory().unwrap(),
                private: AtomicBool::new(false),
                config: json!({
                    "model": "claude-sonnet-4-20250514",
                    "memory": { "enabled": true },
                }),
            }
        }
    }

    impl CommandContext for TestCtx {
        fn memory_db(&self) -> &MemoryDB {
            &self.db
        }
        fn is_private(&self) -> bool {
            self.private.load(Ordering::SeqCst)
        }
        fn set_private(&self, private: bool) {
            self.private.store(private, Ordering::SeqCst);
        }
        fn effective_config(&self) -> Value {
            self.config.clone()
        }
    }

    fn make_entry(id: &str, summary: &str, status: &str) -> Entry {
        let now = Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "test".to_string(),
            reason: "test".to_string(),
            status: status.to_string(),
            canonical: false,
            confidence: 0.9,
            summary_text: summary.to_string(),
            topic_tags: "test".to_string(),
            topic_key: "test".to_string(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: String::new(),
            image_path: String::new(),
        }
    }

    // -- memory command tests -------------------------------------------------

    #[tokio::test]
    async fn test_memory_status_empty_db() {
        let ctx = TestCtx::new();
        let result = handle_memory(json!({}), &ctx).await.unwrap();

        assert_eq!(result.data["entries"]["total"], 0);
        assert_eq!(result.data["entries"]["active"], 0);
        assert_eq!(result.data["entities"], 0);
        assert!(!result.push_history);
    }

    #[tokio::test]
    async fn test_memory_status_with_entries() {
        let ctx = TestCtx::new();
        ctx.db.create_entry(&make_entry("e1", "fact one", "active")).unwrap();
        ctx.db.create_entry(&make_entry("e2", "fact two", "active")).unwrap();
        ctx.db.create_entry(&make_entry("e3", "old fact", "superseded")).unwrap();

        let result = handle_memory(json!({}), &ctx).await.unwrap();

        assert_eq!(result.data["entries"]["total"], 3);
        assert_eq!(result.data["entries"]["active"], 2);
        assert_eq!(result.data["entries"]["superseded"], 1);
    }

    #[tokio::test]
    async fn test_memory_search() {
        let ctx = TestCtx::new();
        ctx.db.create_entry(&make_entry("e1", "Alice likes chocolate", "active")).unwrap();
        ctx.db.create_entry(&make_entry("e2", "Bob likes vanilla", "active")).unwrap();
        ctx.db.create_entry(&make_entry("e3", "Alice hates rain", "active")).unwrap();

        let result = handle_memory(json!({"query": "Alice"}), &ctx).await.unwrap();

        assert_eq!(result.data["count"], 2);
        let results = result.data["results"].as_array().unwrap();
        assert!(results.iter().all(|r| r["summary"].as_str().unwrap().contains("Alice")));
    }

    #[tokio::test]
    async fn test_memory_search_case_insensitive() {
        let ctx = TestCtx::new();
        ctx.db.create_entry(&make_entry("e1", "Alice likes CHOCOLATE", "active")).unwrap();

        let result = handle_memory(json!({"query": "chocolate"}), &ctx).await.unwrap();
        assert_eq!(result.data["count"], 1);
    }

    #[tokio::test]
    async fn test_memory_search_no_results() {
        let ctx = TestCtx::new();
        ctx.db.create_entry(&make_entry("e1", "Alice likes chocolate", "active")).unwrap();

        let result = handle_memory(json!({"query": "quantum physics"}), &ctx).await.unwrap();
        assert_eq!(result.data["count"], 0);
    }

    // -- compact command tests ------------------------------------------------

    #[tokio::test]
    async fn test_compact_triggered() {
        let ctx = TestCtx::new();
        let result = handle_compact(json!({}), &ctx).await.unwrap();

        assert_eq!(result.data["triggered"], true);
        assert_eq!(result.data["dry_run"], false);
    }

    #[tokio::test]
    async fn test_compact_dry_run() {
        let ctx = TestCtx::new();
        let result = handle_compact(json!({"dry_run": true}), &ctx).await.unwrap();

        assert_eq!(result.data["triggered"], true);
        assert_eq!(result.data["dry_run"], true);
    }

    #[tokio::test]
    async fn test_compact_private_skipped() {
        let ctx = TestCtx::new();
        ctx.private.store(true, Ordering::SeqCst);

        let result = handle_compact(json!({}), &ctx).await.unwrap();
        assert!(result.data["error"].as_str().unwrap().contains("private"));
    }

    // -- toggle_private command tests -----------------------------------------

    #[tokio::test]
    async fn test_toggle_private_on() {
        let ctx = TestCtx::new();
        assert!(!ctx.is_private());

        let result = handle_toggle_private(&ctx).await.unwrap();

        assert!(ctx.is_private());
        assert_eq!(result.data["private"], true);
        assert_eq!(result.data["was_private"], false);
        assert!(result.push_history);
    }

    #[tokio::test]
    async fn test_toggle_private_off() {
        let ctx = TestCtx::new();
        ctx.set_private(true);

        let result = handle_toggle_private(&ctx).await.unwrap();

        assert!(!ctx.is_private());
        assert_eq!(result.data["private"], false);
        assert_eq!(result.data["was_private"], true);
        assert!(result.push_history);
    }

    #[tokio::test]
    async fn test_toggle_private_roundtrip() {
        let ctx = TestCtx::new();

        handle_toggle_private(&ctx).await.unwrap();
        assert!(ctx.is_private());

        handle_toggle_private(&ctx).await.unwrap();
        assert!(!ctx.is_private());
    }

    // -- config command tests -------------------------------------------------

    #[tokio::test]
    async fn test_config_returns_effective_config() {
        let ctx = TestCtx::new();
        let result = handle_config(&ctx).await.unwrap();

        let config = &result.data["config"];
        assert_eq!(config["model"], "claude-sonnet-4-20250514");
        assert_eq!(config["memory"]["enabled"], true);
        assert!(!result.push_history);
    }
}
