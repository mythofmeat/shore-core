use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum OutboundClientError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("server not ready")]
    NotReady,
    #[error("tool '{0}' not found")]
    ToolNotFound(String),
    #[error("tool call failed: {0}")]
    CallFailed(String),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("server has been shut down")]
    ShutDown,
}
