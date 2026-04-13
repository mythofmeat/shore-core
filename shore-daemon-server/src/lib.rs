pub mod registry;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use shore_protocol::client_msg::{ClientMessage, Command};
use shore_protocol::error::ErrorCode;
use shore_protocol::server_msg::{Error, History, Ping, ServerHello, ServerMessage, Shutdown};
use shore_protocol::types::{CharacterInfo, Message};
use shore_protocol::{MAX_WIRE_MESSAGE_SIZE, SWP_V1};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{error, info, instrument, warn};
const PING_INTERVAL: Duration = Duration::from_secs(30);

type HandshakeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone)]
pub struct HelloSnapshot {
    pub characters: Vec<CharacterInfo>,
}

#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    pub messages: Vec<Message>,
    pub config: serde_json::Value,
    pub selected_character: Option<String>,
    pub revision: u64,
}

#[derive(Clone)]
pub struct HandshakeProvider {
    pub hello: Arc<dyn Fn() -> HandshakeFuture<HelloSnapshot> + Send + Sync>,
    pub history: Arc<dyn Fn(Option<String>) -> HandshakeFuture<HistorySnapshot> + Send + Sync>,
}

impl std::fmt::Debug for HandshakeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandshakeProvider").finish_non_exhaustive()
    }
}

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

impl ClientInfo {
    /// Convert the registered client facts into the current session metadata.
    pub fn session_meta(&self) -> SessionMeta {
        SessionMeta {
            client_id: ClientId(self.id),
            session_id: SessionId(self.id),
            client_type: self.client_type.clone(),
            client_name: self.client_name.clone(),
            capabilities: self.capabilities.clone(),
            selected_character: self.character.clone(),
        }
    }
}

/// Opaque internal identifier for a connected client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

/// Opaque internal identifier for the current session.
///
/// For now this intentionally wraps the same numeric ID as the connection's
/// `ClientId`, matching Shore's current "one TCP connection == one session"
/// behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

/// High-level request type preserved through internal routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Message,
    Regen,
    Command,
    Cancel,
}

/// Session-scoped facts captured during the SWP handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub client_id: ClientId,
    pub session_id: SessionId,
    pub client_type: String,
    pub client_name: String,
    pub capabilities: Vec<String>,
    pub selected_character: Option<String>,
}

impl SessionMeta {
    fn with_selected_character(&self, selected_character: Option<String>) -> Self {
        Self {
            selected_character,
            ..self.clone()
        }
    }
}

/// Per-request metadata preserved from routing into the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMeta {
    pub session: SessionMeta,
    pub rid: Option<String>,
    pub kind: RequestKind,
}

/// Per-session direct-message router and session metadata mutator.
#[derive(Clone)]
pub struct SessionRouter {
    clients: Arc<RwLock<HashMap<u64, ClientInfo>>>,
    direct_txs: Arc<RwLock<HashMap<u64, mpsc::Sender<ServerMessage>>>>,
}

impl SessionRouter {
    /// Register a connected session and its direct sender.
    pub async fn register_session(
        &self,
        client: ClientInfo,
        direct_tx: mpsc::Sender<ServerMessage>,
    ) {
        let id = client.id;
        self.clients.write().await.insert(id, client);
        self.direct_txs.write().await.insert(id, direct_tx);
    }

    /// Unregister a disconnected session.
    pub async fn unregister_session(&self, session_id: SessionId) {
        self.clients.write().await.remove(&session_id.0);
        self.direct_txs.write().await.remove(&session_id.0);
    }

    /// Look up the direct sender for a session.
    pub async fn sender_for(&self, session_id: SessionId) -> Option<mpsc::Sender<ServerMessage>> {
        self.direct_txs.read().await.get(&session_id.0).cloned()
    }

    /// Send a request-scoped response directly to one session.
    pub async fn send_to_session(
        &self,
        session_id: SessionId,
        msg: ServerMessage,
    ) -> Result<(), mpsc::error::SendError<ServerMessage>> {
        if let Some(tx) = self.sender_for(session_id).await {
            tx.send(msg).await
        } else {
            Ok(())
        }
    }

    /// Update the transport-visible selected character after an authoritative session mutation.
    pub async fn set_selected_character(
        &self,
        session_id: SessionId,
        selected_character: Option<String>,
    ) -> bool {
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(&session_id.0) {
            client.character = selected_character;
            true
        } else {
            false
        }
    }
}

/// Messages the server routes internally after handshake.
#[derive(Debug, Clone)]
pub enum RoutedMessage {
    /// Message or Regen — route to engine.
    Engine {
        msg: ClientMessage,
        meta: RequestMeta,
    },
    /// Command — route to command dispatcher.
    Command { cmd: Command, meta: RequestMeta },
    /// All clients have disconnected — handler should cancel in-flight generation.
    AllClientsDisconnected,
}

/// Configuration for the server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: String,
    /// Optional peer IP allowlist. This is not authentication or transport security.
    pub allowed_hosts: Vec<String>,
    pub server_name: String,
    pub handshake: Option<HandshakeProvider>,
}

/// The SWP server.
///
/// Listens on TCP. Accepts concurrent client connections, performs the SWP
/// handshake, routes incoming messages, and broadcasts push messages to all
/// connected clients.
pub struct Server {
    config: ServerConfig,
    clients: Arc<RwLock<HashMap<u64, ClientInfo>>>,
    direct_txs: Arc<RwLock<HashMap<u64, mpsc::Sender<ServerMessage>>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    event_tx: broadcast::Sender<ServerMessage>,
    /// Receiver for routed messages (engine / command dispatcher consumes these).
    route_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<RoutedMessage>>>,
    route_tx: tokio::sync::mpsc::Sender<RoutedMessage>,
}

impl Server {
    /// Create a new server with the given config and broadcast capacity.
    pub fn new(config: ServerConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let (route_tx, route_rx) = tokio::sync::mpsc::channel(256);
        Self {
            config,
            clients: Arc::new(RwLock::new(HashMap::new())),
            direct_txs: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            event_tx,
            route_rx: Arc::new(tokio::sync::Mutex::new(route_rx)),
            route_tx,
        }
    }

    pub fn set_handshake_provider(&mut self, handshake: HandshakeProvider) {
        self.config.handshake = Some(handshake);
    }

    /// Returns a clone of the broadcast sender for unsolicited events.
    pub fn event_sender(&self) -> broadcast::Sender<ServerMessage> {
        self.event_tx.clone()
    }

    /// Returns the session router used for direct responses and session updates.
    pub fn session_router(&self) -> SessionRouter {
        SessionRouter {
            clients: self.clients.clone(),
            direct_txs: self.direct_txs.clone(),
        }
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

    /// Run the server. Listens on TCP forever.
    #[instrument(skip(self), fields(server_name = %self.config.server_name))]
    pub async fn run(&self, shutdown: tokio::sync::watch::Receiver<()>) -> std::io::Result<()> {
        let listener = TcpListener::bind(&self.config.addr).await?;
        info!(addr = %self.config.addr, "TCP listening");

        loop {
            tokio::select! {
                // Accept TCP connections.
                result = listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            // Best-effort peer IP allowlist. Empty means allow all.
                            if !self.config.allowed_hosts.is_empty() {
                                let peer_ip = addr.ip().to_string();
                                if !self.config.allowed_hosts.iter().any(|h| h == &peer_ip) {
                                    warn!(%addr, "TCP connection rejected: not in allowed_hosts");
                                    drop(stream);
                                    continue;
                                }
                            }
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

        Ok(())
    }

    /// Broadcast an unsolicited event to all connected clients.
    pub fn broadcast(&self, msg: ServerMessage) {
        // Ignore send errors — they just mean no receivers are listening.
        let _ = self.event_tx.send(msg);
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
        let direct_txs = self.direct_txs.clone();
        let event_rx = self.event_tx.subscribe();
        let route_tx = self.route_tx.clone();
        let server_name = self.config.server_name.clone();
        let handshake = self.config.handshake.clone();
        let (direct_tx, direct_rx) = mpsc::channel(256);

        tokio::spawn(async move {
            let ctx = ClientCtx {
                client_id,
                clients,
                direct_txs,
                event_rx,
                direct_rx,
                direct_tx,
                route_tx,
                server_name,
                handshake,
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
    direct_txs: Arc<RwLock<HashMap<u64, mpsc::Sender<ServerMessage>>>>,
    event_rx: broadcast::Receiver<ServerMessage>,
    direct_rx: mpsc::Receiver<ServerMessage>,
    direct_tx: mpsc::Sender<ServerMessage>,
    route_tx: tokio::sync::mpsc::Sender<RoutedMessage>,
    server_name: String,
    handshake: Option<HandshakeProvider>,
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

    let hello_snapshot = load_hello_snapshot(ctx.handshake.as_ref()).await;

    // ── Step 1: Send server Hello ────────────────────────────────────
    let server_hello = ServerMessage::Hello(ServerHello {
        v: SWP_V1,
        server_name: ctx.server_name.clone(),
        characters: hello_snapshot.characters.clone(),
    });
    write_message(&mut writer, &server_hello).await?;
    info!(client_id, "Sent server hello");

    // ── Step 2: Receive client Hello ─────────────────────────────────
    let client_hello = read_message(&mut buf_reader).await?;
    let client_hello = match client_hello {
        Some(ClientMessage::Hello(hello)) => {
            info!(
                client_id,
                client_type = %hello.client_type,
                client_name = %hello.client_name,
                character = ?hello.character,
                "Client hello received"
            );
            hello
        }
        Some(other) => {
            let err = ServerMessage::Error(Error {
                rid: None,
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

    let selected_character =
        resolve_handshake_character(client_hello.character, &hello_snapshot.characters);
    let history_snapshot =
        load_history_snapshot(ctx.handshake.as_ref(), selected_character.clone()).await;
    let client_info = ClientInfo {
        id: client_id,
        client_type: client_hello.client_type,
        client_name: client_hello.client_name,
        capabilities: client_hello.capabilities,
        character: history_snapshot.selected_character.clone(),
    };
    let session = client_info.session_meta();

    // Register client.
    ctx.clients.write().await.insert(client_id, client_info);
    ctx.direct_txs
        .write()
        .await
        .insert(client_id, ctx.direct_tx.clone());

    // ── Step 3: Send History ─────────────────────────────────────────
    let history = ServerMessage::History(History {
        rid: None,
        messages: history_snapshot.messages,
        config: history_snapshot.config,
        selected_character: history_snapshot.selected_character,
        revision: history_snapshot.revision,
    });
    write_message(&mut writer, &history).await?;
    info!(client_id, "Handshake complete");
    let result = message_loop(
        client_id,
        &mut buf_reader,
        &mut writer,
        &ctx.clients,
        &mut ctx.event_rx,
        &mut ctx.direct_rx,
        &ctx.route_tx,
        &session,
        &mut ctx.shutdown,
    )
    .await;

    // Unregister client on disconnect.
    // Hold the write lock across remove + is_empty to prevent two concurrent
    // disconnects from both seeing is_empty() == true (double-fire).
    let all_gone = {
        let mut clients = ctx.clients.write().await;
        clients.remove(&client_id);
        info!(client_id, "Client disconnected");
        clients.is_empty()
    };
    ctx.direct_txs.write().await.remove(&client_id);
    if all_gone {
        let _ = ctx
            .route_tx
            .send(RoutedMessage::AllClientsDisconnected)
            .await;
    }

    result
}

/// Main message loop: reads client messages and forwards push messages.
async fn message_loop<R, W>(
    client_id: u64,
    reader: &mut BufReader<R>,
    writer: &mut W,
    clients: &Arc<RwLock<HashMap<u64, ClientInfo>>>,
    event_rx: &mut broadcast::Receiver<ServerMessage>,
    direct_rx: &mut mpsc::Receiver<ServerMessage>,
    route_tx: &tokio::sync::mpsc::Sender<RoutedMessage>,
    session: &SessionMeta,
    shutdown: &mut tokio::sync::watch::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    let mut consecutive_lags: u32 = 0;
    const MAX_CONSECUTIVE_LAGS: u32 = 3;
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping_interval.tick().await;

    loop {
        tokio::select! {
            // Incoming client message.
            msg = read_message(reader) => {
                match msg? {
                    Some(client_msg) => {
                        route_client_message(
                            client_id,
                            client_msg,
                            route_tx,
                            writer,
                            clients,
                            session,
                        )
                        .await?;
                    }
                    None => {
                        // Client closed the connection.
                        break;
                    }
                }
            }

            // Direct response for this session.
            msg = direct_rx.recv() => {
                match msg {
                    Some(server_msg) => {
                        write_message(writer, &server_msg).await?;
                    }
                    None => break,
                }
            }

            _ = ping_interval.tick() => {
                write_message(writer, &ServerMessage::Ping(Ping {})).await?;
            }

            // Broadcast event from the event channel.
            msg = event_rx.recv() => {
                match msg {
                    Ok(server_msg) => {
                        consecutive_lags = 0;
                        if event_matches_session(clients, client_id, &server_msg).await {
                            write_message(writer, &server_msg).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        consecutive_lags += 1;
                        warn!(client_id, skipped = n, consecutive = consecutive_lags,
                              "Client lagged on broadcast");
                        if consecutive_lags >= MAX_CONSECUTIVE_LAGS {
                            warn!(client_id, "Disconnecting client after repeated lag");
                            break;
                        }
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

async fn load_hello_snapshot(handshake: Option<&HandshakeProvider>) -> HelloSnapshot {
    match handshake {
        Some(provider) => (provider.hello)().await,
        None => HelloSnapshot {
            characters: vec![CharacterInfo {
                name: "default".into(),
            }],
        },
    }
}

async fn load_history_snapshot(
    handshake: Option<&HandshakeProvider>,
    selected_character: Option<String>,
) -> HistorySnapshot {
    match handshake {
        Some(provider) => (provider.history)(selected_character).await,
        None => HistorySnapshot {
            messages: Vec::new(),
            config: serde_json::json!({}),
            selected_character,
            revision: 0,
        },
    }
}

fn resolve_handshake_character(
    requested: Option<String>,
    characters: &[CharacterInfo],
) -> Option<String> {
    match requested {
        Some(name) if characters.iter().any(|character| character.name == name) => Some(name),
        Some(name) => {
            warn!(requested = %name, "Ignoring unknown connect-time character selection");
            None
        }
        None if characters.len() == 1 => Some(characters[0].name.clone()),
        None => None,
    }
}

/// Route a client message to the appropriate handler.
async fn route_client_message<W>(
    client_id: u64,
    msg: ClientMessage,
    route_tx: &tokio::sync::mpsc::Sender<RoutedMessage>,
    writer: &mut W,
    clients: &Arc<RwLock<HashMap<u64, ClientInfo>>>,
    session: &SessionMeta,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    let character = clients
        .read()
        .await
        .get(&client_id)
        .and_then(|info| info.character.clone());

    match msg {
        ClientMessage::Hello(_) => {
            // Second hello is a protocol error.
            let err = ServerMessage::Error(Error {
                rid: None,
                code: ErrorCode::ProtocolError,
                message: "Duplicate hello".into(),
            });
            write_message(writer, &err).await?;
        }
        ClientMessage::Message(_) | ClientMessage::Regen(_) | ClientMessage::Cancel(_) => {
            info!(client_id, msg_type = %msg_type_name(&msg), "Routing to engine");
            let (rid, kind) = match &msg {
                ClientMessage::Message(body) => (body.rid.clone(), RequestKind::Message),
                ClientMessage::Regen(regen) => (regen.rid.clone(), RequestKind::Regen),
                ClientMessage::Cancel(_) => (None, RequestKind::Cancel),
                ClientMessage::Hello(_) | ClientMessage::Command(_) => unreachable!(),
            };
            let meta = RequestMeta {
                session: session.with_selected_character(character),
                rid,
                kind,
            };
            route_tx.send(RoutedMessage::Engine { msg, meta }).await?;
        }
        ClientMessage::Command(cmd) => {
            info!(client_id, command = %cmd.name, "Routing to command dispatcher");
            let meta = RequestMeta {
                session: session.with_selected_character(character),
                rid: cmd.rid.clone(),
                kind: RequestKind::Command,
            };
            route_tx.send(RoutedMessage::Command { cmd, meta }).await?;
        }
    }
    Ok(())
}

async fn event_matches_session(
    clients: &Arc<RwLock<HashMap<u64, ClientInfo>>>,
    client_id: u64,
    msg: &ServerMessage,
) -> bool {
    match msg {
        ServerMessage::Hello(_) => true,
        ServerMessage::NewMessage(_)
        | ServerMessage::History(_)
        | ServerMessage::Shutdown(_)
        | ServerMessage::Ping(_)
        | ServerMessage::CacheWarning(_) => true,
        ServerMessage::CommandOutput(_)
        | ServerMessage::Error(_)
        | ServerMessage::StreamStart(_)
        | ServerMessage::StreamChunk(_)
        | ServerMessage::StreamEnd(_)
        | ServerMessage::Phase(_)
        | ServerMessage::ToolCall(_)
        | ServerMessage::ToolResult(_)
        | ServerMessage::SendImage(_) => clients.read().await.contains_key(&client_id),
    }
}

/// Write a ServerMessage as a JSON line.
async fn write_message<W>(writer: &mut W, msg: &ServerMessage) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut json = serde_json::to_string(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one ClientMessage from a newline-delimited JSON stream.
/// Returns `None` on EOF.
///
/// The read is bounded to `MAX_WIRE_MESSAGE_SIZE` bytes to prevent a
/// malicious client from exhausting server memory with a single line.
async fn read_message<R>(
    reader: &mut BufReader<R>,
) -> Result<Option<ClientMessage>, Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(None); // EOF
            }
            break; // EOF mid-line — try to parse what we have
        }
        // Find newline in the buffer.
        let (consume, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(pos) => (pos + 1, true),
            None => (buf.len(), false),
        };
        // Check size limit BEFORE allocating.
        if line.len() + consume > MAX_WIRE_MESSAGE_SIZE {
            return Err("Message exceeds maximum size".into());
        }
        line.push_str(std::str::from_utf8(&buf[..consume]).map_err(|e| e.to_string())?);
        reader.consume(consume);
        if done {
            break;
        }
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
        ClientMessage::Cancel(_) => "cancel",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shore_protocol::client_msg::{ClientHello, ClientMessageBody, Command, Regen};
    use shore_protocol::types::{Message, Role};
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
    async fn recv_server_msg(reader: &mut BufReader<tokio::io::DuplexStream>) -> ServerMessage {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    fn test_handshake_provider() -> HandshakeProvider {
        HandshakeProvider {
            hello: Arc::new(|| {
                Box::pin(async {
                    HelloSnapshot {
                        characters: vec![
                            CharacterInfo {
                                name: "alice".into(),
                            },
                            CharacterInfo { name: "bob".into() },
                        ],
                    }
                })
            }),
            history: Arc::new(|selected_character| {
                Box::pin(async move {
                    let messages = match selected_character.as_deref() {
                        Some("alice") => vec![Message {
                            msg_id: "m1".into(),
                            role: Role::Assistant,
                            content: "hello from alice".into(),
                            images: vec![],
                            content_blocks: vec![],
                            alt_index: None,
                            alt_count: None,
                            timestamp: "2026-01-01T00:00:00Z".into(),
                        }],
                        _ => Vec::new(),
                    };
                    HistorySnapshot {
                        messages,
                        config: serde_json::json!({}),
                        selected_character,
                        revision: 1,
                    }
                })
            }),
        }
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

        let clients: Arc<RwLock<HashMap<u64, ClientInfo>>> = Arc::new(RwLock::new(HashMap::new()));
        let direct_txs: Arc<RwLock<HashMap<u64, mpsc::Sender<ServerMessage>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (push_tx, _) = broadcast::channel(16);
        let event_rx = push_tx.subscribe();
        let (direct_tx, direct_rx) = mpsc::channel(16);
        let (route_tx, route_rx) = tokio::sync::mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

        let clients_clone = clients.clone();
        let direct_txs_clone = direct_txs.clone();
        let handle = tokio::spawn(async move {
            let ctx = ClientCtx {
                client_id: 1,
                clients: clients_clone,
                direct_txs: direct_txs_clone,
                event_rx,
                direct_rx,
                direct_tx,
                route_tx,
                server_name: "test-server".into(),
                handshake: Some(test_handshake_provider()),
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
        character: Option<&str>,
    ) {
        let _hello = recv_server_msg(reader).await;
        send_client_msg(
            writer,
            &ClientMessage::Hello(ClientHello {
                client_type: client_type.into(),
                client_name: "test".into(),
                capabilities: vec![],
                character: character.map(str::to_string),
            }),
        )
        .await
        .unwrap();
        let _history = recv_server_msg(reader).await;
    }

    fn assert_session_meta(
        meta: &SessionMeta,
        client_id: u64,
        client_type: &str,
        client_name: &str,
        capabilities: &[&str],
        selected_character: Option<&str>,
    ) {
        assert_eq!(meta.client_id, ClientId(client_id));
        assert_eq!(meta.session_id, SessionId(client_id));
        assert_eq!(meta.client_type, client_type);
        assert_eq!(meta.client_name, client_name);
        let expected_caps: Vec<String> = capabilities.iter().map(|s| s.to_string()).collect();
        assert_eq!(meta.capabilities, expected_caps);
        assert_eq!(meta.selected_character.as_deref(), selected_character);
    }

    #[tokio::test]
    async fn handshake_and_disconnect() {
        let mut h = spawn_handler();

        let server_hello = recv_server_msg(&mut h.client_reader).await;
        match server_hello {
            ServerMessage::Hello(hello) => {
                assert_eq!(hello.v, SWP_V1);
                assert_eq!(hello.server_name, "test-server");
                assert_eq!(hello.characters.len(), 2);
                assert_eq!(hello.characters[0].name, "alice");
                assert_eq!(hello.characters[1].name, "bob");
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
            ServerMessage::History(hist) => {
                assert!(hist.messages.is_empty());
                assert!(hist.selected_character.is_none());
                assert_eq!(hist.revision, 1);
            }
            other => panic!("Expected History, got {:?}", other),
        }

        // Verify client is registered.
        {
            let map = h.clients.read().await;
            let info = map.get(&1).expect("Client should be registered");
            assert_eq!(info.client_type, "tui");
            assert_eq!(info.client_name, "test-client");
            assert_eq!(info.capabilities, vec!["streaming"]);
            assert_session_meta(
                &info.session_meta(),
                1,
                "tui",
                "test-client",
                &["streaming"],
                None,
            );
        }

        drop(h.client_writer);
        assert!(h.handle.await.unwrap().is_ok());
        assert!(h.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn routes_message_to_engine() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "cli", None).await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Message(ClientMessageBody {
                rid: Some("msg_01".into()),
                text: "Hello world".into(),
                stream: true,
                images: vec![],
                image_data: vec![],
                absence_seconds: None,
                overrides: None,
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Engine {
                msg: ClientMessage::Message(body),
                meta,
            } => {
                assert_eq!(body.text, "Hello world");
                assert_eq!(body.rid, Some("msg_01".into()));
                assert_eq!(meta.kind, RequestKind::Message);
                assert_eq!(meta.rid.as_deref(), Some("msg_01"));
                assert_session_meta(&meta.session, 1, "cli", "test", &[], None);
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
            RoutedMessage::Engine {
                msg: ClientMessage::Regen(r),
                meta,
            } => {
                assert_eq!(r.rid, Some("regen_01".into()));
                assert_eq!(meta.kind, RequestKind::Regen);
                assert_eq!(meta.rid.as_deref(), Some("regen_01"));
                assert_session_meta(&meta.session, 1, "cli", "test", &[], None);
            }
            other => panic!("Expected Engine(Regen), got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn handshake_uses_requested_character_snapshot() {
        let mut h = spawn_handler();

        let _hello = recv_server_msg(&mut h.client_reader).await;
        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Hello(ClientHello {
                client_type: "tui".into(),
                client_name: "test-client".into(),
                capabilities: vec!["streaming".into()],
                character: Some("alice".into()),
            }),
        )
        .await
        .unwrap();

        let history = recv_server_msg(&mut h.client_reader).await;
        match history {
            ServerMessage::History(hist) => {
                assert_eq!(hist.selected_character.as_deref(), Some("alice"));
                assert_eq!(hist.messages.len(), 1);
                assert_eq!(hist.messages[0].content, "hello from alice");
                assert_eq!(hist.revision, 1);
            }
            other => panic!("Expected History, got {:?}", other),
        }

        {
            let map = h.clients.read().await;
            let info = map.get(&1).expect("Client should be registered");
            assert_eq!(info.character.as_deref(), Some("alice"));
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn routes_command_to_dispatcher() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "cli", None).await;

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
            RoutedMessage::Command { cmd, meta } => {
                assert_eq!(cmd.name, "status");
                assert_eq!(cmd.rid, Some("cmd_01".into()));
                assert_eq!(meta.kind, RequestKind::Command);
                assert_eq!(meta.rid.as_deref(), Some("cmd_01"));
                assert_session_meta(&meta.session, 1, "cli", "test", &[], None);
            }
            other => panic!("Expected Command, got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn switch_character_waits_for_authoritative_session_update() {
        let mut h = spawn_handler();
        do_handshake(
            &mut h.client_reader,
            &mut h.client_writer,
            "tui",
            Some("alice"),
        )
        .await;

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Command(Command {
                rid: Some("cmd_switch".into()),
                name: "switch_character".into(),
                args: serde_json::json!({ "name": "Alice" }),
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Command { cmd, meta } => {
                assert_eq!(cmd.name, "switch_character");
                assert_eq!(meta.kind, RequestKind::Command);
                assert_eq!(meta.rid.as_deref(), Some("cmd_switch"));
                assert_session_meta(&meta.session, 1, "tui", "test", &[], Some("alice"));
            }
            other => panic!("Expected Command, got {:?}", other),
        }

        {
            let mut clients = h.clients.write().await;
            clients.get_mut(&1).unwrap().character = Some("Alice".into());
        }

        send_client_msg(
            &mut h.client_writer,
            &ClientMessage::Command(Command {
                rid: Some("cmd_status".into()),
                name: "status".into(),
                args: serde_json::json!({}),
            }),
        )
        .await
        .unwrap();

        let routed = h.route_rx.recv().await.unwrap();
        match routed {
            RoutedMessage::Command { cmd, meta } => {
                assert_eq!(cmd.name, "status");
                assert_eq!(meta.kind, RequestKind::Command);
                assert_eq!(meta.rid.as_deref(), Some("cmd_status"));
                assert_session_meta(&meta.session, 1, "tui", "test", &[], Some("Alice"));
            }
            other => panic!("Expected Command, got {:?}", other),
        }

        drop(h.client_writer);
        h.handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn broadcast_reaches_client() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui", None).await;

        use shore_protocol::server_msg::{
            CacheWarning, NewMessage, Phase, SendImage, StreamChunk, ToolCall, ToolResult,
        };

        let push_msgs: Vec<ServerMessage> = vec![
            ServerMessage::StreamChunk(StreamChunk {
                rid: None,
                text: "hello".into(),
                content_type: "text".into(),
            }),
            ServerMessage::Phase(Phase {
                rid: None,
                phase: "thinking".into(),
                model: Some("test-model".into()),
            }),
            ServerMessage::NewMessage(NewMessage {
                revision: 2,
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
                rid: None,
                tool_id: "t1".into(),
                tool_name: "search".into(),
                input: serde_json::json!({"q": "test"}),
            }),
            ServerMessage::ToolResult(ToolResult {
                rid: None,
                tool_id: "t1".into(),
                tool_name: "search".into(),
                output: "found it".into(),
                is_error: false,
            }),
            ServerMessage::SendImage(SendImage {
                rid: None,
                path: "/tmp/img.png".into(),
                caption: None,
                data: None,
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
    async fn periodic_ping_reaches_client() {
        tokio::time::pause();
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui", None).await;

        tokio::time::advance(PING_INTERVAL).await;

        let ping = recv_server_msg(&mut h.client_reader).await;
        assert!(matches!(ping, ServerMessage::Ping(_)));

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
                image_data: vec![],
                absence_seconds: None,
                overrides: None,
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
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui", None).await;

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

    // ── concurrent clients ───────────────────────────────────────────────

    #[tokio::test]
    async fn broadcast_reaches_two_clients() {
        // Shared state for both clients.
        let clients: Arc<RwLock<HashMap<u64, ClientInfo>>> = Arc::new(RwLock::new(HashMap::new()));
        let direct_txs: Arc<RwLock<HashMap<u64, mpsc::Sender<ServerMessage>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (push_tx, _) = broadcast::channel::<ServerMessage>(16);
        let (route_tx, _route_rx) = tokio::sync::mpsc::channel::<RoutedMessage>(16);
        let (shutdown_tx, _) = tokio::sync::watch::channel(());

        // Spawn client 1.
        let (c1_stream, s1_stream) = duplex(8192);
        let (c1_stream2, s1_stream2) = duplex(8192);
        let h1 = {
            let (direct_tx, direct_rx) = mpsc::channel(16);
            let ctx = ClientCtx {
                client_id: 1,
                clients: clients.clone(),
                direct_txs: direct_txs.clone(),
                event_rx: push_tx.subscribe(),
                direct_rx,
                direct_tx,
                route_tx: route_tx.clone(),
                server_name: "test-server".into(),
                handshake: Some(test_handshake_provider()),
                shutdown: shutdown_tx.subscribe(),
            };
            tokio::spawn(async move { handle_client(s1_stream, s1_stream2, ctx).await })
        };
        let mut r1 = BufReader::new(c1_stream2);
        let mut w1 = c1_stream;

        // Spawn client 2.
        let (c2_stream, s2_stream) = duplex(8192);
        let (c2_stream2, s2_stream2) = duplex(8192);
        let h2 = {
            let (direct_tx, direct_rx) = mpsc::channel(16);
            let ctx = ClientCtx {
                client_id: 2,
                clients: clients.clone(),
                direct_txs: direct_txs.clone(),
                event_rx: push_tx.subscribe(),
                direct_rx,
                direct_tx,
                route_tx: route_tx.clone(),
                server_name: "test-server".into(),
                handshake: Some(test_handshake_provider()),
                shutdown: shutdown_tx.subscribe(),
            };
            tokio::spawn(async move { handle_client(s2_stream, s2_stream2, ctx).await })
        };
        let mut r2 = BufReader::new(c2_stream2);
        let mut w2 = c2_stream;

        // Complete handshakes for both.
        do_handshake(&mut r1, &mut w1, "tui", None).await;
        do_handshake(&mut r2, &mut w2, "cli", None).await;

        // Both clients should be registered.
        assert_eq!(clients.read().await.len(), 2);

        // Send a broadcast.
        let chunk = ServerMessage::StreamChunk(shore_protocol::server_msg::StreamChunk {
            rid: None,
            text: "hello both".into(),
            content_type: "text".into(),
        });
        push_tx.send(chunk.clone()).unwrap();

        // Both clients should receive it.
        let got1 = recv_server_msg(&mut r1).await;
        let got2 = recv_server_msg(&mut r2).await;
        assert_eq!(
            serde_json::to_value(&got1).unwrap(),
            serde_json::to_value(&chunk).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&got2).unwrap(),
            serde_json::to_value(&chunk).unwrap()
        );

        // Clean shutdown.
        drop(w1);
        drop(w2);
        h1.await.unwrap().unwrap();
        h2.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn direct_send_reaches_only_target_session() {
        let server = Server::new(ServerConfig {
            addr: "127.0.0.1:0".into(),
            allowed_hosts: vec![],
            server_name: "test-server".into(),
            handshake: None,
        });
        let router = server.session_router();

        let (tx1, mut rx1) = mpsc::channel(16);
        let (tx2, mut rx2) = mpsc::channel(16);

        router
            .register_session(
                ClientInfo {
                    id: 1,
                    client_type: "tui".into(),
                    client_name: "first".into(),
                    capabilities: vec![],
                    character: None,
                },
                tx1,
            )
            .await;
        router
            .register_session(
                ClientInfo {
                    id: 2,
                    client_type: "cli".into(),
                    client_name: "second".into(),
                    capabilities: vec![],
                    character: None,
                },
                tx2,
            )
            .await;

        let direct = ServerMessage::StreamChunk(shore_protocol::server_msg::StreamChunk {
            rid: None,
            text: "only one".into(),
            content_type: "text".into(),
        });
        router
            .send_to_session(SessionId(1), direct.clone())
            .await
            .unwrap();

        let got1 = rx1.recv().await.unwrap();
        assert_eq!(
            serde_json::to_value(&got1).unwrap(),
            serde_json::to_value(&direct).unwrap()
        );

        match tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv()).await {
            Err(_) => {}
            Ok(None) => {}
            Ok(Some(other)) => panic!("unexpected direct delivery to other session: {other:?}"),
        }
    }

    #[tokio::test]
    async fn client_disconnect_is_graceful() {
        let mut h = spawn_handler();
        do_handshake(&mut h.client_reader, &mut h.client_writer, "tui", None).await;

        // Client is registered.
        assert_eq!(h.clients.read().await.len(), 1);

        // Simulate abrupt disconnect by dropping the writer.
        drop(h.client_writer);

        // Handler task should complete without error.
        let result = h.handle.await.unwrap();
        assert!(result.is_ok());

        // Client should be deregistered.
        assert!(h.clients.read().await.is_empty());
    }

    // ── TCP ACL enforcement ─────────────────────────────────────────

    /// Find an available TCP port by briefly binding to port 0.
    fn available_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Spin up a real `Server::run()` with the given allowed_hosts.
    /// Returns the server handle and a shutdown sender.
    fn spawn_tcp_server(
        port: u16,
        allowed_hosts: Vec<String>,
    ) -> (
        tokio::task::JoinHandle<std::io::Result<()>>,
        tokio::sync::watch::Sender<()>,
    ) {
        let config = ServerConfig {
            addr: format!("127.0.0.1:{port}"),
            allowed_hosts,
            server_name: "test-acl-server".into(),
            handshake: None,
        };
        let server = Server::new(config);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
        let handle = tokio::spawn(async move { server.run(shutdown_rx).await });

        (handle, shutdown_tx)
    }

    /// Connect via TCP to the given port, complete the SWP handshake, and
    /// return true if ServerHello was received (i.e. connection was accepted).
    async fn tcp_handshake_succeeds(port: u16) -> bool {
        use tokio::net::TcpStream;
        use tokio::time::{timeout, Duration};

        // Small delay to let the server bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stream = match timeout(
            Duration::from_secs(2),
            TcpStream::connect(format!("127.0.0.1:{port}")),
        )
        .await
        {
            Ok(Ok(s)) => s,
            _ => return false,
        };

        let (reader, _writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        match timeout(Duration::from_secs(2), reader.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {
                // Parse as ServerMessage — a ServerHello means ACL passed.
                serde_json::from_str::<ServerMessage>(line.trim())
                    .map(|msg| matches!(msg, ServerMessage::Hello(_)))
                    .unwrap_or(false)
            }
            _ => false,
        }
    }

    #[tokio::test]
    async fn tcp_acl_empty_allows_all() {
        let port = available_port();
        let (_handle, shutdown_tx) = spawn_tcp_server(port, vec![]);

        assert!(
            tcp_handshake_succeeds(port).await,
            "Empty allowed_hosts should allow all"
        );

        let _ = shutdown_tx.send(());
    }

    #[tokio::test]
    async fn tcp_acl_allows_matching_ip() {
        let port = available_port();
        let (_handle, shutdown_tx) = spawn_tcp_server(port, vec!["127.0.0.1".into()]);

        assert!(
            tcp_handshake_succeeds(port).await,
            "Matching IP should be allowed"
        );

        let _ = shutdown_tx.send(());
    }

    /// `read_message` must reject messages exceeding MAX_WIRE_MESSAGE_SIZE.
    /// The read itself should be bounded to prevent OOM from a malicious
    /// client sending a multi-GB line.
    #[tokio::test]
    async fn read_message_rejects_oversized() {
        let oversized = format!(
            "{{\"type\":\"message\",\"body\":{{\"content\":\"{}\"}}}}\n",
            "x".repeat(MAX_WIRE_MESSAGE_SIZE + 1)
        );

        let (mut writer, reader) = duplex(MAX_WIRE_MESSAGE_SIZE + 4096);
        let mut buf_reader = BufReader::new(reader);

        use tokio::io::AsyncWriteExt;
        writer.write_all(oversized.as_bytes()).await.unwrap();
        drop(writer);

        let result = read_message(&mut buf_reader).await;
        assert!(
            result.is_err(),
            "Oversized message should be rejected, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn tcp_acl_rejects_non_matching_ip() {
        let port = available_port();
        let (_handle, shutdown_tx) = spawn_tcp_server(port, vec!["10.0.0.1".into()]);

        assert!(
            !tcp_handshake_succeeds(port).await,
            "Non-matching IP should be rejected"
        );

        let _ = shutdown_tx.send(());
    }
}
