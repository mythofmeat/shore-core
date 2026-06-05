//! Per-call API debug logging.
//!
//! When enabled, every outbound LLM request and its paired response (or error)
//! are dumped as individual JSON files under `{cache_dir}/debug/api_logs/`
//! (per-turn chat traffic) or `{cache_dir}/debug/api_logs_long/` (background
//! tasks flagged with `LlmRequest::retain_long`). Request filename:
//! `{call_id}.json`. Response filename: `{call_id}_response.json`.
//!
//! This is the diagnostic counterpart to the old single `api_payloads.jsonl`:
//! one file per direction per call, so greps stay fast and individual calls
//! can be opened in isolation when a provider (or our code) misbehaves.
//!
//! Splitting into two directories lets operators run different retention
//! policies on each. Per-turn chat payloads churn fast (one user message,
//! one response) and are usually only useful for a few days. Background
//! payloads (compaction, dreaming, heartbeat) are low-frequency and
//! high-value for forensic analysis of cache regressions and memory drift
//! — typically worth keeping for weeks. A representative split:
//!
//! ```sh
//! # chat payloads: 3-day retention
//! find ~/.cache/shore/debug/api_logs/ -type f -mtime +3 -delete
//! # background payloads: 30-day retention
//! find ~/.cache/shore/debug/api_logs_long/ -type f -mtime +30 -delete
//! ```
//!
//! Rotation is intentionally not implemented inside shore-llm — operators
//! own the cron/systemd timers that prune each tier.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use chrono::Local;
use serde::Serialize;
use tokio::io::{AsyncRead, ReadBuf};
use tracing::warn;

use crate::types::{GenerateResponse, LlmRequest};
use crate::LlmError;

const SUBDIR_CHAT: &str = "debug/api_logs";
const SUBDIR_LONG: &str = "debug/api_logs_long";

static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Handle returned by `log_request`, carrying enough state to write the paired
/// response/error file. Opaque to callers.
#[derive(Debug)]
pub struct CallHandle {
    response_path: PathBuf,
    started: std::time::Instant,
    envelope: Envelope,
}

#[derive(Debug, Clone, Serialize)]
struct Envelope {
    ts: String,
    call_id: String,
    sdk: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    character: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rid: Option<String>,
}

/// Compute the api_logs directory under a cache dir.
fn api_logs_dir(cache_dir: &Path, retain_long: bool) -> PathBuf {
    cache_dir.join(if retain_long {
        SUBDIR_LONG
    } else {
        SUBDIR_CHAT
    })
}

fn next_call_id() -> String {
    let now = Local::now();
    let seq = CALL_COUNTER.fetch_add(1, Ordering::Relaxed) % 10_000;
    // Millisecond precision + a rolling 4-digit counter — unique across any
    // realistic rate of concurrent calls inside one daemon process.
    format!("{}-{:04}", now.format("%Y%m%dT%H%M%S%3f"), seq)
}

/// Redact api_key from a serialized `LlmRequest` JSON body.
fn redact_request(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                if obj.contains_key("api_key") {
                    let _ignored = obj.insert("api_key".into(), serde_json::json!("[REDACTED]"));
                }
            }
            serde_json::to_string(&v).unwrap_or_else(|_| body.to_owned())
        }
        Err(_) => body.to_owned(),
    }
}

/// Write the request file and return a handle for the paired response write.
///
/// Returns `None` if logging is disabled (no directory) or the directory
/// cannot be created.
pub fn log_request(
    cache_dir: Option<&Path>,
    request: &LlmRequest,
    body: &str,
) -> Option<CallHandle> {
    let dir = api_logs_dir(cache_dir?, request.retain_long);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(error = %e, path = %dir.display(), "Failed to create api_logs dir");
        return None;
    }

    let call_id = next_call_id();
    let request_path = dir.join(format!("{call_id}.json"));
    let response_path = dir.join(format!("{call_id}_response.json"));

    let envelope = Envelope {
        ts: Local::now().to_rfc3339(),
        call_id: call_id.clone(),
        sdk: format!("{:?}", request.sdk),
        model: request.model.clone(),
        character: request.forensic_character.clone(),
        rid: request.rid.clone(),
    };

    let payload: serde_json::Value = serde_json::from_str(&redact_request(body))
        .unwrap_or_else(|_| serde_json::Value::String(body.to_owned()));

    let doc = serde_json::json!({
        "ts": envelope.ts,
        "direction": "request",
        "call_id": envelope.call_id,
        "sdk": envelope.sdk,
        "model": envelope.model,
        "character": envelope.character,
        "rid": envelope.rid,
        "payload": payload,
    });

    if let Err(e) = write_pretty(&request_path, &doc) {
        warn!(error = %e, path = %request_path.display(), "Failed to write api_log request file");
        return None;
    }

    Some(CallHandle {
        response_path,
        started: std::time::Instant::now(),
        envelope,
    })
}

/// Write the paired response file for a successful non-streaming call.
pub fn log_response(handle: &CallHandle, resp: &GenerateResponse) {
    let duration_ms = crate::convert::elapsed_ms_u64(handle.started.elapsed());
    let doc = serde_json::json!({
        "ts": Local::now().to_rfc3339(),
        "direction": "response",
        "call_id": handle.envelope.call_id,
        "sdk": handle.envelope.sdk,
        "model": handle.envelope.model,
        "character": handle.envelope.character,
        "rid": handle.envelope.rid,
        "duration_ms": duration_ms,
        "response": resp,
    });
    if let Err(e) = write_pretty(&handle.response_path, &doc) {
        warn!(error = %e, path = %handle.response_path.display(), "Failed to write api_log response file");
    }
}

/// Write the paired response file for a failed call.
pub fn log_error(handle: &CallHandle, err: &LlmError) {
    let duration_ms = crate::convert::elapsed_ms_u64(handle.started.elapsed());
    let doc = serde_json::json!({
        "ts": Local::now().to_rfc3339(),
        "direction": "error",
        "call_id": handle.envelope.call_id,
        "sdk": handle.envelope.sdk,
        "model": handle.envelope.model,
        "character": handle.envelope.character,
        "rid": handle.envelope.rid,
        "duration_ms": duration_ms,
        "error": err.to_string(),
        "error_kind": error_kind(err),
    });
    if let Err(e) = write_pretty(&handle.response_path, &doc) {
        warn!(error = %e, path = %handle.response_path.display(), "Failed to write api_log error file");
    }
}

fn error_kind(err: &LlmError) -> &'static str {
    match err {
        LlmError::Request(_) => "request",
        LlmError::HttpStatus { .. } => "http_status",
        LlmError::Serialize(_) => "serialize",
        LlmError::Deserialize(_) => "deserialize",
        LlmError::IncompleteStream => "incomplete_stream",
        LlmError::StreamErrored { .. } => "stream_errored",
        LlmError::MissingApiKey { .. } => "missing_api_key",
        LlmError::Provider { .. } => "provider",
        LlmError::Refusal => "refusal",
    }
}

fn write_pretty(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming tee: wrap an AsyncRead so every byte read also lands in a file.
// ---------------------------------------------------------------------------

/// Wrap a stream reader so every chunk it yields is also written to
/// `{call_id}_response.json`. Used for streaming calls where the response is
/// a sequence of NDJSON events rather than a single value. The file contains
/// one event per line — identical to what the consumer sees on the wire.
///
/// The handle's paired-response write is consumed by this wrapper; failures
/// are logged at warn level and never surface to the caller.
pub struct TeeReader<R> {
    inner: R,
    file: Option<std::fs::File>,
}

impl<R: std::fmt::Debug> std::fmt::Debug for TeeReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeeReader")
            .field("inner", &self.inner)
            .field("file", &self.file)
            .finish()
    }
}

impl<R> TeeReader<R> {
    pub fn new(inner: R, handle: &CallHandle) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&handle.response_path)
            .map_err(|e| {
                warn!(
                    error = %e,
                    path = %handle.response_path.display(),
                    "Failed to open stream tee file"
                );
                e
            })
            .ok();
        // Write a leading envelope line so greps against character/call_id
        // still work even though the body itself is NDJSON.
        if let Some(mut f) = file.as_ref() {
            let header = serde_json::json!({
                "ts": Local::now().to_rfc3339(),
                "direction": "stream_start",
                "call_id": handle.envelope.call_id,
                "sdk": handle.envelope.sdk,
                "model": handle.envelope.model,
                "character": handle.envelope.character,
                "rid": handle.envelope.rid,
            });
            let _ignored = writeln!(f, "{header}");
        }
        Self { inner, file }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for TeeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let after = buf.filled().len();
            if after > before {
                if let Some(f) = self.file.as_mut() {
                    if let Some(new_bytes) = buf.filled().get(before..after) {
                        if let Err(e) = f.write_all(new_bytes) {
                            warn!(error = %e, "Failed to write stream tee chunk");
                        }
                    }
                }
            }
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_config::models::Sdk;
    use tempfile::tempdir;

    fn make_request(retain_long: bool) -> LlmRequest {
        LlmRequest {
            sdk: Sdk::Anthropic,
            model: "m".into(),
            api_key: "k".into(),
            api_key_name: None,
            base_url: None,
            messages: vec![],
            system: None,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: Some("anthropic".into()),
            rid: None,
            forensic_character: None,
            retain_long,
            keepalive_interval: None,
        }
    }

    #[test]
    fn chat_payloads_land_in_short_retention_dir() {
        let tmp = tempdir().unwrap();
        let req = make_request(false);
        let handle = log_request(Some(tmp.path()), &req, "{}").expect("handle");
        assert!(handle
            .response_path
            .starts_with(tmp.path().join(SUBDIR_CHAT)));
        assert!(!handle
            .response_path
            .starts_with(tmp.path().join(SUBDIR_LONG)));
    }

    #[test]
    fn flagged_payloads_land_in_long_retention_dir() {
        let tmp = tempdir().unwrap();
        let req = make_request(true);
        let handle = log_request(Some(tmp.path()), &req, "{}").expect("handle");
        assert!(handle
            .response_path
            .starts_with(tmp.path().join(SUBDIR_LONG)));
        assert!(!handle
            .response_path
            .starts_with(tmp.path().join(SUBDIR_CHAT)));
    }
}
