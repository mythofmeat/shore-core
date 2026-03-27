pub mod registry;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use shore_protocol::client_msg::{ClientMessage, Command};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error, History, ServerHello, ServerMessage, Shutdown};
use shore_protocol::types::CharacterInfo;
use shore_protocol::SWP_V1;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, instrument, warn};

/// Maximum SWP message size (16 MB).
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Connected client metadata.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: u64,
    pub client_type: String,
    pub client_name: String,
    pub capabilities: Vec<String>,
    /// Which character this client is talking to.
    pub character: Option<String>,
}

/// Messages the server routes internally after handshake.
#[derive(Debug, Clone)]
pub enum RoutedMessage {
    /// Message or Regen — route to engine.
    Engine {
        msg: ClientMessage,
        character: Option<String>,
    },
    /// Command — route to command dispatcher.
    Command {
        cmd: Command,
        character: Option<String>,
    },
}

/// Configuration for the server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub tcp_addr: Option<String>,
    pub server_name: String,
}

/// The SWP server.
///
/// Listens on a Unix socket and optionally on TCP. Accepts concurrent client
/// connections, performs the SWP handshake, routes incoming messages, and
/// broadcasts push messages to all connected clients.
pub struct Server {
    config: ServerConfig,
    clients: Arc<RwLock<HashMap<u64, ClientInfo>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    push_tx: broadcast::Sender<ServerMessage>,
    /// Receiver for routed messages (engine / command dispatcher consumes these).
    route_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<RoutedMessage>>>,
    route_tx: tokio::sync::mpsc::Sender<RoutedMessage>,
}

impl Server {
    /// Create a new server with the given config and broadcast capacity.
    pub fn new(config: ServerConfig) -> Self {
        let (push_tx, _) = broadcast::channel(256);
        let (route_tx, route_rx) = tokio::sync::mpsc::channel(256);
        Self {
            config,
            clients: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            push_tx,
            route_rx: Arc::new(tokio::sync::Mutex::new(route_rx)),
            route_tx,
        }
    }

    /// Returns a clone of the broadcast sender for push messages.
    pub fn push_sender(&self) -> broadcast::Sender<ServerMessage> {
        self.push_tx.clone()
    }

    /// Returns the routed-message receiver (engine / command dispatcher).
    pub fn take_route_rx(
        &self,
    ) -> Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<RoutedMessage>>> {
        self.route_rx.clone()
    }

    /// Returns a read-only handle to the connected-clients map.
    pub fn clients(&self) -> Arc<RwLock<HashMap<u64, ClientInfo>>> {
        self.clients.clone()
    }

    /// Run the server. Listens on Unix socket (and optionally TCP) forever.
    #[instrument(skip(self), fields(server_name = %self.config.server_name))]
    pub async fn run(&self, shutdown: tokio::sync::watch::Receiver<()>) -> std::io::Result<()> {
        // Ensure parent directory exists for Unix socket.
        if let Some(parent) = self.config.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Remove stale socket file.
        let _ = tokio::fs::remove_file(&self.config.socket_path).await;

        let unix_listener = UnixListener::bind(&self.config.socket_path)?;
        info!(path = %self.config.socket_path.display(), "Unix socket listening");

        let tcp_listener = if let Some(ref addr) = self.config.tcp_addr {
            let listener = TcpListener::bind(addr).await?;
            info!(%addr, "TCP listening");
            Some(listener)
        } else {
            None
        };

        loop {
            tokio::select! {
                // Accept Unix connections.
                result = unix_listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            let (reader, writer) = stream.into_split();
                            self.spawn_client(reader, writer, shutdown.clone());
                        }
                        Err(e) => error!(error = %e, "Unix accept error"),
                    }
                }

                // Accept TCP connections (if enabled).
                result = async {
                    match tcp_listener.as_ref() {
                        Some(l) => l.accept().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok((stream, addr)) => {
                            info!(%addr, "TCP client connected");
                            let (reader, writer) = stream.into_split();
                            self.spawn_client(reader, writer, shutdown.clone());
                        }
                        Err(e) => error!(error = %e, "TCP accept error"),
                    }
                }

                // Shutdown signal.
                _ = {
                    let mut rx = shutdown.clone();
                    async move { rx.changed().await }
                } => {
                    info!("Server shutting down");
                    self.broadcast(ServerMessage::Shutdown(Shutdown {}));
                    break;
                }
            }
        }

        // Clean up Unix socket.
        let _ = tokio::fs::remove_file(&self.config.socket_path).await;
        Ok(())
    }

    /// Broadcast a push message to all connected clients.
    pub fn broadcast(&self, msg: ServerMessage) {
        // Ignore send errors — they just mean no receivers are listening.
        let _ = self.push_tx.send(msg);
    }

    /// Spawn a tokio task to handle one client connection.
    fn spawn_client<R, W>(&self, reader: R, writer: W, shutdown: tokio::sync::watch::Receiver<()>)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let client_id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let clients = self.clients.clone();
        let push_rx = self.push_tx.subscribe();
        let route_tx = self.route_tx.clone();
        let server_name = self.config.server_name.clone();

        tokio::spawn(async move {
            let ctx = ClientCtx {
                client_id,
                clients,
                push_rx,
                route_tx,
                server_name,
                shutdown,
            };
            if let Err(e) = handle_client(reader, writer, ctx).await {
                warn!(client_id, error = %e, "Client handler error");
            }
        });
    }
}

/// Per-client shared context passed to the handler.
struct ClientCtx {
    client_id: u64,
    clients: Arc<RwLock<HashMap<u64, ClientInfo>>>,
    push_rx: broadcast::Receiver<ServerMessage>,
    route_tx: tokio::sync::mpsc::Sender<RoutedMessage>,
    server_name: String,
    shutdown: tokio::sync::watch::Receiver<()>,
}

/// Handle a single client connection: handshake → message loop.
#[instrument(skip(reader, writer, ctx), fields(client_id = ctx.client_id))]
async fn handle_client<R, W>(
    reader: R,
    writer: W,
    mut ctx: ClientCtx,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    let client_id = ctx.client_id;
    let mut buf_reader = BufReader::new(reader);
    let mut writer = writer;

    // ── Step 1: Send server Hello ────────────────────────────────────
    let server_hello = ServerMessage::Hello(ServerHello {
        v: SWP_V1,
        server_name: ctx.server_name.clone(),
        characters: vec![CharacterInfo {
            name: "default".into(),
        }],
    });
    write_message(&mut writer, &server_hello).await?;
    info!(client_id, "Sent server hello");

    // ── Step 2: Receive client Hello ─────────────────────────────────
    let client_hello = read_message(&mut buf_reader).await?;
    let client_info = match client_hello {
        Some(ClientMessage::Hello(hello)) => {
            let info = ClientInfo {
                id: client_id,
                client_type: hello.client_type,
                client_name: hello.client_name,
                capabilities: hello.capabilities,
                character: hello.character,
            };
            info!(
                client_id,
                client_type = %info.client_type,
                client_name = %info.client_name,
                character = ?info.character,
                "Client hello received"
            );
            info
        }
        Some(other) => {
            let err = ServerMessage::Error(Error {
                code: ErrorCode::ProtocolError,
                message: format!("Expected hello, got {:?}", msg_type_name(&other)),
            });
            write_message(&mut writer, &err).await?;
            return Err("Protocol error: expected hello".into());
        }
        None => {
            return Err("Client disconnected before hello".into());
        }
    };

    // Extract character before moving client_info into the map.
    let character = client_info.character.clone();

    // Register client.
    ctx.clients.write().await.insert(client_id, client_info);

    // ── Step 3: Send History ─────────────────────────────────────────
    let history = ServerMessage::History(History {
        messages: Vec::new(),
        config: serde_json::json!({}),
    });
    write_message(&mut writer, &history).await?;
    info!(client_id, "Handshake complete");
    let result = message_loop(
        client_id,
        &mut buf_reader,
        &mut writer,
        &mut ctx.push_rx,
        &ctx.route_tx,
        &mut ctx.shutdown,
        character,
    )
    .await;

    // Unregister client on disconnect.
    ctx.clients.write().await.remove(&client_id);
    info!(client_id, "Client disconnected");

    result
}

/// Main message loop: reads client messages and forwards push messages.
async fn message_loop<R, W>(
    client_id: u64,
    reader: &mut BufReader<R>,
    writer: &mut W,
    push_rx: &mut broadcast::Receiver<ServerMessage>,
    route_tx: &tokio::sync::mpsc::Sender<RoutedMessage>,
    shutdown: &mut tokio::sync::watch::Receiver<()>,
    character: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    loop {
        tokio::select! {
            // Incoming client message.
            msg = read_message(reader) => {
                match msg? {
                    Some(client_msg) => {
                        route_client_message(client_id, client_msg, route_tx, writer, &character).await?;
                    }
                    None => {
                        // Client closed the connection.
                        break;
                    }
                }
            }

            // Push message from broadcast channel.
            msg = push_rx.recv() => {
                match msg {
                    Ok(server_msg) => {
                        write_message(writer, &server_msg).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(client_id, skipped = n, "Client lagged on broadcast");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }

            // Shutdown signal.
            _ = shutdown.changed() => {
                break;
            }
        }
    }
    Ok(())
}

/// Route a client message to the appropriate handler.
async fn route_client_message<W>(
    client_id: u64,
    msg: ClientMessage,
    route_tx: &tokio::sync::mpsc::Sender<RoutedMessage>,
    writer: &mut W,
    character: &Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    match msg {
        ClientMessage::Hello(_) => {
            // Second hello is a protocol error.
            let err = ServerMessage::Error(Error {
                code: ErrorCode::ProtocolError,
                message: "Duplicate hello".into(),
            });
            write_message(writer, &err).await?;
        }
        ClientMessage::Message(_) | ClientMessage::Regen(_) => {
            info!(client_id, msg_type = %msg_type_name(&msg), "Routing to engine");
            route_tx
                .send(RoutedMessage::Engine {
                    msg,
                    character: character.clone(),
                })
                .await?;
        }
        ClientMessage::Command(cmd) => {
            info!(client_id, command = %cmd.name, "Routing to command dispatcher");
            route_tx
                .send(RoutedMessage::Command {
                    cmd,
                    character: character.clone(),
                })
                .await?;
        }
    }
    Ok(())
}

/// Write a ServerMessage as a JSON line.
async fn write_message<W>(writer: &mut W, msg: &ServerMessage) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut json = serde_json::to_string(msg).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one ClientMessage from a newline-delimited JSON stream.
/// Returns `None` on EOF.
async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Option<ClientMessage>, Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_MESSAGE_SIZE {
        return Err("Message exceeds maximum size".into());
    }
    let msg: ClientMessage = serde_json::from_str(line.trim())?;
    Ok(Some(msg))
}

/// Return a human-readable type name for a ClientMessage variant.
fn msg_type_name(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::Hello(_) => "hello",
        ClientMessage::Message(_) => "message",
        ClientMessage::Regen(_) => "regen",
        ClientMessage::Command(_) => "command",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::{ClientHello, ClientMessageBody, Command, Regen};
    use shore_protocol::types::Message;
    use tokio::io::{duplex, AsyncWriteExt, BufReader};

    /// Helper: write a ClientMessage as JSON line into the writer half.
    async fn send_client_msg(
        writer: &mut tokio::io::DuplexStream,
        msg: &ClientMessage,
    ) -> std::io::Result<()> {
        let mut json = serde_json::to_string(msg).unwrap();
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await
    }

    /// Helper: read one ServerMessage from the reader half.
    async fn recv_server_msg(
        reader: &mut BufReader<tokio::io::DuplexStream>,
    ) -> ServerMessage {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    /// Spawn a handle_client task and return the pieces the test needs.
    struct TestHarness {
        handle: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
        client_reader: BufReader<tokio::io::DuplexStream>,
        client_writer: tokio::io::DuplexStream,
        clients: Arc<RwLock<HashMap<u64, ClientInfo>>>,
        push_tx: broadcast::Sender<ServerMessage>,
        route_rx: tokio::sync::mpsc::Receiver<RoutedMessage>,
        _shutdown_tx: tokio::sync::watch::Sender<()>,
    }

    fn spawn_handler() -> TestHarness {
        let (client_stream, server_stream) = duplex(8192);
        let (client_stream2, server_stream2) = duplex(8192);

        let clients: Arc<RwLock<HashMap<u64, ClientInfo>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (push_tx, _) = broadcast::channel(16);
        let push_rx = push_tx.subscribe();
        let (route_tx, route_rx) = tokio::sync::mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

        let clients_clone = clients.clone();
        let handle = tokio::spawn(async move {
            let ctx = ClientCtx {
                client_id: 1,
                clients: clients_clone,
                push_rx,
                route_tx,
                server_name: "test-server".into(),
                shutdown: shutdown_rx,
            };
            handle_client(server_stream, server_stream2, ctx).await
        });

        TestHarness {
            handle,
            client_reader: BufReader::new(client_stream2),
            client_writer: client_stream,
            clients,
            push_tx,
            route_rx,
            _shutdown_tx: shutdown_tx,
        }
    }

    /// Complete the SWP handshake on the client side.
    async fn do_handshake(
        reader: &mut BufReader<tokio::io::DuplexStream>,
        writer: &mut tokio::io::DuplexStream,
        client_type: &str,
    ) {
        let _hello = recv_server_msg(reader).await;
        send_client_msg(
            writer,
            &ClientMessage::Hello(ClientHello {
                client_type: client_type.into(),
                client_name: "test".into(),
                capabilities: vec![],
                character: None,
            }),
        )
        .await
        .unwrap();
        let _history = recv_server_msg(reader).await;
    }

    #[tokio::test]
    async fn handshake_and_disconnect() {
        let mut h = spawn_handler();

        let server_hello = recv_server_msg(&mut h.client_reader).await;
        match server_hello {
            ServerMessage::Hello(hello) => {
                assert_eq!(hello.v, SWP_V1);
                assert_eq!(hello.server_name, "test-server");
            }
            other => panic!("Expected Hello, got {:?}", other),
        }

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Hello(ClientHello {
                client_type: "tui".into(),
                client_name: "test-client".into(),
                capabilities: vec!["streaming".into()],
                character: None,
            }),
        )
        .await
        .unwrap();

        let history = recv_server_msg(&mut h.client_reader).await;
        match history {
            ServerMessage::History(hist) => assert!(hist.messages.is_empty()),
            other => panic!("Expected History, got {:?}", other),
        }

        // Verify client is registered.
        {
            let map = h.clients.read().await;
            let info = map.get(&1).expect("Client should be registered");
            assert_eq!(info.client_type, "tui");
            assert_eq!(info.client_name, "test-client");
            assert_eq!(info.capabilities, vec!["streaming"]);
        }

        drop(h.client_writer);
        assert!(h.handle.await.unwrap().is_ok());
        assert!(h.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn routes_message_to_engine() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "cli").await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Message(ClientMessageBody {
                rid: Some("msg_01".into()),
                text: "Hello world".into(),
                stream: true,
                images: vec![],
                absence_seconds: None,
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Engine { msg: ClientMessage::Message(body), .. } => {
                assert_eq!(body.text, "Hello world");
                assert_eq!(body.rid, Some("msg_01".into()));
            }
            other => panic!("Expected Engine(Message), got {:?}", other),
        }

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Regen(Regen {
                rid: Some("regen_01".into()),
                stream: true,
                guidance: None,
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Engine { msg: ClientMessage::Regen(r), .. } => {
                assert_eq!(r.rid, Some("regen_01".into()));
            }
            other => panic!("Expected Engine(Regen), got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn routes_command_to_dispatcher() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "cli").await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Command(Command {
                rid: Some("cmd_01".into()),
                name: "status".into(),
                args: serde_json::json!({}),
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Command { cmd, .. } => {
                assert_eq!(cmd.name, "status");
                assert_eq!(cmd.rid, Some("cmd_01".into()));
            }
            other => panic!("Expected Command, got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn broadcast_reaches_client() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui").await;

        use shore_protocol::server_msg::{
            CacheWarning, NewMessage, Phase, SendImage, StreamChunk, ToolCall, ToolResult,
        };

        let push_msgs: Vec<ServerMessage> = vec![
            ServerMessage::StreamChunk(StreamChunk {
                text: "hello".into(),
                content_type: "text".into(),
            }),
            ServerMessage::Phase(Phase {
                phase: "thinking".into(),
                model: Some("test-model".into()),
            }),
            ServerMessage::NewMessage(NewMessage {
                message: Message {
                    msg_id: "m1".into(),
                    role: shore_protocol::types::Role::Assistant,
                    content: "auto msg".into(),
                    images: vec![],
                    content_blocks: vec![],
                    alt_index: None,
                    alt_count: None,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                },
            }),
            ServerMessage::ToolCall(ToolCall {
                tool_id: "t1".into(),
                tool_name: "search".into(),
                input: serde_json::json!({"q": "test"}),
            }),
            ServerMessage::ToolResult(ToolResult {
                tool_id: "t1".into(),
                tool_name: "search".into(),
                output: "found it".into(),
                is_error: false,
            }),
            ServerMessage::SendImage(SendImage {
                path: "/tmp/img.png".into(),
                caption: None,
            }),
            ServerMessage::CacheWarning(CacheWarning {
                expected_tokens: 5000,
                message: "cache miss".into(),
            }),
        ];

        for msg in &push_msgs {
            h.push_tx.send(msg.clone()).unwrap();
        }

        for expected in &push_msgs {
            let received = recv_server_msg(&mut h.client_reader).await;
            let expected_json = serde_json::to_value(expected).unwrap();
            let received_json = serde_json::to_value(&received).unwrap();
            assert_eq!(expected_json, received_json);
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn protocol_error_on_non_hello_first() {
        let mut h = spawn_handler();

        let _hello = recv_server_msg(&mut h.client_reader).await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Message(ClientMessageBody {
                rid: None,
                text: "oops".into(),
                stream: false,
                images: vec![],
                absence_seconds: None,
            }),
        )
        .await
        .unwrap();

        let err = recv_server_msg(&mut h.client_reader).await;
        match err {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ProtocolError);
            }
            other => panic!("Expected Error, got {:?}", other),
        }

        assert!(h.handle.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn duplicate_hello_returns_error() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui").await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Hello(ClientHello {
                client_type: "tui".into(),
                client_name: "test".into(),
                capabilities: vec![],
                character: None,
            }),
        )
        .await
        .unwrap();

        let err = recv_server_msg(&mut h.client_reader).await;
        match err {
            ServerMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ProtocolError);
                assert!(e.message.contains("Duplicate hello"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }
}
