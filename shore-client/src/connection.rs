use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tracing::{debug, error, trace, warn};

use shore_protocol::client_msg::{ClientHello, ClientMessage};
use shore_protocol::server_msg::{History, ServerHello, ServerMessage};
use shore_protocol::SWP_V1;

use crate::error::{ClientError, Result};

/// Address to connect to — either a Unix socket path or a TCP host:port.
#[derive(Debug, Clone)]
pub enum ServerAddr {
    /// Path to a Unix domain socket.
    Unix(String),
    /// TCP address in `host:port` form.
    Tcp(String),
}

/// Internal trait to unify read/write halves across transport types.
trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> AsyncReadWrite for T {}

/// A connection to a Shore daemon over the SWP protocol.
///
/// Sends and receives JSON-Lines framed messages. The connection is `Send`
/// so it can be moved across tokio tasks.
pub struct SWPConnection {
    reader: BufReader<Box<dyn AsyncReadWrite>>,
    writer: BufWriter<Box<dyn AsyncReadWrite>>,
}

impl std::fmt::Debug for SWPConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SWPConnection").finish_non_exhaustive()
    }
}

impl SWPConnection {
    /// Open a raw transport to `addr` without performing the handshake.
    async fn open(addr: &ServerAddr) -> Result<Self> {
        match addr {
            #[cfg(unix)]
            ServerAddr::Unix(path) => {
                debug!(path = %path, "connecting via unix socket");
                let stream = UnixStream::connect(path).await.map_err(|e| {
                    error!(path = %path, error = %e, "unix socket connect failed");
                    ClientError::Connect(format!("unix:{path}: {e}"))
                })?;
                let (r, w) = stream.into_split();
                debug!(path = %path, "unix socket connected");
                Ok(Self {
                    reader: BufReader::new(Box::new(tokio::io::join(r, tokio::io::sink()))),
                    writer: BufWriter::new(Box::new(tokio::io::join(tokio::io::empty(), w))),
                })
            }
            #[cfg(not(unix))]
            ServerAddr::Unix(_) => Err(ClientError::Connect(
                "Unix sockets are not supported on this platform".into(),
            )),
            ServerAddr::Tcp(addr) => {
                debug!(addr = %addr, "connecting via tcp");
                let stream = TcpStream::connect(addr).await.map_err(|e| {
                    error!(addr = %addr, error = %e, "tcp connect failed");
                    ClientError::Connect(format!("tcp:{addr}: {e}"))
                })?;
                let (r, w) = stream.into_split();
                debug!(addr = %addr, "tcp connected");
                Ok(Self {
                    reader: BufReader::new(Box::new(tokio::io::join(r, tokio::io::sink()))),
                    writer: BufWriter::new(Box::new(tokio::io::join(tokio::io::empty(), w))),
                })
            }
        }
    }

    /// Connect to the daemon and perform the SWP handshake.
    ///
    /// The handshake sequence is:
    /// 1. Receive `ServerMessage::Hello` from daemon
    /// 2. Send `ClientMessage::Hello`
    /// 3. Receive `ServerMessage::History`
    ///
    /// Returns the connection along with the server hello and initial history.
    pub async fn connect(
        addr: &ServerAddr,
        client_type: impl Into<String>,
        client_name: impl Into<String>,
        character: Option<String>,
    ) -> Result<(Self, ServerHello, History)> {
        let mut conn = Self::open(addr).await?;
        let (server_hello, history) = conn
            .do_handshake(client_type.into(), client_name.into(), character)
            .await?;
        Ok((conn, server_hello, history))
    }

    /// Perform the 3-step SWP handshake on an already-open connection.
    async fn do_handshake(
        &mut self,
        client_type: String,
        client_name: String,
        character: Option<String>,
    ) -> Result<(ServerHello, History)> {
        debug!(client_type = %client_type, client_name = %client_name, character = ?character, "starting SWP handshake");

        // Step 1: receive server hello
        let first_msg = self.recv().await?;
        let server_hello = match first_msg {
            ServerMessage::Hello(h) => {
                if h.v != SWP_V1 {
                    error!(server_version = h.v, expected = SWP_V1, "protocol version mismatch");
                    return Err(ClientError::Protocol(format!(
                        "unsupported protocol version: {} (expected {})",
                        h.v, SWP_V1
                    )));
                }
                debug!(
                    server_name = %h.server_name,
                    characters = h.characters.len(),
                    "received server hello"
                );
                h
            }
            other => {
                error!("expected server hello, got unexpected message");
                return Err(ClientError::Protocol(format!(
                    "expected server hello, got: {other:?}"
                )));
            }
        };

        // Step 2: send client hello
        let hello = ClientMessage::Hello(ClientHello {
            client_type,
            client_name,
            capabilities: vec!["streaming".into()],
            character,
        });
        self.send(&hello).await?;
        debug!("sent client hello");

        // Step 3: receive history
        let history_msg = self.recv().await?;
        let history = match history_msg {
            ServerMessage::History(h) => {
                debug!(message_count = h.messages.len(), "received history");
                h
            }
            other => {
                error!("expected history, got unexpected message");
                return Err(ClientError::Protocol(format!(
                    "expected history, got: {other:?}"
                )));
            }
        };

        debug!("SWP handshake complete");
        Ok((server_hello, history))
    }

    /// Send a client message as a JSON line.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let line = serde_json::to_string(msg).map_err(|e| {
            error!(error = %e, "failed to serialize client message");
            ClientError::Serialize(e)
        })?;
        trace!(bytes = line.len(), "sending message");
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(ClientError::Io)?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(ClientError::Io)?;
        self.writer.flush().await.map_err(ClientError::Io)?;
        Ok(())
    }

    /// Receive the next server message (one JSON line).
    ///
    /// Returns `Err(ClientError::Disconnected)` on EOF.
    pub async fn recv(&mut self) -> Result<ServerMessage> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(ClientError::Io)?;
        if n == 0 {
            debug!("EOF on connection — disconnected");
            return Err(ClientError::Disconnected);
        }
        let msg: ServerMessage = serde_json::from_str(line.trim()).map_err(|e| {
            warn!(error = %e, raw_len = line.len(), "failed to deserialize server message");
            ClientError::Deserialize(e)
        })?;
        trace!(bytes = n, "received message");
        Ok(msg)
    }

    /// Send a user message. Returns the `rid` used.
    pub async fn send_message(
        &mut self,
        text: impl Into<String>,
        stream: bool,
    ) -> Result<Option<String>> {
        self.send_message_with_images(text, stream, vec![]).await
    }

    /// Send a user message with image attachments. Returns the `rid` used.
    pub async fn send_message_with_images(
        &mut self,
        text: impl Into<String>,
        stream: bool,
        images: Vec<String>,
    ) -> Result<Option<String>> {
        self.send_message_full(text, stream, images, None).await
    }

    /// Send a user message with image attachments and parameter overrides.
    ///
    /// Reads each image path, base64-encodes the data, and sends both
    /// `images` (paths, for legacy daemons) and `image_data` (base64, preferred).
    pub async fn send_message_full(
        &mut self,
        text: impl Into<String>,
        stream: bool,
        images: Vec<String>,
        overrides: Option<shore_protocol::client_msg::MessageOverrides>,
    ) -> Result<Option<String>> {
        use base64::Engine;
        use shore_protocol::client_msg::{ClientMessageBody, ImageUpload};

        let image_data: Vec<ImageUpload> = images
            .iter()
            .filter_map(|path| {
                let bytes = std::fs::read(path).ok()?;
                let filename = std::path::Path::new(path)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "image".to_string());
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                Some(ImageUpload { filename, data })
            })
            .collect();

        let rid = Some(uuid_v4());
        let msg = ClientMessage::Message(ClientMessageBody {
            rid: rid.clone(),
            text: text.into(),
            stream,
            images,
            image_data,
            absence_seconds: None,
            overrides,
        });
        self.send(&msg).await?;
        Ok(rid)
    }

    /// Send a regen request. Returns the `rid` used.
    pub async fn send_regen(
        &mut self,
        stream: bool,
        guidance: Option<String>,
    ) -> Result<Option<String>> {
        use shore_protocol::client_msg::Regen;
        let rid = Some(uuid_v4());
        let msg = ClientMessage::Regen(Regen {
            rid: rid.clone(),
            stream,
            guidance,
        });
        self.send(&msg).await?;
        Ok(rid)
    }

    /// Send a command. Returns the `rid` used.
    pub async fn send_command(
        &mut self,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Result<Option<String>> {
        use shore_protocol::client_msg::Command;
        let rid = Some(uuid_v4());
        let msg = ClientMessage::Command(Command {
            rid: rid.clone(),
            name: name.into(),
            args,
        });
        self.send(&msg).await?;
        Ok(rid)
    }
}

/// Build an `SWPConnection` from an already-connected async stream.
/// Useful for testing or when the caller manages the transport.
impl SWPConnection {
    pub fn from_raw_stream<S>(stream: S) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        let (r, w) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(Box::new(tokio::io::join(r, tokio::io::sink()))),
            writer: BufWriter::new(Box::new(tokio::io::join(tokio::io::empty(), w))),
        }
    }

    /// Connect using an already-established stream and perform the SWP handshake.
    ///
    /// This is useful for testing (with `tokio::io::duplex`) or when the caller
    /// manages transport setup.
    pub async fn connect_raw<S>(
        stream: S,
        client_type: impl Into<String>,
        client_name: impl Into<String>,
        character: Option<String>,
    ) -> Result<(Self, ServerHello, History)>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        let mut conn = Self::from_raw_stream(stream);
        let (server_hello, history) = conn
            .do_handshake(client_type.into(), client_name.into(), character)
            .await?;
        Ok((conn, server_hello, history))
    }
}

/// Simple v4-style random ID (not cryptographic, just unique enough for rids).
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("rid_{:016x}", nanos)
}

/// Check whether a path looks like it could be a Unix socket path.
pub fn is_unix_path(s: &str) -> bool {
    Path::new(s).is_absolute()
        || s.starts_with("./")
        || s.starts_with("../")
        || s.ends_with(".sock")
}
