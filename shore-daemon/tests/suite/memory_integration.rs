use shore_daemon::memory::compaction::{
    CompactionConfig, CompactionError, CompactionLlm, CompactionManager, CompactionOutcome,
    ConversationMessage, DEFAULT_COMPACT_PROMPT,
};
use shore_daemon::memory::compaction_impls::RealConversationManager;
use shore_daemon::memory::markdown_query;
use shore_daemon::memory::markdown_store::MarkdownMemoryStore;
use tempfile::TempDir;

struct MockCompactionLlm {
    response: String,
}

impl MockCompactionLlm {
    fn with_entries(entries: &[(&str, &str)]) -> Self {
        let mut xml = String::new();
        xml.push_str("<recap>Test recap of the conversation.</recap>\n");
        xml.push_str("<memory>\n");
        for (path, body) in entries {
            xml.push_str(&format!("<write path=\"{path}\">\n{body}\n</write>\n"));
        }
        xml.push_str("</memory>");
        Self { response: xml }
    }
}

impl CompactionLlm for MockCompactionLlm {
    fn summarize(
        &self,
        _prompt: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, CompactionError>> + Send + '_>,
    > {
        let response = self.response.clone();
        Box::pin(async move { Ok(response) })
    }
}

fn make_conversation() -> Vec<ConversationMessage> {
    vec![
        ConversationMessage {
            role: "user".into(),
            content: "I love ramen and tea.".into(),
            timestamp: "2026-04-23T10:00:00Z".into(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "assistant".into(),
            content: "Noted.".into(),
            timestamp: "2026-04-23T10:00:10Z".into(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "user".into(),
            content: "My cat Mochi knocked over a mug again.".into(),
            timestamp: "2026-04-23T10:01:00Z".into(),
            is_tool_result_only: false,
        },
        ConversationMessage {
            role: "assistant".into(),
            content: "Poor mug.".into(),
            timestamp: "2026-04-23T10:01:10Z".into(),
            is_tool_result_only: false,
        },
    ]
}

fn active_jsonl(messages: &[ConversationMessage]) -> String {
    messages
        .iter()
        .map(|msg| {
            serde_json::json!({
                "msg_id": format!("m_{}", uuid::Uuid::new_v4()),
                "role": msg.role,
                "content": msg.content,
                "images": [],
                "content_blocks": [{"type": "text", "text": msg.content}],
                "alt_index": serde_json::Value::Null,
                "alt_count": serde_json::Value::Null,
                "timestamp": msg.timestamp,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn test_markdown_memory_compaction_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let char_dir = tmp.path().join("Shore");
    std::fs::create_dir_all(&char_dir).unwrap();

    let messages = make_conversation();
    let active = active_jsonl(&messages);
    std::fs::write(char_dir.join("active.jsonl"), &active).unwrap();

    let store = MarkdownMemoryStore::open(char_dir.join("memories"))
        .await
        .unwrap();
    let conv_mgr = RealConversationManager::new(&char_dir);
    let llm = MockCompactionLlm::with_entries(&[
        (
            "people/user.md",
            "# User\n\n- Loves ramen\n- Prefers tea over coffee",
        ),
        (
            "topics/pets/mochi.md",
            "# Mochi\n\n- The user's cat\n- Knocks over mugs",
        ),
    ]);
    let mgr = CompactionManager::new(CompactionConfig::default());

    let outcome = mgr
        .compact(
            "conv-1",
            &messages,
            &active,
            false,
            DEFAULT_COMPACT_PROMPT,
            None,
            "Shore",
            "User",
            &llm,
            &conv_mgr,
            Some(&store),
            false,
            Some(1),
        )
        .await
        .unwrap();

    let result = match outcome {
        CompactionOutcome::Compacted(result) => result,
        CompactionOutcome::DryRun(_) => panic!("expected real compaction"),
    };

    assert_eq!(result.memory_files_written.len(), 2);
    assert!(store.read("people/user.md").await.is_ok());
    assert!(store.read("topics/pets/mochi.md").await.is_ok());

    let dreams = store.read("DREAMS.md").await.unwrap();
    assert!(dreams.content.contains("Updated memory files"));

    let recap = std::fs::read_to_string(char_dir.join("memory").join("recap.md")).unwrap();
    assert!(recap.contains("Test recap of the conversation."));

    let direct =
        markdown_query::format_direct_response("ramen", &store.search_text("ramen").await.unwrap());
    assert!(direct.contains("people/user.md"));
}

#[tokio::test]
async fn test_compaction_rejects_private_conversation() {
    let tmp = TempDir::new().unwrap();
    let char_dir = tmp.path().join("Shore");
    std::fs::create_dir_all(&char_dir).unwrap();
    let store = MarkdownMemoryStore::open(char_dir.join("memories"))
        .await
        .unwrap();
    let conv_mgr = RealConversationManager::new(&char_dir);
    let llm =
        MockCompactionLlm::with_entries(&[("people/user.md", "# User\n\n- Should not exist")]);
    let mgr = CompactionManager::new(CompactionConfig::default());

    let result = mgr
        .compact(
            "private-conv",
            &make_conversation(),
            "",
            true,
            DEFAULT_COMPACT_PROMPT,
            None,
            "Shore",
            "User",
            &llm,
            &conv_mgr,
            Some(&store),
            false,
            None,
        )
        .await;

    assert!(matches!(result, Err(CompactionError::PrivateConversation)));
    assert!(store.read("people/user.md").await.is_err());
}
