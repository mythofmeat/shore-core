use std::path::Path;

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::{AsyncWriteExt, DuplexStream};
use tracing::warn;

use crate::types::{GenerateResponse, ImageGenerateParams, ImageGenerateResponse, LlmRequest};
use crate::LlmError;

use super::{check_response, format_reqwest_error, NON_STREAMING_TIMEOUT};

const SIDECAR_ORIGIN: &str = "http://sidecar";

#[cfg(unix)]
fn sidecar_client(socket_path: &Path) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .unix_socket(socket_path)
        .build()
        .map_err(LlmError::Request)
}

#[cfg(not(unix))]
fn sidecar_client(_socket_path: &Path) -> Result<reqwest::Client, LlmError> {
    Err(LlmError::Provider {
        message: "LLM sidecar transport requires Unix domain sockets".into(),
    })
}

pub(crate) async fn stream(
    request: &LlmRequest,
    socket_path: &Path,
) -> Result<DuplexStream, LlmError> {
    let client = sidecar_client(socket_path)?;
    let response = client
        .post(format!("{SIDECAR_ORIGIN}/v1/stream"))
        .json(request)
        .send()
        .await?;
    let response = check_response(response).await?;

    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut body = response.bytes_stream();
        while let Some(next) = body.next().await {
            match next {
                Ok(bytes) => {
                    if writer.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!(
                        error = %format_reqwest_error(&e),
                        "LLM sidecar stream body read error"
                    );
                    break;
                }
            }
        }
    });

    Ok(reader)
}

pub(crate) async fn generate(
    request: &LlmRequest,
    socket_path: &Path,
) -> Result<GenerateResponse, LlmError> {
    let client = sidecar_client(socket_path)?;
    let response = client
        .post(format!("{SIDECAR_ORIGIN}/v1/generate"))
        .json(request)
        .timeout(NON_STREAMING_TIMEOUT)
        .send()
        .await?;
    let response = check_response(response).await?;
    let body = response.text().await?;
    serde_json::from_str(&body).map_err(LlmError::Deserialize)
}

pub(crate) async fn image_generate(
    params: &ImageGenerateParams<'_>,
    socket_path: &Path,
) -> Result<ImageGenerateResponse, LlmError> {
    let client = sidecar_client(socket_path)?;
    let request = SidecarImageRequest::from(params);
    let response = client
        .post(format!("{SIDECAR_ORIGIN}/v1/image"))
        .json(&request)
        .timeout(NON_STREAMING_TIMEOUT)
        .send()
        .await?;
    let response = check_response(response).await?;
    let body = response.text().await?;
    serde_json::from_str(&body).map_err(LlmError::Deserialize)
}

#[derive(Serialize)]
struct SidecarImageRequest<'a> {
    provider_key: &'a str,
    model: &'a str,
    api_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<&'a str>,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_size: Option<&'a str>,
}

impl<'a> From<&'a ImageGenerateParams<'a>> for SidecarImageRequest<'a> {
    fn from(params: &'a ImageGenerateParams<'a>) -> Self {
        Self {
            provider_key: params.provider_key,
            model: params.model,
            api_key: params.api_key,
            base_url: params.base_url,
            prompt: params.prompt,
            size: params.size,
            quality: params.quality,
            aspect_ratio: params.aspect_ratio,
            image_size: params.image_size,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::oneshot;

    fn test_request() -> LlmRequest {
        LlmRequest {
            sdk: Sdk::Openai,
            model: "openai/gpt-test".into(),
            api_key: "sk-test".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            system: None,
            tools: None,
            max_tokens: 128,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: Some("openai".into()),
            rid: None,
            forensic_character: None,
            retain_long: false,
        }
    }

    async fn serve_once(
        socket_path: &Path,
        status: &str,
        body: String,
    ) -> oneshot::Receiver<(String, String)> {
        let listener = UnixListener::bind(socket_path).unwrap();
        let (tx, rx) = oneshot::channel();
        let status = status.to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, request_body) = read_http_request(&mut stream).await;
            let _ = tx.send((path, request_body));
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        rx
    }

    async fn read_http_request(stream: &mut UnixStream) -> (String, String) {
        let mut buf = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 1024];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "client closed before headers");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
        };

        let headers = String::from_utf8(buf[..header_end].to_vec()).unwrap();
        let path = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_string();
        let content_len = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);

        while buf.len() < header_end + content_len {
            let mut chunk = [0_u8; 1024];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "client closed before body");
            buf.extend_from_slice(&chunk[..n]);
        }

        let body = String::from_utf8(buf[header_end..header_end + content_len].to_vec()).unwrap();
        (path, body)
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[tokio::test]
    async fn generate_posts_request_over_unix_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("llm.sock");
        let response_body = json!({
            "content": "hello",
            "content_blocks": [{"type": "text", "text": "hello"}],
            "finish_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_read_tokens": 0,
                "cache_creation_tokens": 0
            },
            "timing": {"total_ms": 3, "time_to_first_token_ms": 3},
            "model": "openai/gpt-test"
        })
        .to_string();
        let captured = serve_once(&socket, "200 OK", response_body).await;

        let resp = generate(&test_request(), &socket).await.unwrap();
        let (path, body) = captured.await.unwrap();

        assert_eq!(path, "/v1/generate");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["model"],
            "openai/gpt-test"
        );
        assert_eq!(resp.content, "hello");
        assert_eq!(resp.usage.output_tokens, 2);
    }

    #[tokio::test]
    async fn stream_returns_sidecar_ndjson_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("llm.sock");
        let response_body = concat!(
            "{\"type\":\"start\",\"model\":\"m\"}\n",
            "{\"type\":\"done\",\"content\":\"\",\"finish_reason\":\"end_turn\",",
            "\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"cache_read_tokens\":0,\"cache_creation_tokens\":0},",
            "\"timing\":{\"total_ms\":1,\"time_to_first_token_ms\":1}}\n"
        )
        .to_string();
        let captured = serve_once(&socket, "200 OK", response_body).await;

        let mut reader = stream(&test_request(), &socket).await.unwrap();
        let mut body = String::new();
        reader.read_to_string(&mut body).await.unwrap();
        let (path, _) = captured.await.unwrap();

        assert_eq!(path, "/v1/stream");
        assert!(body.contains("\"type\":\"start\""));
        assert!(body.contains("\"type\":\"done\""));
    }

    #[tokio::test]
    async fn image_generate_posts_request_over_unix_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("llm.sock");
        let response_body = json!({
            "url": "https://example.test/image.png",
            "revised_prompt": "a better prompt",
            "timing": {"total_ms": 5}
        })
        .to_string();
        let captured = serve_once(&socket, "200 OK", response_body).await;

        let params = ImageGenerateParams {
            provider_key: "openrouter",
            model: "image-model",
            api_key: "sk-test",
            base_url: Some("https://openrouter.ai/api/v1"),
            prompt: "draw a test",
            size: None,
            quality: None,
            aspect_ratio: Some("16:9"),
            image_size: Some("1024x576"),
        };
        let resp = image_generate(&params, &socket).await.unwrap();
        let (path, body) = captured.await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(path, "/v1/image");
        assert_eq!(body["provider_key"], "openrouter");
        assert_eq!(body["base_url"], "https://openrouter.ai/api/v1");
        assert_eq!(body["aspect_ratio"], "16:9");
        assert_eq!(body["image_size"], "1024x576");
        assert_eq!(resp.url, "https://example.test/image.png");
        assert_eq!(resp.timing.total_ms, 5);
    }

    #[tokio::test]
    async fn non_success_status_maps_to_http_status() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("llm.sock");
        let captured = serve_once(&socket, "429 Too Many Requests", "slow down".into()).await;

        let err = generate(&test_request(), &socket).await.unwrap_err();
        let (path, _) = captured.await.unwrap();

        assert_eq!(path, "/v1/generate");
        match err {
            LlmError::HttpStatus { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body, "slow down");
            }
            other => panic!("expected HttpStatus, got {other:?}"),
        }
    }
}
