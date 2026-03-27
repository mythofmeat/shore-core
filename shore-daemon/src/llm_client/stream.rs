use shore_protocol::server_msg::{
    CacheWarning, ServerMessage, StreamChunk, StreamEnd, StreamStart,
};
use shore_protocol::types::{StreamMetadata, TimingInfo, TokenCounts};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use shore_protocol::types::ContentBlock;

use super::types::{StreamEvent, StreamResult, ToolUseEvent};
use super::LlmError;

/// Context for cache invalidation detection per section 13.3.
#[derive(Debug, Clone)]
pub struct CacheContext {
    /// Number of turns in the conversation before this request.
    pub conversation_turn_count: usize,

    /// Whether this is the first message after a daemon restart.
    pub is_first_after_restart: bool,

    /// Whether this is the first message after a compaction.
    pub is_first_after_compaction: bool,

    /// Whether cache invalidation warnings are enabled ([advanced] config).
    pub cache_invalidation_warnings: bool,
}

impl Default for CacheContext {
    fn default() -> Self {
        Self {
            conversation_turn_count: 0,
            is_first_after_restart: true,
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        }
    }
}

/// Consumes a newline-delimited JSON stream from shore-llm's /v1/stream
/// endpoint, relaying `StreamChunk` events (with content_type) to SWP clients
/// via the broadcast sender, and accumulating the final `StreamResult`.
pub struct StreamConsumer {
    push_tx: broadcast::Sender<ServerMessage>,
}

impl StreamConsumer {
    /// Create a new stream consumer that broadcasts to the given sender.
    pub fn new(push_tx: broadcast::Sender<ServerMessage>) -> Self {
        Self { push_tx }
    }

    /// Consume a streaming response from shore-llm.
    ///
    /// Reads newline-delimited JSON events, broadcasts `StreamStart`,
    /// `StreamChunk`, and `StreamEnd` to connected SWP clients, and returns
    /// the accumulated `StreamResult` with metadata.
    ///
    /// Cache invalidation is checked after the stream completes according to
    /// the rules in section 13.3.
    pub async fn consume(
        &self,
        reader: &mut BufReader<UnixStream>,
        regen: bool,
        cache_ctx: &CacheContext,
    ) -> Result<StreamResult, LlmError> {
        let mut model = String::new();
        let mut tool_uses = Vec::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut text_buf = String::new();
        let mut thinking_buf = String::new();
        let mut pending_signature: Option<String> = None;
        let mut started = false;

        // Flush accumulated text buffer into content_blocks.
        let flush_text = |buf: &mut String, blocks: &mut Vec<ContentBlock>| {
            if !buf.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: std::mem::take(buf),
                });
            }
        };

        // Flush accumulated thinking buffer into content_blocks, attaching any pending signature.
        let flush_thinking = |buf: &mut String, blocks: &mut Vec<ContentBlock>, sig: &mut Option<String>| {
            if !buf.is_empty() {
                blocks.push(ContentBlock::Thinking {
                    thinking: std::mem::take(buf),
                    signature: sig.take(),
                });
            }
        };

        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                // EOF — stream ended without "done" event.
                return Err(LlmError::IncompleteStream);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let event: StreamEvent =
                serde_json::from_str(trimmed).map_err(LlmError::Deserialize)?;

            match event {
                StreamEvent::Start {
                    model: start_model,
                } => {
                    model = start_model;
                    started = true;

                    // Broadcast stream_start to SWP clients.
                    let _ = self
                        .push_tx
                        .send(ServerMessage::StreamStart(StreamStart { regen }));

                    debug!(model = %model, "Stream started");
                }

                StreamEvent::Text { text } => {
                    // Flush any pending thinking before accumulating text.
                    flush_thinking(&mut thinking_buf, &mut content_blocks, &mut pending_signature);
                    text_buf.push_str(&text);

                    // Relay as StreamChunk with content_type "text".
                    let _ = self.push_tx.send(ServerMessage::StreamChunk(
                        StreamChunk {
                            text,
                            content_type: "text".into(),
                        },
                    ));
                }

                StreamEvent::Thinking { text } => {
                    // Flush any pending text before accumulating thinking.
                    flush_text(&mut text_buf, &mut content_blocks);
                    thinking_buf.push_str(&text);

                    // Relay as StreamChunk with content_type "thinking".
                    let _ = self.push_tx.send(ServerMessage::StreamChunk(
                        StreamChunk {
                            text,
                            content_type: "thinking".into(),
                        },
                    ));
                }

                StreamEvent::ThinkingSignature { signature } => {
                    // Buffer the signature to attach when the thinking block is flushed.
                    pending_signature = Some(signature);
                }

                StreamEvent::ToolUse { id, name, input } => {
                    // Flush pending buffers before tool_use block.
                    flush_text(&mut text_buf, &mut content_blocks);
                    flush_thinking(&mut thinking_buf, &mut content_blocks, &mut pending_signature);

                    content_blocks.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });

                    tool_uses.push(ToolUseEvent {
                        id,
                        name,
                        input,
                    });
                }

                StreamEvent::Done {
                    content,
                    finish_reason,
                    usage,
                    timing,
                } => {
                    // Flush any remaining buffers.
                    flush_text(&mut text_buf, &mut content_blocks);
                    flush_thinking(&mut thinking_buf, &mut content_blocks, &mut pending_signature);
                    let metadata = StreamMetadata {
                        tokens: TokenCounts {
                            input: usage.input_tokens,
                            output: usage.output_tokens,
                            cache_read: usage.cache_read_tokens,
                            cache_write: usage.cache_creation_tokens,
                        },
                        timing: TimingInfo {
                            total_ms: timing.total_ms,
                            ttft_ms: timing.time_to_first_token_ms,
                        },
                        model: model.clone(),
                    };

                    info!(
                        model = %model,
                        input_tokens = usage.input_tokens,
                        output_tokens = usage.output_tokens,
                        cache_read = usage.cache_read_tokens,
                        cache_write = usage.cache_creation_tokens,
                        total_ms = timing.total_ms,
                        ttft_ms = timing.time_to_first_token_ms,
                        "Stream completed"
                    );

                    // Broadcast stream_end to SWP clients.
                    let _ = self.push_tx.send(ServerMessage::StreamEnd(
                        StreamEnd {
                            content: content.clone(),
                            metadata,
                            finish_reason: finish_reason.clone(),
                        },
                    ));

                    // Check for cache invalidation (section 13.3).
                    check_cache_invalidation(
                        &self.push_tx,
                        cache_ctx,
                        &usage,
                    );

                    return Ok(StreamResult {
                        content,
                        model: if started {
                            model
                        } else {
                            String::new()
                        },
                        finish_reason,
                        usage,
                        timing,
                        tool_uses,
                        content_blocks,
                    });
                }
            }
        }
    }
}

/// Check for unexpected cache invalidation per section 13.3.
///
/// After each response, if `cache_read_tokens == 0` and the conversation has >1
/// turn and this is not the first message after compaction/restart, push a
/// `CacheWarning` event to connected clients and log as ERROR.
fn check_cache_invalidation(
    push_tx: &broadcast::Sender<ServerMessage>,
    ctx: &CacheContext,
    usage: &super::types::Usage,
) {
    if !ctx.cache_invalidation_warnings {
        return;
    }

    // Cache read of zero is expected in these cases:
    // 1. First turn of a conversation (nothing to cache)
    // 2. First message after daemon restart
    // 3. First message after compaction
    if ctx.conversation_turn_count <= 1 {
        return;
    }
    if ctx.is_first_after_restart {
        return;
    }
    if ctx.is_first_after_compaction {
        return;
    }

    if usage.cache_read_tokens == 0 {
        let expected = usage.input_tokens;
        let message = format!(
            "Unexpected cache invalidation: cache_read_tokens=0 with {} input tokens \
             on turn {}. The prompt cache may have been evicted.",
            expected, ctx.conversation_turn_count
        );

        error!("{}", message);

        let _ = push_tx.send(ServerMessage::CacheWarning(CacheWarning {
            expected_tokens: expected,
            message,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{UnixListener, UnixStream};

    /// Helper: set up a Unix socket pair and return (server_writer, client_reader).
    async fn setup_stream_pair() -> (
        tokio::io::WriteHalf<UnixStream>,
        BufReader<UnixStream>,
        broadcast::Sender<ServerMessage>,
        broadcast::Receiver<ServerMessage>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("stream-test.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();

        let client_stream = UnixStream::connect(&socket_path).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();

        let (_, server_writer) = tokio::io::split(server_stream);
        let client_reader = BufReader::new(client_stream);

        let (push_tx, push_rx) = broadcast::channel(64);

        // Keep tmp alive by leaking it (test-only).
        Box::leak(Box::new(tmp));

        (server_writer, client_reader, push_tx, push_rx)
    }

    #[tokio::test]
    async fn consume_simple_stream() {
        let (mut writer, mut reader, push_tx, mut push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);

        let ctx = CacheContext {
            conversation_turn_count: 1,
            is_first_after_restart: false,
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        };

        // Write stream events from "server".
        let events = [
            r#"{"type":"start","model":"claude-test"}"#,
            r#"{"type":"text","text":"Hello "}"#,
            r#"{"type":"text","text":"world"}"#,
            r#"{"type":"done","content":"Hello world","finish_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5,"cache_read_tokens":8,"cache_creation_tokens":2},"timing":{"total_ms":150,"time_to_first_token_ms":50}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        assert_eq!(result.content, "Hello world");
        assert_eq!(result.model, "claude-test");
        assert_eq!(result.finish_reason, "end_turn");
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 5);
        assert_eq!(result.usage.cache_read_tokens, 8);
        assert_eq!(result.timing.total_ms, 150);
        assert_eq!(result.timing.time_to_first_token_ms, 50);
        assert!(result.tool_uses.is_empty());

        // Verify broadcast messages.
        let msg1 = push_rx.try_recv().unwrap();
        assert!(matches!(msg1, ServerMessage::StreamStart(StreamStart { regen: false })));

        let msg2 = push_rx.try_recv().unwrap();
        match msg2 {
            ServerMessage::StreamChunk(chunk) => {
                assert_eq!(chunk.text, "Hello ");
                assert_eq!(chunk.content_type, "text");
            }
            other => panic!("Expected StreamChunk, got {:?}", other),
        }

        let msg3 = push_rx.try_recv().unwrap();
        match msg3 {
            ServerMessage::StreamChunk(chunk) => {
                assert_eq!(chunk.text, "world");
                assert_eq!(chunk.content_type, "text");
            }
            other => panic!("Expected StreamChunk, got {:?}", other),
        }

        let msg4 = push_rx.try_recv().unwrap();
        match msg4 {
            ServerMessage::StreamEnd(end) => {
                assert_eq!(end.content, "Hello world");
                assert_eq!(end.metadata.model, "claude-test");
                assert_eq!(end.metadata.tokens.input, 10);
                assert_eq!(end.metadata.tokens.cache_read, 8);
                assert_eq!(end.metadata.timing.ttft_ms, 50);
            }
            other => panic!("Expected StreamEnd, got {:?}", other),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn consume_stream_with_thinking_and_tools() {
        let (mut writer, mut reader, push_tx, mut push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);

        let ctx = CacheContext::default();

        let events = [
            r#"{"type":"start","model":"claude-test"}"#,
            r#"{"type":"thinking","text":"Let me think..."}"#,
            r#"{"type":"tool_use","id":"t1","name":"search","input":{"q":"test"}}"#,
            r#"{"type":"text","text":"Found it"}"#,
            r#"{"type":"done","content":"Found it","finish_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":10},"timing":{"total_ms":300}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        assert_eq!(result.content, "Found it");
        assert_eq!(result.tool_uses.len(), 1);
        assert_eq!(result.tool_uses[0].id, "t1");
        assert_eq!(result.tool_uses[0].name, "search");
        assert_eq!(result.tool_uses[0].input["q"], "test");

        // Verify content_blocks accumulated correctly.
        assert_eq!(result.content_blocks.len(), 3, "Should have thinking + tool_use + text blocks");
        assert!(matches!(&result.content_blocks[0], ContentBlock::Thinking { thinking, signature } if thinking == "Let me think..." && signature.is_none()));
        assert!(matches!(&result.content_blocks[1], ContentBlock::ToolUse { id, name, .. } if id == "t1" && name == "search"));
        assert!(matches!(&result.content_blocks[2], ContentBlock::Text { text } if text == "Found it"));

        // Verify thinking chunk was broadcast with correct content_type.
        let _ = push_rx.try_recv().unwrap(); // StreamStart
        let thinking_msg = push_rx.try_recv().unwrap();
        match thinking_msg {
            ServerMessage::StreamChunk(chunk) => {
                assert_eq!(chunk.text, "Let me think...");
                assert_eq!(chunk.content_type, "thinking");
            }
            other => panic!("Expected StreamChunk(thinking), got {:?}", other),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn consume_stream_with_thinking_signature() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        let events = [
            r#"{"type":"start","model":"claude-test"}"#,
            r#"{"type":"thinking","text":"Let me "}"#,
            r#"{"type":"thinking","text":"reason..."}"#,
            r#"{"type":"thinking_signature","signature":"sig_test_abc"}"#,
            r#"{"type":"text","text":"The answer"}"#,
            r#"{"type":"done","content":"The answer","finish_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":10},"timing":{"total_ms":300}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        assert_eq!(result.content_blocks.len(), 2);
        match &result.content_blocks[0] {
            ContentBlock::Thinking { thinking, signature } => {
                assert_eq!(thinking, "Let me reason...");
                assert_eq!(signature.as_deref(), Some("sig_test_abc"));
            }
            other => panic!("Expected Thinking with signature, got {:?}", other),
        }
        assert!(matches!(&result.content_blocks[1], ContentBlock::Text { text } if text == "The answer"));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn consume_regen_sets_flag() {
        let (mut writer, mut reader, push_tx, mut push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        let server_handle = tokio::spawn(async move {
            writer
                .write_all(b"{\"type\":\"start\",\"model\":\"test\"}\n")
                .await
                .unwrap();
            writer
                .write_all(b"{\"type\":\"done\",\"content\":\"ok\",\"finish_reason\":\"end_turn\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"timing\":{\"total_ms\":10}}\n")
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        });

        consumer
            .consume(&mut reader, true, &ctx)
            .await
            .unwrap();

        let msg = push_rx.try_recv().unwrap();
        match msg {
            ServerMessage::StreamStart(start) => assert!(start.regen),
            other => panic!("Expected StreamStart with regen=true, got {:?}", other),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn incomplete_stream_returns_error() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        // Server sends start but closes connection without "done".
        let server_handle = tokio::spawn(async move {
            writer
                .write_all(b"{\"type\":\"start\",\"model\":\"test\"}\n")
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        });

        let err = consumer
            .consume(&mut reader, false, &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::IncompleteStream));

        server_handle.await.unwrap();
    }

    // ── Content block accumulation tests ────────────────────────────────

    #[tokio::test]
    async fn content_blocks_merge_consecutive_text() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        let events = [
            r#"{"type":"start","model":"test"}"#,
            r#"{"type":"text","text":"Hello "}"#,
            r#"{"type":"text","text":"world"}"#,
            r#"{"type":"text","text":"!"}"#,
            r#"{"type":"done","content":"Hello world!","finish_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"timing":{"total_ms":10}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        // Consecutive text chunks should be merged into a single Text block.
        assert_eq!(result.content_blocks.len(), 1);
        assert!(matches!(&result.content_blocks[0], ContentBlock::Text { text } if text == "Hello world!"));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn content_blocks_merge_consecutive_thinking() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        let events = [
            r#"{"type":"start","model":"test"}"#,
            r#"{"type":"thinking","text":"First "}"#,
            r#"{"type":"thinking","text":"thought"}"#,
            r#"{"type":"text","text":"Answer"}"#,
            r#"{"type":"done","content":"Answer","finish_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"timing":{"total_ms":10}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        // Consecutive thinking chunks merged, then text block.
        assert_eq!(result.content_blocks.len(), 2);
        assert!(matches!(&result.content_blocks[0], ContentBlock::Thinking { thinking, signature } if thinking == "First thought" && signature.is_none()));
        assert!(matches!(&result.content_blocks[1], ContentBlock::Text { text } if text == "Answer"));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn content_blocks_type_change_flushes_buffer() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        // Interleaved: text → thinking → text
        let events = [
            r#"{"type":"start","model":"test"}"#,
            r#"{"type":"text","text":"pre-thought "}"#,
            r#"{"type":"thinking","text":"hmm..."}"#,
            r#"{"type":"text","text":"post-thought"}"#,
            r#"{"type":"done","content":"pre-thought post-thought","finish_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"timing":{"total_ms":10}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        // Type change should flush: text, thinking, text → 3 blocks.
        assert_eq!(result.content_blocks.len(), 3);
        assert!(matches!(&result.content_blocks[0], ContentBlock::Text { text } if text == "pre-thought "));
        assert!(matches!(&result.content_blocks[1], ContentBlock::Thinking { thinking, signature } if thinking == "hmm..." && signature.is_none()));
        assert!(matches!(&result.content_blocks[2], ContentBlock::Text { text } if text == "post-thought"));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn content_blocks_text_only_stream() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        let events = [
            r#"{"type":"start","model":"test"}"#,
            r#"{"type":"text","text":"Just text"}"#,
            r#"{"type":"done","content":"Just text","finish_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"timing":{"total_ms":10}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        assert_eq!(result.content_blocks.len(), 1);
        assert!(matches!(&result.content_blocks[0], ContentBlock::Text { text } if text == "Just text"));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn content_blocks_empty_on_no_content() {
        let (mut writer, mut reader, push_tx, _push_rx) =
            setup_stream_pair().await;
        let consumer = StreamConsumer::new(push_tx);
        let ctx = CacheContext::default();

        // Start → Done with empty content (edge case).
        let events = [
            r#"{"type":"start","model":"test"}"#,
            r#"{"type":"done","content":"","finish_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":0},"timing":{"total_ms":10}}"#,
        ];

        let server_handle = tokio::spawn(async move {
            for event in events {
                writer.write_all(event.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });

        let result = consumer.consume(&mut reader, false, &ctx).await.unwrap();

        assert!(result.content_blocks.is_empty());

        server_handle.await.unwrap();
    }

    // ── Cache invalidation tests ──────────────────────────────────────

    #[test]
    fn cache_invalidation_triggers_warning() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 5, // Multi-turn conversation.
            is_first_after_restart: false,
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        };

        let usage = super::super::types::Usage {
            input_tokens: 5000,
            output_tokens: 100,
            cache_read_tokens: 0, // Unexpected!
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);

        let msg = push_rx.try_recv().unwrap();
        match msg {
            ServerMessage::CacheWarning(warning) => {
                assert_eq!(warning.expected_tokens, 5000);
                assert!(warning.message.contains("cache_read_tokens=0"));
                assert!(warning.message.contains("turn 5"));
            }
            other => panic!("Expected CacheWarning, got {:?}", other),
        }
    }

    #[test]
    fn cache_invalidation_skips_first_turn() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 1, // First turn — no cache expected.
            is_first_after_restart: false,
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        };

        let usage = super::super::types::Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);

        // Should NOT have sent a warning.
        assert!(push_rx.try_recv().is_err());
    }

    #[test]
    fn cache_invalidation_skips_after_restart() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 10,
            is_first_after_restart: true, // First message after restart.
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        };

        let usage = super::super::types::Usage {
            input_tokens: 5000,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);
        assert!(push_rx.try_recv().is_err());
    }

    #[test]
    fn cache_invalidation_skips_after_compaction() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 10,
            is_first_after_restart: false,
            is_first_after_compaction: true,
            cache_invalidation_warnings: true,
        };

        let usage = super::super::types::Usage {
            input_tokens: 3000,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);
        assert!(push_rx.try_recv().is_err());
    }

    #[test]
    fn cache_invalidation_respects_config_disabled() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 5,
            is_first_after_restart: false,
            is_first_after_compaction: false,
            cache_invalidation_warnings: false, // Disabled!
        };

        let usage = super::super::types::Usage {
            input_tokens: 5000,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);
        assert!(push_rx.try_recv().is_err());
    }

    #[test]
    fn cache_invalidation_no_warning_when_cache_hit() {
        let (push_tx, mut push_rx) = broadcast::channel(16);

        let ctx = CacheContext {
            conversation_turn_count: 5,
            is_first_after_restart: false,
            is_first_after_compaction: false,
            cache_invalidation_warnings: true,
        };

        let usage = super::super::types::Usage {
            input_tokens: 5000,
            output_tokens: 100,
            cache_read_tokens: 4500, // Cache hit — no warning.
            cache_creation_tokens: 0,
        };

        check_cache_invalidation(&push_tx, &ctx, &usage);
        assert!(push_rx.try_recv().is_err());
    }
}
