//! Cache forensics — on-disk diagnostic log for every Anthropic cache event.
//!
//! Writes append-only JSONL to `{data_dir}/cache_forensics.jsonl`.
//! Enabled at startup via `enable()`.  Each Anthropic API call produces
//! a "request" entry (breakpoint placement) and, if the response has
//! cache_creation_tokens > 0, a "cache_write" entry.  Both share the
//! same `call_id` for correlation.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use serde_json::json;

static FORENSIC_DIR: OnceLock<PathBuf> = OnceLock::new();
static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Enable cache forensics.  Call once at startup with the data directory.
pub fn enable(dir: PathBuf) {
    let _ = FORENSIC_DIR.set(dir);
}

/// Whether forensics is enabled.
pub fn is_enabled() -> bool {
    FORENSIC_DIR.get().is_some()
}

/// Allocate a monotonic call ID for correlating request/response entries.
pub fn next_call_id() -> u64 {
    CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Write an arbitrary JSON entry to the forensic log.
pub fn write_entry(entry: &serde_json::Value) {
    let Some(dir) = FORENSIC_DIR.get() else {
        return;
    };
    let path = dir.join("cache_forensics.jsonl");
    let line = entry.to_string();

    // Best-effort append — don't crash or block the main path on I/O errors.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Log the request-side cache placement for an Anthropic call.
pub fn log_request(
    call_id: u64,
    character: Option<&str>,
    model: &str,
    msg_count: usize,
    breakpoint_pos: Option<usize>,
    sys_blocks: usize,
    prefix_hash: u64,
    has_existing_markers: bool,
    cache_enabled: bool,
    rid: Option<&str>,
) {
    let ts = chrono::Local::now().to_rfc3339();
    write_entry(&json!({
        "ts": ts,
        "type": "request",
        "call_id": call_id,
        "character": character,
        "model": model,
        "msg_count": msg_count,
        "breakpoint_pos": breakpoint_pos,
        "sys_blocks": sys_blocks,
        "prefix_hash": format!("{prefix_hash:016x}"),
        "has_existing_markers": has_existing_markers,
        "cache_enabled": cache_enabled,
        "rid": rid,
    }));
}

/// Log the response-side cache event.
pub fn log_response(
    call_id: u64,
    model: &str,
    character: &str,
    call_type: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
) {
    let ts = chrono::Local::now().to_rfc3339();
    write_entry(&json!({
        "ts": ts,
        "type": "response",
        "call_id": call_id,
        "model": model,
        "character": character,
        "call_type": call_type,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_tokens": cache_read_tokens,
        "cache_creation_tokens": cache_creation_tokens,
    }));
}

/// Fire a desktop notification for a cache anomaly.
///
/// Spawns `notify-send` in the background — best-effort, never blocks.
pub fn notify_anomaly(
    character: &str,
    anomaly: &str,
    call_type: &str,
    cache_read: u32,
    cache_write: u32,
) {
    if !is_enabled() {
        return;
    }
    let summary = format!("shore: cache {anomaly}");
    let body = format!(
        "{character} ({call_type})\nread={cache_read} write={cache_write}"
    );
    let _ = std::process::Command::new("notify-send")
        .args(["--urgency=critical", "--app-name=shore", &summary, &body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
