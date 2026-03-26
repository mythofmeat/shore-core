pub mod client_msg;
pub mod error;
pub mod server_msg;
pub mod types;

/// SWP protocol version.
pub const SWP_V1: u32 = 1;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::client_msg::*;
    use crate::error::*;
    use crate::server_msg::*;
    use crate::types::*;
    use crate::SWP_V1;

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
            absence_seconds: None,
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
            messages: vec![Message {
                msg_id: "m1".into(),
                role: Role::User,
                content: "hi".into(),
                images: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
            }],
            config: json!({}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "history");
        assert_eq!(json["messages"][0]["role"], "user");
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
            name: "status".into(),
            data: json!({"ok": true}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "command_output");
        assert_eq!(json["name"], "status");
    }

    #[test]
    fn server_error_round_trip() {
        let msg = ServerMessage::Error(Error {
            code: ErrorCode::Busy,
            message: "engine busy".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "busy");
    }

    #[test]
    fn server_stream_start_round_trip() {
        let msg = ServerMessage::StreamStart(StreamStart { regen: false });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_start");
        assert_eq!(json["regen"], false);
    }

    #[test]
    fn server_stream_chunk_round_trip() {
        let msg = ServerMessage::StreamChunk(StreamChunk {
            text: "partial".into(),
            content_type: "text".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_chunk");
        assert_eq!(json["content_type"], "text");
    }

    #[test]
    fn server_stream_chunk_thinking() {
        let msg = ServerMessage::StreamChunk(StreamChunk {
            text: "hmm...".into(),
            content_type: "thinking".into(),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["content_type"], "thinking");
    }

    #[test]
    fn server_stream_end_round_trip() {
        let msg = ServerMessage::StreamEnd(StreamEnd {
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
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "stream_end");
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
                phase: phase_val.to_string(),
                model: Some("test-model".into()),
            });
            let (json, _back) = round_trip(&msg);
            assert_eq!(json["type"], "phase");
            assert_eq!(json["phase"], *phase_val);
        }
    }

    #[test]
    fn server_new_message_round_trip() {
        let msg = ServerMessage::NewMessage(NewMessage {
            message: Message {
                msg_id: "m2".into(),
                role: Role::Assistant,
                content: "autonomous msg".into(),
                images: vec![],
                alt_index: None,
                alt_count: None,
                timestamp: "2026-01-01T00:00:01Z".into(),
            },
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "new_message");
        assert_eq!(json["msg_id"], "m2");
    }

    #[test]
    fn server_tool_call_round_trip() {
        let msg = ServerMessage::ToolCall(ToolCall {
            tool_id: "t1".into(),
            tool_name: "search".into(),
            input: json!({"query": "rust serde"}),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["input"]["query"], "rust serde");
        // Verify input is a JSON object, not a string
        assert!(json["input"].is_object());
    }

    #[test]
    fn server_tool_result_round_trip() {
        let msg = ServerMessage::ToolResult(ToolResult {
            tool_id: "t1".into(),
            tool_name: "search".into(),
            output: "found 5 results".into(),
            is_error: false,
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "tool_result");
    }

    #[test]
    fn server_send_image_round_trip() {
        let msg = ServerMessage::SendImage(SendImage {
            path: "/tmp/img.png".into(),
            caption: Some("generated chart".into()),
        });
        let (json, _back) = round_trip(&msg);
        assert_eq!(json["type"], "send_image");
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
            }],
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
}
