pub mod retry;
pub mod stream;
pub mod types;

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, error};

use crate::config::models::ResolvedModel;
use types::LlmRequest;

/// Errors from the LLM client.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("failed to connect to shore-llm at {path}: {source}")]
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("I/O error communicating with shore-llm: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to serialize request: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to parse response: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("HTTP error from shore-llm: {status}")]
    HttpStatus { status: String, body: String },

    #[error("failed to parse HTTP response headers")]
    BadResponse,

    #[error("stream ended without done event")]
    IncompleteStream,

    #[error("API key environment variable {var} is not set")]
    MissingApiKey { var: String },

    #[error("provider error: {message}")]
    Provider { message: String },

    #[error("model refusal detected")]
    Refusal,
}

/// Thin HTTP client that calls shore-llm for completions via Unix socket.
///
/// shore-llm is a stateless HTTP proxy — the daemon sends fully-resolved
/// config per-request (provider, model, API key, options).
#[derive(Debug, Clone)]
pub struct LlmClient {
    socket_path: PathBuf,
}

impl LlmClient {
    /// Create a new client pointing at the given shore-llm Unix socket.
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// The socket path this client connects to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Build an `LlmRequest` from a resolved model profile and conversation state.
    ///
    /// Resolves the API key from the environment variable specified in the model
    /// profile. Returns `LlmError::MissingApiKey` if the variable is not set.
    pub fn build_request(
        model: &ResolvedModel,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        let api_key_env = model
            .api_key_env
            .as_deref()
            .unwrap_or(default_api_key_env(&model.provider));

        let api_key =
            std::env::var(api_key_env).map_err(|_| LlmError::MissingApiKey {
                var: api_key_env.to_string(),
            })?;

        Ok(LlmRequest {
            provider: model.provider.clone(),
            model: model.model_id.clone(),
            api_key,
            base_url: model.base_url.clone(),
            messages,
            system,
            tools,
            max_tokens: model.max_tokens.unwrap_or(4096),
            temperature: model.temperature,
            top_p: model.top_p,
            provider_options,
        })
    }

    /// Send a streaming completion request to shore-llm's POST /v1/stream.
    ///
    /// Returns a `BufReader` over the response body for line-by-line consumption
    /// by the stream consumer. The `rid` is propagated via X-Request-ID header
    /// for structured log tracing.
    pub async fn stream_raw(
        &self,
        request: &LlmRequest,
        rid: Option<&str>,
    ) -> Result<BufReader<UnixStream>, LlmError> {
        let body =
            serde_json::to_string(request).map_err(LlmError::Serialize)?;

        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| LlmError::Connect {
                path: self.socket_path.clone(),
                source: e,
            })?;

        // Build HTTP/1.0 POST request.
        // HTTP/1.0 avoids chunked transfer encoding — body streams until EOF.
        let mut http_request = format!(
            "POST /v1/stream HTTP/1.0\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n",
            body.len()
        );
        if let Some(rid) = rid {
            http_request.push_str(&format!("X-Request-ID: {rid}\r\n"));
        }
        http_request.push_str("\r\n");

        stream.write_all(http_request.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        stream.flush().await?;

        debug!(
            socket = %self.socket_path.display(),
            rid = rid.unwrap_or("-"),
            model = %request.model,
            "Sent streaming request to shore-llm"
        );

        // Read and validate HTTP status line.
        let mut reader = BufReader::new(stream);
        let status_line = read_status_line(&mut reader).await?;

        if !status_line.contains("200") {
            // Read remaining headers + body for error context.
            let body = read_error_body(&mut reader).await;
            return Err(LlmError::HttpStatus {
                status: status_line,
                body,
            });
        }

        // Skip remaining headers (until blank line).
        skip_headers(&mut reader).await?;

        Ok(reader)
    }

    /// Send a non-streaming completion request to shore-llm's POST /v1/generate.
    pub async fn generate(
        &self,
        request: &LlmRequest,
        rid: Option<&str>,
    ) -> Result<types::GenerateResponse, LlmError> {
        let body =
            serde_json::to_string(request).map_err(LlmError::Serialize)?;

        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| LlmError::Connect {
                path: self.socket_path.clone(),
                source: e,
            })?;

        let mut http_request = format!(
            "POST /v1/generate HTTP/1.0\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n",
            body.len()
        );
        if let Some(rid) = rid {
            http_request.push_str(&format!("X-Request-ID: {rid}\r\n"));
        }
        http_request.push_str("\r\n");

        stream.write_all(http_request.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        stream.flush().await?;

        let mut reader = BufReader::new(stream);
        let status_line = read_status_line(&mut reader).await?;

        if !status_line.contains("200") {
            let body = read_error_body(&mut reader).await;
            return Err(LlmError::HttpStatus {
                status: status_line,
                body,
            });
        }

        skip_headers(&mut reader).await?;

        // Read the full JSON body.
        let mut body_buf = String::new();
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            body_buf.push_str(&line);
        }

        serde_json::from_str(&body_buf).map_err(LlmError::Deserialize)
    }
}

/// Return the conventional API key env var name for a provider.
fn default_api_key_env(provider: &str) -> &str {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "zhipuai" => "ZHIPUAI_API_KEY",
        _ => "LLM_API_KEY",
    }
}

/// Read the HTTP status line from a response.
async fn read_status_line(
    reader: &mut BufReader<UnixStream>,
) -> Result<String, LlmError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(LlmError::BadResponse);
    }
    Ok(line)
}

/// Skip HTTP headers until we hit the blank line separating headers from body.
async fn skip_headers(
    reader: &mut BufReader<UnixStream>,
) -> Result<(), LlmError> {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line.trim().is_empty() {
            break;
        }
    }
    Ok(())
}

/// Read remaining headers + body for error reporting.
async fn read_error_body(reader: &mut BufReader<UnixStream>) -> String {
    let mut buf = String::new();
    // Read up to 4KB for error context.
    for _ in 0..100 {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => buf.push_str(&line),
            Err(e) => {
                error!(error = %e, "Error reading response body");
                break;
            }
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::ResolvedModel;

    #[test]
    fn build_request_resolves_api_key() {
        // Set a test env var.
        std::env::set_var("TEST_API_KEY_015", "sk-test-key");

        let model = ResolvedModel {
            name: "test-model".into(),
            provider: "anthropic".into(),
            model_id: "claude-test".into(),
            max_context_tokens: None,
            max_tokens: Some(2048),
            temperature: Some(0.5),
            top_p: None,
            base_url: None,
            api_key_env: Some("TEST_API_KEY_015".into()),
        };

        let req = LlmClient::build_request(
            &model,
            vec![serde_json::json!({"role": "user", "content": "Hi"})],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(req.provider, "anthropic");
        assert_eq!(req.model, "claude-test");
        assert_eq!(req.api_key, "sk-test-key");
        assert_eq!(req.max_tokens, 2048);
        assert_eq!(req.temperature, Some(0.5));
        assert!(req.base_url.is_none());

        std::env::remove_var("TEST_API_KEY_015");
    }

    #[test]
    fn build_request_uses_default_api_key_env() {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

        let model = ResolvedModel {
            name: "test".into(),
            provider: "anthropic".into(),
            model_id: "claude-test".into(),
            max_context_tokens: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            base_url: None,
            api_key_env: None, // Should fall back to ANTHROPIC_API_KEY.
        };

        let req = LlmClient::build_request(
            &model,
            vec![],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(req.api_key, "sk-ant-test");
        assert_eq!(req.max_tokens, 4096); // Default.

        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn build_request_errors_on_missing_api_key() {
        std::env::remove_var("NONEXISTENT_KEY_015");

        let model = ResolvedModel {
            name: "test".into(),
            provider: "anthropic".into(),
            model_id: "claude-test".into(),
            max_context_tokens: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            base_url: None,
            api_key_env: Some("NONEXISTENT_KEY_015".into()),
        };

        let err = LlmClient::build_request(&model, vec![], None, None, None)
            .unwrap_err();
        match err {
            LlmError::MissingApiKey { var } => {
                assert_eq!(var, "NONEXISTENT_KEY_015");
            }
            other => panic!("Expected MissingApiKey, got {:?}", other),
        }
    }

    #[test]
    fn default_api_key_env_for_providers() {
        assert_eq!(default_api_key_env("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(default_api_key_env("openai"), "OPENAI_API_KEY");
        assert_eq!(default_api_key_env("gemini"), "GEMINI_API_KEY");
        assert_eq!(default_api_key_env("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(default_api_key_env("zhipuai"), "ZHIPUAI_API_KEY");
        assert_eq!(default_api_key_env("unknown"), "LLM_API_KEY");
    }

    #[tokio::test]
    async fn stream_raw_connects_and_sends_request() {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("test-llm.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();
        let client = LlmClient::new(socket_path);

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);

            // Read the full request.
            let mut buf = vec![0u8; 4096];
            let n = reader.read(&mut buf).await.unwrap();
            let request_text = String::from_utf8_lossy(&buf[..n]).to_string();

            // Verify it's a POST to /v1/stream.
            assert!(request_text.starts_with("POST /v1/stream HTTP/1.0\r\n"));
            assert!(request_text.contains("Content-Type: application/json"));
            assert!(request_text.contains("X-Request-ID: test-rid-001"));
            assert!(request_text.contains("\"provider\":\"anthropic\""));

            // Send response: status + headers + streaming body.
            let response = "HTTP/1.0 200 OK\r\n\
                           Content-Type: application/x-ndjson\r\n\
                           \r\n\
                           {\"type\":\"start\",\"model\":\"claude-test\"}\n\
                           {\"type\":\"text\",\"text\":\"Hello\"}\n\
                           {\"type\":\"done\",\"content\":\"Hello\",\"finish_reason\":\"end_turn\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"timing\":{\"total_ms\":100}}\n";

            writer.write_all(response.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });

        let request = LlmRequest {
            provider: "anthropic".into(),
            model: "claude-test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![serde_json::json!({"role": "user", "content": "Hi"})],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

        let mut reader = client
            .stream_raw(&request, Some("test-rid-001"))
            .await
            .unwrap();

        // Read the streaming lines.
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 {
                break;
            }
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                lines.push(trimmed);
            }
        }

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"type\":\"start\""));
        assert!(lines[1].contains("\"type\":\"text\""));
        assert!(lines[2].contains("\"type\":\"done\""));

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn stream_raw_handles_http_error() {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("test-llm-err.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();
        let client = LlmClient::new(socket_path);

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);

            let mut buf = vec![0u8; 4096];
            let _ = reader.read(&mut buf).await;

            let response = "HTTP/1.0 500 Internal Server Error\r\n\r\n{\"error\":\"provider timeout\"}";
            writer.write_all(response.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });

        let request = LlmRequest {
            provider: "anthropic".into(),
            model: "claude-test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

        let err = client.stream_raw(&request, None).await.unwrap_err();
        match err {
            LlmError::HttpStatus { status, body } => {
                assert!(status.contains("500"));
                assert!(body.contains("provider timeout"));
            }
            other => panic!("Expected HttpStatus, got {:?}", other),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn generate_parses_response() {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("test-llm-gen.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();
        let client = LlmClient::new(socket_path);

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);

            let mut buf = vec![0u8; 4096];
            let _ = reader.read(&mut buf).await;

            let body = serde_json::json!({
                "content": "Generated text",
                "finish_reason": "end_turn",
                "usage": {"input_tokens": 20, "output_tokens": 10},
                "timing": {"total_ms": 500, "time_to_first_token_ms": 0},
                "model": "claude-test"
            });

            let body_str = body.to_string();
            let response = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body_str.len(),
                body_str
            );

            writer.write_all(response.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });

        let request = LlmRequest {
            provider: "anthropic".into(),
            model: "claude-test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

        let resp = client.generate(&request, Some("gen-rid")).await.unwrap();
        assert_eq!(resp.content, "Generated text");
        assert_eq!(resp.finish_reason, "end_turn");
        assert_eq!(resp.model, "claude-test");
        assert_eq!(resp.usage.input_tokens, 20);
        assert_eq!(resp.timing.total_ms, 500);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn connect_error_on_missing_socket() {
        let client = LlmClient::new(PathBuf::from("/tmp/nonexistent-shore-llm-test.sock"));
        let request = LlmRequest {
            provider: "anthropic".into(),
            model: "test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
        };

        let err = client.stream_raw(&request, None).await.unwrap_err();
        assert!(matches!(err, LlmError::Connect { .. }));
    }
}
