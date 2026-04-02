pub mod client_config;
pub mod conn_manager;
pub mod connection;
pub mod discovery;
pub mod error;
pub mod image_protocol;
pub mod stream;

pub use client_config::{load_client_config, ClientConfig};
pub use conn_manager::{ConnCommand, ConnEvent, spawn_connection};
pub use image_protocol::{ImageProtocol, detect_protocol};
pub use connection::{SWPConnection, ServerAddr};
pub use discovery::{discover, discover_or_default};
pub use error::{ClientError, Result};
pub use stream::{StreamCallbacks, StreamHandler};

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use shore_protocol::client_msg::ClientMessage;
    use shore_protocol::server_msg::*;
    use shore_protocol::types::*;
    use shore_protocol::SWP_V1;

    use crate::connection::SWPConnection;
    use crate::stream::StreamHandler;

    /// Helper: write a JSON line to a writer.
    async fn write_json_line<W: tokio::io::AsyncWriteExt + Unpin, T: serde::Serialize>(
        w: &mut W,
        val: &T,
    ) {
        let line = serde_json::to_string(val).unwrap();
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    /// Helper: read one JSON line from a reader.
    async fn read_json_line<R: tokio::io::AsyncBufReadExt + Unpin, T: serde::de::DeserializeOwned>(
        r: &mut R,
    ) -> T {
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    // ── Handshake tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn handshake_success() {
        let (client_stream, server_stream) = duplex(8192);

        let server_handle = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server_stream);
            let mut reader = tokio::io::BufReader::new(r);

            // Server sends hello
            let server_hello = ServerMessage::Hello(ServerHello {
                v: SWP_V1,
                server_name: "test-daemon".into(),
                characters: vec![CharacterInfo {
                    name: "alice".into(),
                }],
            });
            write_json_line(&mut w, &server_hello).await;

            // Server reads client hello
            let client_hello: ClientMessage = read_json_line(&mut reader).await;
            match client_hello {
                ClientMessage::Hello(h) => {
                    assert_eq!(h.client_type, "tui");
                    assert_eq!(h.client_name, "test-client");
                    assert!(h.capabilities.contains(&"streaming".to_string()));
                }
                other => panic!("expected client hello, got: {other:?}"),
            }

            // Server sends history
            let history = ServerMessage::History(History {
                messages: vec![Message {
                    msg_id: "m1".into(),
                    role: Role::User,
                    content: "hello".into(),
                    images: vec![],
                    content_blocks: vec![],
                    alt_index: None,
                    alt_count: None,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                }],
                config: serde_json::json!({}),
            });
            write_json_line(&mut w, &history).await;
        });

        let (conn, server_hello, history) =
            SWPConnection::connect_raw(client_stream, "tui", "test-client", None)
                .await
                .unwrap();

        assert_eq!(server_hello.v, SWP_V1);
        assert_eq!(server_hello.server_name, "test-daemon");
        assert_eq!(server_hello.characters.len(), 1);
        assert_eq!(history.messages.len(), 1);
        assert_eq!(history.messages[0].content, "hello");

        drop(conn);
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_wrong_version() {
        let (client_stream, server_stream) = duplex(8192);

        tokio::spawn(async move {
            let (_r, mut w) = tokio::io::split(server_stream);
            let bad_hello = ServerMessage::Hello(ServerHello {
                v: 999,
                server_name: "bad".into(),
                characters: vec![],
            });
            write_json_line(&mut w, &bad_hello).await;
        });

        let result =
            SWPConnection::connect_raw(client_stream, "tui", "test", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("unsupported protocol version"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn handshake_unexpected_first_message() {
        let (client_stream, server_stream) = duplex(8192);

        tokio::spawn(async move {
            let (_r, mut w) = tokio::io::split(server_stream);
            let ping = ServerMessage::Ping(Ping {});
            write_json_line(&mut w, &ping).await;
        });

        let result =
            SWPConnection::connect_raw(client_stream, "tui", "test", None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("expected server hello"),
            "got: {err}"
        );
    }

    // ── Send/receive tests ───────────────────────────────────────────

    #[tokio::test]
    async fn send_and_receive_round_trip() {
        let (client_stream, server_stream) = duplex(8192);

        let server_handle = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server_stream);
            let mut reader = tokio::io::BufReader::new(r);

            // Read client message
            let msg: ClientMessage = read_json_line(&mut reader).await;
            match msg {
                ClientMessage::Message(m) => {
                    assert_eq!(m.text, "test message");
                    assert!(m.stream);
                }
                other => panic!("expected message, got: {other:?}"),
            }

            // Send a ping back
            let pong = ServerMessage::Ping(Ping {});
            write_json_line(&mut w, &pong).await;
        });

        let mut conn = SWPConnection::from_raw_stream(client_stream);
        let rid = conn.send_message("test message", true).await.unwrap();
        assert!(rid.is_some());

        let reply = conn.recv().await.unwrap();
        assert!(matches!(reply, ServerMessage::Ping(_)));

        drop(conn);
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_regen_and_command() {
        let (client_stream, server_stream) = duplex(8192);

        let server_handle = tokio::spawn(async move {
            let (r, _w) = tokio::io::split(server_stream);
            let mut reader = tokio::io::BufReader::new(r);

            // Read regen
            let msg: ClientMessage = read_json_line(&mut reader).await;
            assert!(matches!(msg, ClientMessage::Regen(_)));

            // Read command
            let msg: ClientMessage = read_json_line(&mut reader).await;
            match msg {
                ClientMessage::Command(c) => {
                    assert_eq!(c.name, "switch_character");
                }
                other => panic!("expected command, got: {other:?}"),
            }
        });

        let mut conn = SWPConnection::from_raw_stream(client_stream);
        conn.send_regen(true, None).await.unwrap();
        conn.send_command("switch_character", serde_json::json!({"name": "alice"}))
            .await
            .unwrap();

        drop(conn);
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn recv_on_eof_returns_disconnected() {
        let (client_stream, server_stream) = duplex(8192);
        drop(server_stream); // close immediately

        let mut conn = SWPConnection::from_raw_stream(client_stream);
        let result = conn.recv().await;
        assert!(result.is_err());
        assert!(
            format!("{}", result.unwrap_err()).contains("disconnected"),
            "expected disconnected error"
        );
    }

    // ── StreamHandler tests ──────────────────────────────────────────

    #[test]
    fn stream_handler_assembles_chunks() {
        let mut handler = StreamHandler::new();

        let start = ServerMessage::StreamStart(StreamStart { regen: false });
        assert!(handler.feed(&start, None).unwrap());
        assert!(handler.is_active());
        assert!(!handler.is_regen());

        let chunk1 = ServerMessage::StreamChunk(StreamChunk {
            text: "Hello ".into(),
            content_type: "text".into(),
        });
        assert!(handler.feed(&chunk1, None).unwrap());

        let chunk2 = ServerMessage::StreamChunk(StreamChunk {
            text: "world!".into(),
            content_type: "text".into(),
        });
        assert!(handler.feed(&chunk2, None).unwrap());

        assert_eq!(handler.assembled_text(), "Hello world!");

        let end = ServerMessage::StreamEnd(StreamEnd {
            content: "Hello world!".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 100,
                    ttft_ms: 20,
                },
                model: "test-model".into(),
            },
            finish_reason: "end_turn".into(),
        });
        assert!(handler.feed(&end, None).unwrap());
        assert!(!handler.is_active());
        assert_eq!(handler.final_content(), Some("Hello world!"));
        assert_eq!(handler.metadata().unwrap().model, "test-model");
    }

    #[test]
    fn stream_handler_regen_flag() {
        let mut handler = StreamHandler::new();

        let start = ServerMessage::StreamStart(StreamStart { regen: true });
        handler.feed(&start, None).unwrap();
        assert!(handler.is_regen());
    }

    #[test]
    fn stream_handler_ignores_non_stream_messages() {
        let mut handler = StreamHandler::new();
        let ping = ServerMessage::Ping(Ping {});
        assert!(!handler.feed(&ping, None).unwrap());
    }

    #[test]
    fn stream_handler_error_on_chunk_without_start() {
        let mut handler = StreamHandler::new();
        let chunk = ServerMessage::StreamChunk(StreamChunk {
            text: "oops".into(),
            content_type: "text".into(),
        });
        let result = handler.feed(&chunk, None);
        assert!(result.is_err());
    }

    #[test]
    fn stream_handler_error_on_end_without_start() {
        let mut handler = StreamHandler::new();
        let end = ServerMessage::StreamEnd(StreamEnd {
            content: "".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 0,
                    ttft_ms: 0,
                },
                model: "".into(),
            },
            finish_reason: "end_turn".into(),
        });
        let result = handler.feed(&end, None);
        assert!(result.is_err());
    }

    #[test]
    fn stream_handler_error_on_double_start() {
        let mut handler = StreamHandler::new();
        let start = ServerMessage::StreamStart(StreamStart { regen: false });
        handler.feed(&start, None).unwrap();

        let start2 = ServerMessage::StreamStart(StreamStart { regen: false });
        let result = handler.feed(&start2, None);
        assert!(result.is_err());
    }

    #[test]
    fn stream_handler_callbacks_invoked() {
        use std::sync::{Arc, Mutex};

        struct TestCallbacks {
            started: Arc<Mutex<bool>>,
            chunk_count: Arc<Mutex<u32>>,
            ended: Arc<Mutex<bool>>,
        }

        impl crate::stream::StreamCallbacks for TestCallbacks {
            fn on_start(&mut self, _start: &StreamStart) {
                *self.started.lock().unwrap() = true;
            }
            fn on_chunk(&mut self, _chunk: &StreamChunk) {
                *self.chunk_count.lock().unwrap() += 1;
            }
            fn on_end(&mut self, _end: &StreamEnd) {
                *self.ended.lock().unwrap() = true;
            }
        }

        let started = Arc::new(Mutex::new(false));
        let chunk_count = Arc::new(Mutex::new(0u32));
        let ended = Arc::new(Mutex::new(false));

        let mut cb = TestCallbacks {
            started: started.clone(),
            chunk_count: chunk_count.clone(),
            ended: ended.clone(),
        };

        let mut handler = StreamHandler::new();

        let start = ServerMessage::StreamStart(StreamStart { regen: false });
        handler.feed(&start, Some(&mut cb)).unwrap();
        assert!(*started.lock().unwrap());

        let chunk = ServerMessage::StreamChunk(StreamChunk {
            text: "hi".into(),
            content_type: "text".into(),
        });
        handler.feed(&chunk, Some(&mut cb)).unwrap();
        handler.feed(&chunk, Some(&mut cb)).unwrap();
        assert_eq!(*chunk_count.lock().unwrap(), 2);

        let end = ServerMessage::StreamEnd(StreamEnd {
            content: "hihi".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 10,
                    ttft_ms: 5,
                },
                model: "m".into(),
            },
            finish_reason: "end_turn".into(),
        });
        handler.feed(&end, Some(&mut cb)).unwrap();
        assert!(*ended.lock().unwrap());
    }

    #[test]
    fn stream_handler_reset_allows_reuse() {
        let mut handler = StreamHandler::new();

        // First stream
        let start = ServerMessage::StreamStart(StreamStart { regen: false });
        handler.feed(&start, None).unwrap();
        let chunk = ServerMessage::StreamChunk(StreamChunk {
            text: "first".into(),
            content_type: "text".into(),
        });
        handler.feed(&chunk, None).unwrap();
        let end = ServerMessage::StreamEnd(StreamEnd {
            content: "first".into(),
            metadata: StreamMetadata {
                tokens: TokenCounts {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                },
                timing: TimingInfo {
                    total_ms: 10,
                    ttft_ms: 5,
                },
                model: "m".into(),
            },
            finish_reason: "end_turn".into(),
        });
        handler.feed(&end, None).unwrap();

        // Second stream — feed automatically resets on stream_start
        let start2 = ServerMessage::StreamStart(StreamStart { regen: true });
        handler.feed(&start2, None).unwrap();
        assert!(handler.is_active());
        assert!(handler.is_regen());
        assert_eq!(handler.assembled_text(), "");
    }

    // ── Discovery tests ──────────────────────────────────────────────

    #[test]
    fn discovery_instances_path_uses_xdg() {
        // Save and restore env
        let orig = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/test-xdg");
        let path = crate::discovery::instances_path();
        assert_eq!(
            path.to_str().unwrap(),
            "/tmp/test-xdg/shore/instances.json"
        );
        // Restore
        match orig {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    #[test]
    fn is_unix_path_detection() {
        assert!(crate::connection::is_unix_path("/tmp/shore.sock"));
        assert!(crate::connection::is_unix_path("/run/user/1000/shore/shore.sock"));
        assert!(crate::connection::is_unix_path("./shore.sock"));
        assert!(!crate::connection::is_unix_path("localhost:8080"));
        assert!(!crate::connection::is_unix_path("127.0.0.1:9090"));
    }

    // ── ClientConfig tests ──────────────────────────────────────────

    #[test]
    fn client_config_parses_tcp_address() {
        let toml = r#"default_address = "192.168.1.50:7320""#;
        let cfg: crate::client_config::ClientConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.default_address.as_deref(), Some("192.168.1.50:7320"));
    }

    #[test]
    fn client_config_parses_unix_address() {
        let toml = r#"default_address = "/tmp/shore.sock""#;
        let cfg: crate::client_config::ClientConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.default_address.as_deref(), Some("/tmp/shore.sock"));
    }

    #[test]
    fn client_config_empty_file() {
        let cfg: crate::client_config::ClientConfig = toml::from_str("").unwrap();
        assert!(cfg.default_address.is_none());
    }

    #[test]
    fn client_config_rejects_unknown_fields() {
        let toml = r#"unknown_field = "oops""#;
        let result = toml::from_str::<crate::client_config::ClientConfig>(toml);
        assert!(result.is_err());
    }
}
