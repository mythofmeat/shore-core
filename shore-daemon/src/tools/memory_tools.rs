use super::{ToolCategory, ToolContext, ToolDef, ToolError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn tool_defs() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "memory",
        description: "Search or save a memory. Pass a natural language request describing what to search for or what to remember.",
        parameters: json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": "Natural language query to search memories, or a statement to save."
                }
            },
            "required": ["request"]
        }),
        category: ToolCategory::MemoryWrite,
    }]
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Handle the `memory` tool — search or save via the memory agent.
pub async fn handle_memory(input: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
    let request = input
        .get("request")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs("missing 'request' field".to_string()))?;

    let agent = ctx.memory_agent();
    let result = agent.query(request, ctx.rag(), ctx.memory_db()).await?;

    let entries: Vec<Value> = result
        .entries
        .iter()
        .map(|e| {
            json!({
                "entry_id": e.entry_id,
                "summary": e.summary_text,
                "type": e.memory_type,
                "confidence": e.confidence,
                "relevance": e.relevance_score,
            })
        })
        .collect();

    Ok(json!({
        "entries": entries,
        "query": result.query_text,
        "resolved_query": result.resolved_query,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::agent::{
        AgentError, AgentIndexer, AgentRag, CallerIdentity, MemoryAgent, RagHit,
    };
    use crate::memory::db::{Entry, MemoryDB};
    use chrono::Utc;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;

    struct MockRag {
        results: Vec<RagHit>,
    }

    impl AgentRag for MockRag {
        fn query(
            &self,
            _query: &str,
            _top_k: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<RagHit>, AgentError>> + Send + '_>> {
            let result = Ok(self.results.clone());
            Box::pin(async move { result })
        }
    }

    struct MockIndexer;

    impl AgentIndexer for MockIndexer {
        fn index_entry(
            &self,
            _entry_id: &str,
            _text: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct TestContext {
        db: MemoryDB,
        agent: MemoryAgent,
        rag: MockRag,
        indexer: MockIndexer,
    }

    impl ToolContext for TestContext {
        fn memory_db(&self) -> &MemoryDB {
            &self.db
        }
        fn memory_agent(&self) -> &MemoryAgent {
            &self.agent
        }
        fn rag(&self) -> &dyn AgentRag {
            &self.rag
        }
        fn indexer(&self) -> &dyn AgentIndexer {
            &self.indexer
        }
        fn image_dir(&self) -> &str {
            "/tmp/test_images"
        }
    }

    fn make_entry(id: &str, summary: &str) -> Entry {
        let now = Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: "semantic".to_string(),
            source: "agent".to_string(),
            reason: "tool_call".to_string(),
            status: "active".to_string(),
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

    #[test]
    fn test_memory_tool_def() {
        let defs = tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "memory");
        assert_eq!(defs[0].category, ToolCategory::MemoryWrite);
    }

    #[tokio::test]
    async fn test_handle_memory_returns_results() {
        let db = MemoryDB::open_in_memory().unwrap();
        let e1 = make_entry("e1", "Alice likes chocolate");
        db.create_entry(&e1).unwrap();

        let ctx = TestContext {
            db,
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Alice"),
            rag: MockRag {
                results: vec![RagHit {
                    entry_id: "e1".to_string(),
                    score: 0.95,
                }],
            },
            indexer: MockIndexer,
        };

        let result = handle_memory(json!({"request": "What do I like?"}), &ctx)
            .await
            .unwrap();

        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["entry_id"], "e1");
        assert_eq!(entries[0]["summary"], "Alice likes chocolate");
    }

    #[tokio::test]
    async fn test_handle_memory_missing_request() {
        let ctx = TestContext {
            db: MemoryDB::open_in_memory().unwrap(),
            agent: MemoryAgent::one_shot(CallerIdentity::Char, "Alice"),
            rag: MockRag { results: vec![] },
            indexer: MockIndexer,
        };

        let result = handle_memory(json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }
}
