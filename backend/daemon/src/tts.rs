//! TTS client — proxies speech requests to an external ttsd server
//! and relays streamed WAV audio as SWP AudioChunk messages.

use base64::Engine as _;
use reqwest::{Client, StatusCode};
use shore_config::app::TtsConfig;
use shore_protocol::server_msg::{AudioChunk, AudioEnd, AudioError, AudioStart, ServerMessage};
use std::{error::Error, fmt};
use tokio::sync::broadcast;
use tracing::{debug, error, info};

const TTS_RESPONSE_FORMAT: &str = "wav";
const MAX_ERROR_BODY_LEN: usize = 4096;

/// HTTP client for an OpenAI-compatible TTS server.
#[derive(Clone)]
pub struct TtsClient {
    http: Client,
    base_url: String,
}

#[derive(Debug)]
enum TtsRequestError {
    Transport(reqwest::Error),
    Status {
        status: StatusCode,
        url: String,
        body: String,
    },
}

impl fmt::Display for TtsRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(err) => err.fmt(f),
            Self::Status { status, url, body } if body.trim().is_empty() => {
                write!(f, "HTTP status {status} for url ({url})")
            }
            Self::Status { status, url, body } => {
                write!(f, "HTTP status {status} for url ({url}): {}", body.trim())
            }
        }
    }
}

impl Error for TtsRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(err) => Some(err),
            Self::Status { .. } => None,
        }
    }
}

impl From<reqwest::Error> for TtsRequestError {
    fn from(err: reqwest::Error) -> Self {
        Self::Transport(err)
    }
}

impl TtsClient {
    pub fn new(config: &TtsConfig) -> Self {
        let base_url = format!("http://{}:{}", config.host, config.port);
        Self {
            http: Client::new(),
            base_url,
        }
    }

    /// Call POST /v1/audio/speech and return the response.
    async fn speak_raw(
        &self,
        text: &str,
        voice: &str,
        model: &str,
    ) -> Result<reqwest::Response, TtsRequestError> {
        let url = format!("{}/v1/audio/speech", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "model": model,
                "input": text,
                "voice": voice,
                "response_format": TTS_RESPONSE_FORMAT,
            }))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let url = response.url().to_string();
            let body = response
                .text()
                .await
                .unwrap_or_else(|err| format!("<failed to read error body: {err}>"));
            return Err(TtsRequestError::Status {
                status,
                url,
                body: truncate_error_body(&body),
            });
        }

        Ok(response)
    }
}

fn truncate_error_body(body: &str) -> String {
    let trimmed = body.trim();
    let mut chars = trimmed.chars();
    let truncated: String = chars.by_ref().take(MAX_ERROR_BODY_LEN).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

/// Parse a standard 44-byte WAV header.
/// Returns (sample_rate, channels, bits_per_sample).
fn parse_wav_header(buf: &[u8]) -> Result<(u32, u16, u16), &'static str> {
    if buf.len() < 44 {
        return Err("response too small for WAV header");
    }
    if &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err("not a valid WAV file");
    }
    let channels = u16::from_le_bytes([buf[22], buf[23]]);
    let sample_rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let bits_per_sample = u16::from_le_bytes([buf[34], buf[35]]);
    Ok((sample_rate, channels, bits_per_sample))
}

/// Stream TTS audio from ttsd and relay as SWP audio messages.
///
/// Calls ttsd, parses the WAV header for metadata, then sends PCM data
/// (header stripped) as base64-encoded AudioChunk messages. The full
/// response is buffered in memory before chunking — TTS audio for a
/// single message is typically under a few hundred KB, small enough
/// that we skip true streaming for now.
pub async fn relay_speech(
    client: &TtsClient,
    text: &str,
    voice: &str,
    model: &str,
    msg_id: &str,
    rid: Option<String>,
    push_tx: &broadcast::Sender<ServerMessage>,
) {
    info!(voice, model, msg_id, "Starting TTS relay");

    let response = match client.speak_raw(text, voice, model).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "TTS request failed");
            let _ = push_tx.send(ServerMessage::AudioError(AudioError {
                rid,
                message: format!("TTS request failed: {e}"),
            }));
            return;
        }
    };

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to read TTS response body");
            let _ = push_tx.send(ServerMessage::AudioError(AudioError {
                rid,
                message: format!("Failed to read TTS response: {e}"),
            }));
            return;
        }
    };

    let (sample_rate, channels, _bits_per_sample) = match parse_wav_header(&bytes) {
        Ok(info) => info,
        Err(e) => {
            error!(error = e, "Invalid WAV header from TTS server");
            let _ = push_tx.send(ServerMessage::AudioError(AudioError {
                rid,
                message: format!("Invalid WAV from TTS server: {e}"),
            }));
            return;
        }
    };

    debug!(
        sample_rate,
        channels,
        pcm_bytes = bytes.len() - 44,
        "WAV header parsed"
    );

    let _ = push_tx.send(ServerMessage::AudioStart(AudioStart {
        rid: rid.clone(),
        msg_id: msg_id.to_string(),
        sample_rate,
        channels,
    }));

    let pcm = &bytes[44..];
    let chunk_size = 8192;
    let encoder = base64::engine::general_purpose::STANDARD;
    for chunk in pcm.chunks(chunk_size) {
        let _ = push_tx.send(ServerMessage::AudioChunk(AudioChunk {
            rid: rid.clone(),
            data: encoder.encode(chunk),
        }));
    }

    let _ = push_tx.send(ServerMessage::AudioEnd(AudioEnd { rid }));
    info!(voice, model, msg_id, "TTS relay complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn minimal_wav_header(sample_rate: u32, channels: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 44];
        buf[0..4].copy_from_slice(b"RIFF");
        buf[8..12].copy_from_slice(b"WAVE");
        buf[22..24].copy_from_slice(&channels.to_le_bytes());
        buf[24..28].copy_from_slice(&sample_rate.to_le_bytes());
        buf[34..36].copy_from_slice(&16u16.to_le_bytes());
        buf
    }

    #[test]
    fn parse_wav_header_valid() {
        let buf = minimal_wav_header(24000, 1);
        let (sr, ch, bps) = parse_wav_header(&buf).unwrap();
        assert_eq!(sr, 24000);
        assert_eq!(ch, 1);
        assert_eq!(bps, 16);
    }

    #[test]
    fn parse_wav_header_too_small() {
        let buf = vec![0u8; 40];
        assert!(parse_wav_header(&buf).is_err());
    }

    #[test]
    fn parse_wav_header_bad_magic() {
        let mut buf = minimal_wav_header(24000, 1);
        buf[0] = b'X';
        assert!(parse_wav_header(&buf).is_err());
    }

    fn client_for_mock(mock: &MockServer) -> TtsClient {
        let url = reqwest::Url::parse(&mock.uri()).unwrap();
        TtsClient::new(&TtsConfig {
            enabled: true,
            host: url.host_str().unwrap().to_string(),
            port: url.port().unwrap(),
            model: "tts-1".into(),
            voice: None,
        })
    }

    #[tokio::test]
    async fn speak_raw_sends_model_and_requests_wav() {
        let mock = MockServer::start().await;
        let body = serde_json::json!({
            "model": "kokoro",
            "input": "hello",
            "voice": "Nanachan",
            "response_format": "wav",
        });

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&mock)
            .await;

        let client = client_for_mock(&mock);
        let response = client
            .speak_raw("hello", "Nanachan", "kokoro")
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn speak_raw_includes_error_body() {
        let mock = MockServer::start().await;
        let body = serde_json::json!({"error": "unknown voice"});

        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(400).set_body_json(&body))
            .mount(&mock)
            .await;

        let client = client_for_mock(&mock);
        let err = client
            .speak_raw("hello", "Nope", "kokoro")
            .await
            .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("400 Bad Request"));
        assert!(message.contains("unknown voice"));
    }
}
