pub mod client_msg;
pub mod error;
pub mod merge;
pub mod server_msg;
pub mod types;

/// SWP protocol version.
pub const SWP_V1: u32 = 1;

/// Maximum newline-delimited SWP frame size in bytes.
pub const MAX_WIRE_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::client_msg::*;
    use crate::error::*;
    use crate::server_msg::*;
    use crate::types::*;
    use crate::{MAX_WIRE_MESSAGE_SIZE, SWP_V1};

    /// Helper: serialize then deserialize, return the intermediate JSON.
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug>(
        val: &T,
    ) -> (serde_json::Value, T) {
        let json = serde_json::to_value(val).expect("serialize");
        let back: T = serde_json::from_value(json.clone()).expect("deserialize");
        (json, back)
    }

    // ── Protocol version ──────────────────────────────────────────────

    #[test]
    fn protocol_version_constant() {
        assert_eq!(SWP_V1, 1);
    }

    #[test]
    fn wire_message_size_constant() {
        assert_eq!(MAX_WIRE_MESSAGE_SIZE, 16 * 1024 * 1024);
    }

    // ── Client messages ───────────────────────────────────────────────

    #[test]
    fn client_hello_round_trip() {
        let msg = ClientMessage::Hello(ClientHello {
            client_type: "tui".into(),
            client_name: "shore-tui".into(),
            capabilities: vec!["streaming".into()],
            character: None,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "hello");
        assert_eq!(json["client_type"], "tui");
    }

    #[test]
    fn client_message_round_trip() {
        let msg = ClientMessage::Message(ClientMessageBody {
            rid: Some("msg_01".into()),
            text: "Hello world".into(),
            stream: true,
            images: vec![],
            image_data: vec![],
            absence_seconds: None,
            overrides: None,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "message");
        assert_eq!(json["text"], "Hello world");
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn client_regen_round_trip() {
        let msg = ClientMessage::Regen(Regen {
            rid: Some("regen_01".into()),
            stream: true,
            guidance: None,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "regen");
    }

    #[test]
    fn client_command_round_trip() {
        let msg = ClientMessage::Command(Command {
            rid: Some("cmd_01".into()),
            name: "switch_character".into(),
            args: json!({"name": "alice"}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "command");
        assert_eq!(json["name"], "switch_character");
        assert_eq!(json["args"]["name"], "alice");
    }

    // ── Server messages ───────────────────────────────────────────────

    #[test]
    fn server_hello_round_trip() {
        let msg = ServerMessage::Hello(ServerHello {
            v: SWP_V1,
            server_name: "shore-daemon".into(),
            characters: vec![CharacterInfo {
                name: "alice".into(),
            }],
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "hello");
        assert_eq!(json["v"], 1);
    }

    #[test]
    fn server_history_round_trip() {
        let msg = ServerMessage::History(History {
            rid: None,
            messages: vec![Message {
                msg_id: "m1".into(),
                role: Role::User,
                content: "hi".into(),
                images: vec![],
                content_blocks: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
            }],
            config: json!({}),
            selected_character: Some("alice".into()),
            revision: 7,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "history");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["selected_character"], "alice");
        assert_eq!(json["revision"], 7);
    }

    #[test]
    fn server_request_history_round_trip() {
        let msg = ServerMessage::History(History {
            rid: Some("cmd_switch_01".into()),
            messages: vec![],
            config: json!({}),
            selected_character: Some("alice".into()),
            revision: 8,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "history");
        assert_eq!(json["rid"], "cmd_switch_01");
        assert_eq!(json["revision"], 8);
    }

    #[test]
    fn server_shutdown_round_trip() {
        let msg = ServerMessage::Shutdown(Shutdown {});
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "shutdown");
    }

    #[test]
    fn server_ping_round_trip() {
        let msg = ServerMessage::Ping(Ping {});
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "ping");
    }

    #[test]
    fn server_command_output_round_trip() {
        let msg = ServerMessage::CommandOutput(CommandOutput {
            rid: Some("cmd_01".into()),
            name: "status".into(),
            data: json!({"ok": true}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "command_output");
        assert_eq!(json["rid"], "cmd_01");
        assert_eq!(json["name"], "status");
    }

    #[test]
    fn server_error_round_trip() {
        let msg = ServerMessage::Error(Error {
            rid: Some("msg_01".into()),
            code: ErrorCode::Busy,
            message: "engine busy".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "error");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["code"], "busy");
    }

    #[test]
    fn server_stream_start_round_trip() {
        let msg = ServerMessage::StreamStart(StreamStart {
            rid: Some("msg_01".into()),
            regen: false,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_start");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["regen"], false);
    }

    #[test]
    fn server_stream_chunk_round_trip() {
        let msg = ServerMessage::StreamChunk(StreamChunk {
            rid: Some("msg_01".into()),
            text: "partial".into(),
            content_type: "text".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_chunk");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["content_type"], "text");
    }

    #[test]
    fn server_stream_chunk_thinking() {
        let msg = ServerMessage::StreamChunk(StreamChunk {
            rid: Some("msg_01".into()),
            text: "hmm...".into(),
            content_type: "thinking".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["content_type"], "thinking");
    }

    #[test]
    fn server_stream_end_round_trip() {
        let msg = ServerMessage::StreamEnd(StreamEnd {
            rid: Some("msg_01".into()),
            content: "full response".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 1234,
                    output: 567,
                    cache_read: 890,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 2340,
                    ttft_ms: 450,
                },
                model: "claude-haiku-4-5-20251001".into(),
            },
            finish_reason: "end_turn".into(),
            is_final: true,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_end");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["metadata"]["tokens"]["input"], 1234);
        assert_eq!(json["metadata"]["tokens"]["cache_read"], 890);
        assert_eq!(json["metadata"]["timing"]["total_ms"], 2340);
        assert_eq!(json["metadata"]["timing"]["ttft_ms"], 450);
        assert_eq!(json["metadata"]["model"], "claude-haiku-4-5-20251001");
    }

    #[test]
    fn server_phase_round_trip() {
        for phase_val in &["thinking", "text_generation", "tool_use"] {
            let msg = ServerMessage::Phase(Phase {
                rid: Some("msg_01".into()),
                phase: phase_val.to_string(),
                model: Some("test-model".into()),
            });
            let (json, _back) = round_trip(&msg);
            assert_eq!(json["type"], "phase");
            assert_eq!(json["rid"], "msg_01");
            assert_eq!(json["phase"], *phase_val);
        }
    }

    #[test]
    fn server_new_message_round_trip() {
        let msg = ServerMessage::NewMessage(NewMessage {
            revision: 3,
            message: Message {
                msg_id: "m2".into(),
                role: Role::Assistant,
                content: "autonomous msg".into(),
                images: vec![],
                content_blocks: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:01Z".into(),
            },
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "new_message");
        assert_eq!(json["msg_id"], "m2");
        assert_eq!(json["revision"], 3);
    }

    #[test]
    fn server_tool_call_round_trip() {
        let msg = ServerMessage::ToolCall(ToolCall {
            rid: Some("msg_01".into()),
            tool_id: "t1".into(),
            tool_name: "search".into(),
            input: json!({"query": "rust serde"}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["input"]["query"], "rust serde");
        // Verify input is a JSON object, not a string
        assert!(json["input"].is_object());
    }

    #[test]
    fn server_tool_result_round_trip() {
        let msg = ServerMessage::ToolResult(ToolResult {
            rid: Some("msg_01".into()),
            tool_id: "t1".into(),
            tool_name: "search".into(),
            output: "found 5 results".into(),
            is_error: false,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["rid"], "msg_01");
    }

    #[test]
    fn server_send_image_round_trip() {
        let msg = ServerMessage::SendImage(SendImage {
            rid: Some("msg_01".into()),
            path: "/tmp/img.png".into(),
            caption: Some("generated chart".into()),
            data: None,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "send_image");
        assert_eq!(json["rid"], "msg_01");
        assert_eq!(json["path"], "/tmp/img.png");
        assert_eq!(json["caption"], "generated chart");
    }

    #[test]
    fn server_cache_warning_round_trip() {
        let msg = ServerMessage::CacheWarning(CacheWarning {
            expected_tokens: 5000,
            message: "cache miss".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "cache_warning");
        assert_eq!(json["expected_tokens"], 5000);
    }

    // ── Types ─────────────────────────────────────────────────────────

    #[test]
    fn message_with_all_fields() {
        let msg = Message {
            msg_id: "m3".into(),
            role: Role::Assistant,
            content: "response".into(),
            images: vec![ImageRef {
                path: "/img/a.png".into(),
                caption: Some("photo".into()),
                data: None,
            }],
            content_blocks: vec![],
            alt_index: Some(1),
            alt_count: Some(3),
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let (json, back) = round_trip(&msg);
        assert_eq!(json["alt_index"], 1);
        assert_eq!(json["alt_count"], 3);
        assert_eq!(json["images"][0]["path"], "/img/a.png");
        assert_eq!(back.alt_index, Some(1));
        assert_eq!(back.alt_count, Some(3));
    }

    #[test]
    fn message_without_alts_omits_fields() {
        let msg = Message {
            msg_id: "m4".into(),
            role: Role::User,
            content: "hi".into(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert!(json.get("alt_index").is_none());
        assert!(json.get("alt_count").is_none());
    }

    #[test]
    fn stream_metadata_nested_structure() {
        let meta = StreamMetadata {
            tokens: TokenCounts {
                input: 100,
                output: 50,
                cache_read: 0,
                cache_write: 0,
            },
            timing: TimingInfo {
                total_ms: 1000,
                ttft_ms: 200,
            },
            model: "test".into(),
        };
        let (json, _back) = round_trip(&meta);
        assert!(json["tokens"].is_object());
        assert!(json["timing"].is_object());
        assert_eq!(json["tokens"]["input"], 100);
        assert_eq!(json["timing"]["ttft_ms"], 200);
    }

    #[test]
    fn error_code_all_variants() {
        let codes = [
            ErrorCode::ProtocolError,
            ErrorCode::InvalidRequest,
            ErrorCode::NotFound,
            ErrorCode::Busy,
            ErrorCode::ProviderError,
            ErrorCode::Timeout,
            ErrorCode::InternalError,
        ];
        let expected = [
            "protocol_error",
            "invalid_request",
            "not_found",
            "busy",
            "provider_error",
            "timeout",
            "internal_error",
        ];
        for (code, exp) in codes.iter().zip(expected.iter()) {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json.as_str().unwrap(), *exp);
        }
    }

    #[test]
    fn character_info_round_trip() {
        let info = CharacterInfo {
            name: "alice".into(),
        };
        let (_json, back) = round_trip(&info);
        assert_eq!(back.name, "alice");
    }

    #[test]
    fn role_serialization() {
        assert_eq!(serde_json::to_value(Role::User).unwrap(), "user");
        assert_eq!(serde_json::to_value(Role::Assistant).unwrap(), "assistant");
        assert_eq!(serde_json::to_value(Role::System).unwrap(), "system");
    }

    // ── ContentBlock serde round-trip ─────────────────────────────────

    #[test]
    fn content_block_text_round_trip() {
        let block = ContentBlock::Text {
            text: "hello world".into(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello world");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_thinking_round_trip() {
        let block = ContentBlock::Thinking {
            thinking: "Let me consider...".into(),
            signature: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["thinking"], "Let me consider...");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_thinking_with_signature_round_trip() {
        let block = ContentBlock::Thinking {
            thinking: "Let me consider...".into(),
            signature: Some("sig_abc123".into()),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["thinking"], "Let me consider...");
        assert_eq!(json["signature"], "sig_abc123");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_redacted_thinking_round_trip() {
        let block = ContentBlock::RedactedThinking {
            data: "opaque_data_abc".into(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "redacted_thinking");
        assert_eq!(json["data"], "opaque_data_abc");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_thinking_without_signature_compat() {
        // Simulate old JSON without signature field — should deserialize with None.
        let json = json!({"type": "thinking", "thinking": "old block"});
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        match block {
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                assert_eq!(thinking, "old block");
                assert!(signature.is_none());
            }
            _ => panic!("Expected Thinking"),
        }
    }

    #[test]
    fn content_block_tool_use_round_trip() {
        let block = ContentBlock::ToolUse {
            id: "tu_123".into(),
            name: "check_time".into(),
            input: json!({"timezone": "UTC"}),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "tu_123");
        assert_eq!(json["name"], "check_time");
        assert_eq!(json["input"]["timezone"], "UTC");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_tool_result_round_trip() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu_123".into(),
            content: "2026-03-27T12:00:00Z".into(),
            is_error: false,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "tu_123");
        assert_eq!(json["content"], "2026-03-27T12:00:00Z");
        // is_error defaults to false, verify it round-trips
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_tool_result_with_error() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu_456".into(),
            content: "Tool not found".into(),
            is_error: true,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["is_error"], true);
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_tool_result_is_error_defaults_false() {
        // Simulate old JSON without is_error field
        let json = json!({"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"});
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        match block {
            ContentBlock::ToolResult { is_error, .. } => assert!(!is_error),
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn message_with_content_blocks_round_trip() {
        let msg = Message {
            msg_id: "m_test".into(),
            role: Role::Assistant,
            content: "The time is noon.".into(),
            images: vec![],
            content_blocks: vec![
                ContentBlock::Thinking {
                    thinking: "User wants the time.".into(),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "check_time".into(),
                    input: json!({}),
                },
                ContentBlock::Text {
                    text: "The time is noon.".into(),
                },
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        // content_blocks should be present in serialized form
        let blocks = json["content_blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[2]["type"], "text");
        // Round-trip
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back.content_blocks.len(), 3);
        assert_eq!(back.content_blocks, msg.content_blocks);
    }

    #[test]
    fn message_always_includes_content_blocks() {
        let msg = Message {
            msg_id: "m_old".into(),
            role: Role::User,
            content: "hello".into(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert!(
            json.get("content_blocks").is_some(),
            "content_blocks should always be serialized"
        );
    }

    #[test]
    fn old_message_json_without_content_blocks_deserializes() {
        // Simulate V1/old JSONL that has no content_blocks field
        let json = json!({
            "msg_id": "m_legacy",
            "role": "assistant",
            "content": "old message",
            "timestamp": "2025-01-01T00:00:00Z"
        });
        let msg: Message = serde_json::from_value(json).unwrap();
        assert!(msg.content_blocks.is_empty());
        assert_eq!(msg.content, "old message");
    }
}
