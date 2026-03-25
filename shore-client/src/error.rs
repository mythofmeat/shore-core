/// Errors produced by the shore-client library.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection failed: {0}")]
    Connect(String),

    #[error("disconnected from server")]
    Disconnected,

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("discovery error: {0}")]
    Discovery(String),

    #[error("serialization error: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("deserialization error: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ClientError>;
