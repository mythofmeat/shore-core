//! TTS client — proxies speech requests to an external ttsd server
//! and relays streamed WAV audio as SWP AudioChunk messages.

use base64::Engine as _;
use reqwest::Client;
use shore_config::app::TtsConfig;
use shore_protocol::server_msg::{AudioChunk, AudioEnd, AudioError, AudioStart, ServerMessage};
use tokio::sync::broadcast;
use tracing::{debug, error, info};

/// HTTP client for an OpenAI-compatible TTS server.
#[derive(Clone)]
pub struct TtsClient {
    http: Client,
    base_url: String,
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
    ) -> Result<reqwest::Response, reqwest::Error> {
        let url = format!("{}/v1/audio/speech", self.base_url);
        self.http
            .post(&url)
            .json(&serde_json::json!({
                "model": "",
                "input": text,
                "voice": voice,
            }))
            .send()
            .await?
            .error_for_status()
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
    msg_id: &str,
    rid: Option<String>,
    push_tx: &broadcast::Sender<ServerMessage>,
) {
    info!(voice, msg_id, "Starting TTS relay");

    let response = match client.speak_raw(text, voice).await {
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
    info!(voice, msg_id, "TTS relay complete");
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
