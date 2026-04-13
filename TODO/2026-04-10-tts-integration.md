# TTS Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add text-to-speech support so characters can speak responses aloud, with on-demand replay and live streaming mode.

**Architecture:** The daemon proxies TTS requests to an external ttsd server (OpenAI-compatible API), relaying streamed WAV audio back to clients over SWP. Clients play audio locally via rodio/cpal. Live mode is a daemon-level flag toggled by any client; after generation completes, the daemon auto-speaks to all connected clients.

**Tech Stack:** Rust, reqwest (HTTP streaming), rodio/cpal (audio playback), base64 (SWP encoding), serde (protocol types)

**Spec:** `docs/superpowers/specs/2026-04-10-tts-integration-design.md`

---

## Design Note: Live Mode

The spec proposed `SetLiveSpeak` as a per-connection flag. This plan implements it as a **daemon-level flag** stored on `MessageHandler` (wrapped in `Arc<AtomicBool>` and passed to `GenContext`). Rationale:

- The CLI is stateless (connects, sends, disconnects). Per-connection state doesn't survive across CLI invocations.
- `shore speak on` needs to persist across invocations — a daemon flag does this naturally.
- After generation, the daemon broadcasts audio to all connected clients. The TUI (persistent connection) plays it. A CLI `shore send` session also plays it if still connected.
- Single boolean is the simplest correct implementation. Per-connection tracking can be added later if needed.

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `shore-daemon/src/tts.rs` | TTS HTTP client + WAV relay to SWP (~200 LOC) |
| `shore-client/src/audio.rs` | AudioPlayer + StreamingSource for rodio playback (~150 LOC) |

### Modified Files
| File | Change |
|------|--------|
| `shore-protocol/src/client_msg.rs` | Add `Speak`, `SetLiveSpeak` variants + structs |
| `shore-protocol/src/server_msg.rs` | Add `AudioStart`, `AudioChunk`, `AudioEnd`, `AudioError` variants + structs |
| `shore-config/src/app.rs` | Add `TtsConfig` struct + `tts` field on `AppConfig` |
| `shore-daemon/Cargo.toml` | Add `reqwest` `stream` feature |
| `shore-daemon/src/main.rs` | Pass `TtsConfig` to handler |
| `shore-daemon/src/handler/mod.rs` | Handle `Speak`/`SetLiveSpeak`, add `live_speak` + `tts_config` to `GenContext`, hook TTS after generation |
| `shore-daemon-server/src/lib.rs` | Route `Speak`/`SetLiveSpeak` to engine, update `msg_type_name` |
| `shore-client/Cargo.toml` | Add `rodio` dependency |
| `shore-client/src/lib.rs` | Re-export `audio` module |
| `shore-cli/src/cli.rs` | Add `Speak` subcommand variant |
| `shore-cli/src/run.rs` | Add `speak` handler, update `recv_streaming_response` to handle audio messages |
| `shore-tui/src/input.rs` | Add `:speak` command to `parse_command` |
| `shore-tui/src/app.rs` | Add `audio_player` field, `live_speak` flag, handle audio `ConnEvent`s |
| `shore-tui/Cargo.toml` | (rodio comes transitively via shore-client) |

---

## Task 1: Protocol — Add SWP Message Types for Audio

**Files:**
- Modify: `shore-protocol/src/client_msg.rs:81-90` (ClientMessage enum)
- Modify: `shore-protocol/src/server_msg.rs:125-144` (ServerMessage enum)

- [ ] **Step 1: Add Speak and SetLiveSpeak structs to client_msg.rs**

Add before the `ClientMessage` enum (after the `Cancel` struct at line 79):

```rust
/// Request TTS playback of a message.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Speak {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    /// Message ID to speak. None = last assistant message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
}

/// Toggle live TTS mode.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SetLiveSpeak {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub enabled: bool,
}
```

- [ ] **Step 2: Add variants to ClientMessage enum**

```rust
pub enum ClientMessage {
    Hello(ClientHello),
    Message(ClientMessageBody),
    Regen(Regen),
    Command(Command),
    Cancel(Cancel),
    Speak(Speak),
    SetLiveSpeak(SetLiveSpeak),
}
```

- [ ] **Step 3: Add audio structs to server_msg.rs**

Add before the `ServerMessage` enum (after `CacheWarning` at line 123):

```rust
/// TTS audio stream starting.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioStart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub msg_id: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// TTS audio data chunk (base64-encoded PCM).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioChunk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub data: String,
}

/// TTS audio stream complete.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioEnd {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
}

/// TTS error.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AudioError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    pub message: String,
}
```

- [ ] **Step 4: Add variants to ServerMessage enum**

```rust
pub enum ServerMessage {
    // ... existing variants ...
    CacheWarning(CacheWarning),
    AudioStart(AudioStart),
    AudioChunk(AudioChunk),
    AudioEnd(AudioEnd),
    AudioError(AudioError),
}
```

- [ ] **Step 5: Add serde round-trip tests to client_msg.rs**

Add to the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn speak_serialization_roundtrip() {
    let msg = ClientMessage::Speak(super::Speak {
        rid: Some("r1".into()),
        msg_id: Some("msg-abc".into()),
    });
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["type"], "speak");
    assert_eq!(json["rid"], "r1");
    assert_eq!(json["msg_id"], "msg-abc");

    let roundtrip: ClientMessage = serde_json::from_value(json).unwrap();
    assert!(matches!(roundtrip, ClientMessage::Speak(_)));
}

#[test]
fn speak_no_msg_id_defaults_to_none() {
    let json = serde_json::json!({"type": "speak"});
    let msg: ClientMessage = serde_json::from_value(json).unwrap();
    match msg {
        ClientMessage::Speak(s) => assert!(s.msg_id.is_none()),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn set_live_speak_roundtrip() {
    let msg = ClientMessage::SetLiveSpeak(super::SetLiveSpeak {
        rid: None,
        enabled: true,
    });
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["type"], "set_live_speak");
    assert_eq!(json["enabled"], true);

    let roundtrip: ClientMessage = serde_json::from_value(json).unwrap();
    match roundtrip {
        ClientMessage::SetLiveSpeak(s) => assert!(s.enabled),
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 6: Add serde round-trip tests to server_msg.rs**

Add a `#[cfg(test)] mod tests` block at the end of server_msg.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_start_roundtrip() {
        let msg = ServerMessage::AudioStart(AudioStart {
            rid: Some("r1".into()),
            msg_id: "msg-123".into(),
            sample_rate: 24000,
            channels: 1,
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_start");
        assert_eq!(json["sample_rate"], 24000);
        assert_eq!(json["channels"], 1);

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioStart(_)));
    }

    #[test]
    fn audio_chunk_roundtrip() {
        let msg = ServerMessage::AudioChunk(AudioChunk {
            rid: None,
            data: "AQID".into(),
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_chunk");

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioChunk(_)));
    }

    #[test]
    fn audio_error_roundtrip() {
        let msg = ServerMessage::AudioError(AudioError {
            rid: None,
            message: "voice not found".into(),
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "audio_error");
        assert_eq!(json["message"], "voice not found");

        let roundtrip: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(roundtrip, ServerMessage::AudioError(_)));
    }
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p shore-protocol`
Expected: All new and existing tests pass.

- [ ] **Step 8: Commit**

```bash
git add shore-protocol/src/client_msg.rs shore-protocol/src/server_msg.rs
git commit -m "feat(protocol): add SWP message types for TTS audio streaming"
```

---

## Task 2: Config — Add TtsConfig

**Files:**
- Modify: `shore-config/src/app.rs:20-46` (AppConfig struct)

- [ ] **Step 1: Add TtsConfig struct to app.rs**

Add a new section after the existing config structs (follow the `serde_default!` + struct + `impl Default` pattern used by `DaemonConfig`):

```rust
// ── [tts] ──────────────────────────────────────────────────────────────

serde_default!(default_tts_port -> u16 { 8778 });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// Enable TTS support.
    #[serde(default)]
    pub enabled: bool,

    /// TTS server hostname.
    #[serde(default)]
    pub host: String,

    /// TTS server port (default: 8778).
    #[serde(default = "default_tts_port")]
    pub port: u16,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_tts_port(),
        }
    }
}
```

- [ ] **Step 2: Add tts field to AppConfig**

```rust
pub struct AppConfig {
    // ... existing fields ...

    #[serde(default)]
    pub advanced: AdvancedConfig,

    #[serde(default)]
    pub tts: TtsConfig,
}
```

- [ ] **Step 3: Add config parsing test**

Find the existing test module in `shore-config/src/app.rs` (or add one). Add:

```rust
#[test]
fn tts_config_defaults() {
    let config: AppConfig = toml::from_str("").unwrap();
    assert!(!config.tts.enabled);
    assert_eq!(config.tts.host, "");
    assert_eq!(config.tts.port, 8778);
}

#[test]
fn tts_config_explicit() {
    let config: AppConfig = toml::from_str(
        r#"
        [tts]
        enabled = true
        host = "192.168.1.50"
        port = 9000
        "#,
    )
    .unwrap();
    assert!(config.tts.enabled);
    assert_eq!(config.tts.host, "192.168.1.50");
    assert_eq!(config.tts.port, 9000);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p shore-config`
Expected: All tests pass including new TTS config tests.

- [ ] **Step 5: Commit**

```bash
git add shore-config/src/app.rs
git commit -m "feat(config): add [tts] section with enabled/host/port"
```

---

## Task 3: Daemon — TTS Client Module

**Files:**
- Modify: `shore-daemon/Cargo.toml` (add `stream` feature to reqwest)
- Create: `shore-daemon/src/tts.rs`
- Modify: `shore-daemon/src/main.rs` (add `mod tts;`)

- [ ] **Step 1: Enable reqwest streaming in Cargo.toml**

In `shore-daemon/Cargo.toml`, change the reqwest line:

```toml
reqwest = { version = "0.12", features = ["json", "stream"] }
```

- [ ] **Step 2: Add mod declaration**

In `shore-daemon/src/main.rs`, add near the other `mod` declarations:

```rust
mod tts;
```

- [ ] **Step 3: Create shore-daemon/src/tts.rs**

```rust
//! TTS client — proxies speech requests to an external ttsd server
//! and relays streamed WAV audio as SWP AudioChunk messages.

use base64::Engine as _;
use reqwest::Client;
use shore_config::app::TtsConfig;
use shore_protocol::server_msg::{
    AudioChunk, AudioEnd, AudioError, AudioStart, ServerMessage,
};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

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

    /// Call POST /v1/audio/speech and return the streaming response.
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
/// Calls ttsd, parses the WAV header for metadata, then streams
/// PCM data (header stripped) as base64-encoded AudioChunk messages.
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

    // Read the full response into memory. TTS audio for a single message
    // is typically a few hundred KB — small enough to buffer.
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

    // Parse WAV header for metadata.
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

    debug!(sample_rate, channels, pcm_bytes = bytes.len() - 44, "WAV header parsed");

    // Send AudioStart with metadata.
    let _ = push_tx.send(ServerMessage::AudioStart(AudioStart {
        rid: rid.clone(),
        msg_id: msg_id.to_string(),
        sample_rate,
        channels,
    }));

    // Send PCM data (skip 44-byte WAV header) in chunks.
    let pcm = &bytes[44..];
    let chunk_size = 8192;
    let encoder = base64::engine::general_purpose::STANDARD;
    for chunk in pcm.chunks(chunk_size) {
        let _ = push_tx.send(ServerMessage::AudioChunk(AudioChunk {
            rid: rid.clone(),
            data: encoder.encode(chunk),
        }));
    }

    // Signal completion.
    let _ = push_tx.send(ServerMessage::AudioEnd(AudioEnd { rid }));
    info!(voice, msg_id, "TTS relay complete");
}
```

- [ ] **Step 4: Build verification**

Run: `cargo build -p shore-daemon`
Expected: Compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/Cargo.toml shore-daemon/src/tts.rs shore-daemon/src/main.rs
git commit -m "feat(daemon): add TTS client module with WAV relay"
```

---

## Task 4: Daemon — Handler Integration + Server Routing

**Files:**
- Modify: `shore-daemon-server/src/lib.rs:365-405` (route_client_message) + line 460 (msg_type_name)
- Modify: `shore-daemon/src/handler/mod.rs` (MessageHandler, GenContext, run loop, handle_generation)

- [ ] **Step 1: Route Speak and SetLiveSpeak in shore-daemon-server**

In `shore-daemon-server/src/lib.rs`, update `route_client_message` (line 374). Add `Speak` and `SetLiveSpeak` to the engine routing match arm:

```rust
ClientMessage::Message(_)
| ClientMessage::Regen(_)
| ClientMessage::Cancel(_)
| ClientMessage::Speak(_)
| ClientMessage::SetLiveSpeak(_) => {
    info!(client_id, msg_type = %msg_type_name(&msg), "Routing to engine");
    route_tx
        .send(RoutedMessage::Engine {
            msg,
            character: character.clone(),
        })
        .await?;
}
```

Update `msg_type_name` (line 460) to add the new variants:

```rust
fn msg_type_name(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::Hello(_) => "hello",
        ClientMessage::Message(_) => "message",
        ClientMessage::Regen(_) => "regen",
        ClientMessage::Command(_) => "command",
        ClientMessage::Cancel(_) => "cancel",
        ClientMessage::Speak(_) => "speak",
        ClientMessage::SetLiveSpeak(_) => "set_live_speak",
    }
}
```

- [ ] **Step 2: Add live_speak and tts fields to MessageHandler and GenContext**

In `shore-daemon/src/handler/mod.rs`, add imports at the top:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use crate::tts::{self, TtsClient};
```

Add fields to `MessageHandler` (after `generation_handle`):

```rust
pub struct MessageHandler {
    // ... existing fields ...
    pub generation_handle: Option<tokio::task::JoinHandle<()>>,
    /// Daemon-wide live TTS flag.
    pub live_speak: Arc<AtomicBool>,
    /// TTS client (None if TTS not configured).
    pub tts_client: Option<TtsClient>,
}
```

Add fields to `GenContext`:

```rust
struct GenContext {
    // ... existing fields ...
    notifier: NotificationService,
    /// Daemon-wide live TTS flag.
    live_speak: Arc<AtomicBool>,
    /// TTS client (None if TTS not configured).
    tts_client: Option<TtsClient>,
}
```

Update `gen_context()` to pass the new fields:

```rust
fn gen_context(&self) -> GenContext {
    GenContext {
        // ... existing fields ...
        notifier: self.notifier.clone(),
        live_speak: self.live_speak.clone(),
        tts_client: self.tts_client.clone(),
    }
}
```

- [ ] **Step 3: Handle Speak and SetLiveSpeak in the run loop**

In `MessageHandler::run()`, inside the `RoutedMessage::Engine` match arm (around line 178), add handling for the new message types. Before the existing `if matches!(msg, ClientMessage::Cancel(_))` check:

```rust
// Handle live-speak toggle (no generation needed).
if let ClientMessage::SetLiveSpeak(ref toggle) = msg {
    let prev = self.live_speak.swap(toggle.enabled, Ordering::Relaxed);
    info!(enabled = toggle.enabled, prev, "Live TTS toggled");
    let _ = self.push_tx.send(ServerMessage::CommandOutput(
        shore_protocol::server_msg::CommandOutput {
            name: "set_live_speak".into(),
            data: serde_json::json!({ "enabled": toggle.enabled }),
        },
    ));
    continue;
}

// Handle speak request (spawns TTS task).
if let ClientMessage::Speak(ref speak) = msg {
    if let Some(ref tts_client) = self.tts_client {
        let tts_client = tts_client.clone();
        let push_tx = self.push_tx.clone();
        let registry = self.registry.clone();
        let rid = speak.rid.clone();
        let msg_id = speak.msg_id.clone();
        let character = character.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_speak(
                &tts_client, &push_tx, &registry, rid, msg_id, character.as_deref(),
            ).await {
                error!(error = %e, "TTS speak failed");
            }
        });
    } else {
        let _ = self.push_tx.send(ServerMessage::AudioError(
            shore_protocol::server_msg::AudioError {
                rid: speak.rid.clone(),
                message: "TTS not configured".into(),
            },
        ));
    }
    continue;
}
```

- [ ] **Step 4: Add handle_speak helper function**

Add this function after `handle_generation`:

```rust
/// Handle a Speak request: resolve message text, call TTS, relay audio.
async fn handle_speak(
    tts_client: &TtsClient,
    push_tx: &broadcast::Sender<ServerMessage>,
    registry: &Arc<Mutex<CharacterRegistry>>,
    rid: Option<String>,
    msg_id: Option<String>,
    character: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let registry = registry.lock().await;
    let char_name = registry.resolve_character(character)?;
    let engine = registry.engine(&char_name)?;

    // Resolve message: specific ID or last assistant message.
    let (resolved_id, text) = match msg_id {
        Some(ref id) => {
            let msg = engine
                .messages()
                .iter()
                .find(|m| m.msg_id == *id)
                .ok_or_else(|| format!("message not found: {id}"))?;
            (msg.msg_id.clone(), msg.content.clone())
        }
        None => {
            let msg = engine
                .messages()
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .ok_or("no assistant messages to speak")?;
            (msg.msg_id.clone(), msg.content.clone())
        }
    };

    drop(registry); // Release lock before network I/O.

    if text.is_empty() {
        let _ = push_tx.send(ServerMessage::AudioError(
            shore_protocol::server_msg::AudioError {
                rid,
                message: "message has no text content".into(),
            },
        ));
        return Ok(());
    }

    tts::relay_speech(tts_client, &text, &char_name, &resolved_id, rid, push_tx).await;
    Ok(())
}
```

- [ ] **Step 5: Hook live TTS after generation**

At the end of `handle_generation` (line 717, after `persist_and_notify`), add:

```rust
    // 13. Live TTS: if enabled, speak the generated response.
    if ctx.live_speak.load(Ordering::Relaxed) {
        if let Some(ref tts_client) = ctx.tts_client {
            let text = &result.full_text;
            if !text.is_empty() {
                // Get the msg_id of the just-persisted assistant message.
                let msg_id = engine_arc
                    .lock()
                    .await
                    .messages()
                    .last()
                    .map(|m| m.msg_id.clone())
                    .unwrap_or_default();
                tts::relay_speech(
                    tts_client,
                    text,
                    &char_name,
                    &msg_id,
                    params.rid.clone(),
                    &ctx.push_tx,
                )
                .await;
            }
        }
    }
```

**Note:** The exact field name for the generated text on `StreamResult` may differ — read `shore-llm-client/src/types.rs` to find the correct field (likely `result.content` or similar). Adjust accordingly.

- [ ] **Step 6: Update MessageHandler construction in main.rs**

In `shore-daemon/src/main.rs`, where `MessageHandler` is constructed (around line 202), add the new fields:

```rust
let live_speak = Arc::new(AtomicBool::new(false));
let tts_client = if effective_config.app.tts.enabled && !effective_config.app.tts.host.is_empty() {
    Some(TtsClient::new(&effective_config.app.tts))
} else {
    None
};

let mut msg_handler = MessageHandler {
    // ... existing fields ...
    generation_handle: None,
    live_speak,
    tts_client,
};
```

Add the necessary import at the top of main.rs:

```rust
use std::sync::atomic::AtomicBool;
use crate::tts::TtsClient;
```

- [ ] **Step 7: Build verification**

Run: `cargo build -p shore-daemon-server -p shore-daemon`
Expected: Compiles with no errors. Warnings about unused fields are acceptable at this stage.

- [ ] **Step 8: Commit**

```bash
git add shore-daemon-server/src/lib.rs shore-daemon/src/handler/mod.rs shore-daemon/src/main.rs
git commit -m "feat(daemon): wire TTS into handler with speak and live mode"
```

---

## Task 5: Client — AudioPlayer with rodio

**Files:**
- Modify: `shore-client/Cargo.toml`
- Create: `shore-client/src/audio.rs`
- Modify: `shore-client/src/lib.rs` (add `pub mod audio;`)

- [ ] **Step 1: Add rodio dependency**

In `shore-client/Cargo.toml`, add:

```toml
[dependencies]
# ... existing ...
rodio = { version = "0.19", default-features = false, features = ["wav"] }
```

- [ ] **Step 2: Add module declaration**

In `shore-client/src/lib.rs`, add:

```rust
pub mod audio;
```

- [ ] **Step 3: Create shore-client/src/audio.rs**

```rust
//! Audio playback for TTS streams using rodio.
//!
//! Receives PCM audio data in chunks and plays them progressively
//! via a custom rodio Source backed by a channel-fed buffer.

use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, OutputStreamHandle, Sink};
use tracing::{debug, error, warn};

/// Manages audio playback for TTS streams.
pub struct AudioPlayer {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    sample_rate: u32,
    channels: u16,
}

/// Error creating an AudioPlayer.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("failed to open audio output: {0}")]
    OutputError(String),
    #[error("failed to create audio sink: {0}")]
    SinkError(String),
}

impl AudioPlayer {
    /// Create a new AudioPlayer. Opens the default audio output device.
    pub fn new() -> Result<Self, AudioError> {
        let (stream, handle) = OutputStream::try_default()
            .map_err(|e| AudioError::OutputError(e.to_string()))?;
        Ok(Self {
            _stream: stream,
            handle,
            sink: None,
            sample_rate: 24000,
            channels: 1,
        })
    }

    /// Start a new audio stream with the given format.
    /// Stops any currently playing audio.
    pub fn start(&mut self, sample_rate: u32, channels: u16) {
        // Stop previous playback if any.
        if let Some(old) = self.sink.take() {
            old.stop();
        }

        self.sample_rate = sample_rate;
        self.channels = channels;

        match Sink::try_new(&self.handle) {
            Ok(sink) => {
                debug!(sample_rate, channels, "Audio playback started");
                self.sink = Some(sink);
            }
            Err(e) => {
                error!(error = %e, "Failed to create audio sink");
            }
        }
    }

    /// Feed a base64-encoded PCM chunk into the player.
    ///
    /// Decodes the base64 data, converts from int16 LE PCM to f32 samples,
    /// and appends to the rodio sink for immediate playback.
    pub fn feed(&self, base64_data: &str) {
        let Some(ref sink) = self.sink else {
            warn!("Audio feed called with no active sink");
            return;
        };

        let bytes = match base64::engine::general_purpose::STANDARD.decode(base64_data) {
            Ok(b) => b,
            Err(e) => {
                error!(error = %e, "Failed to decode audio chunk");
                return;
            }
        };

        // Convert int16 LE PCM bytes to f32 samples.
        let samples: Vec<f32> = bytes
            .chunks_exact(2)
            .map(|pair| {
                let sample = i16::from_le_bytes([pair[0], pair[1]]);
                sample as f32 / 32768.0
            })
            .collect();

        if !samples.is_empty() {
            let buffer =
                SamplesBuffer::new(self.channels, self.sample_rate, samples);
            sink.append(buffer);
        }
    }

    /// Signal that no more audio data will arrive.
    /// Playback continues until the buffer drains.
    pub fn finish(&self) {
        debug!("Audio stream finished, draining buffer");
        // Nothing to do — rodio will play remaining buffered samples.
        // The sink stays alive until stop() or drop.
    }

    /// Immediately stop all playback.
    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
            debug!("Audio playback stopped");
        }
    }

    /// Check if audio is currently playing.
    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().map_or(false, |s| !s.empty())
    }

    /// Block until all queued audio finishes playing.
    pub fn wait_until_done(&self) {
        if let Some(ref sink) = self.sink {
            sink.sleep_until_end();
        }
    }
}
```

- [ ] **Step 4: Build verification**

Run: `cargo build -p shore-client`
Expected: Compiles. May need `alsa-lib` dev headers on Linux (`pacman -S alsa-lib`).

- [ ] **Step 5: Commit**

```bash
git add shore-client/Cargo.toml shore-client/src/audio.rs shore-client/src/lib.rs
git commit -m "feat(client): add AudioPlayer with rodio for TTS playback"
```

---

## Task 6: CLI — `shore speak` Subcommand

**Files:**
- Modify: `shore-cli/src/cli.rs` (add Speak variant to CliCommand)
- Modify: `shore-cli/src/run.rs` (add speak handler, update recv_streaming_response)

- [ ] **Step 1: Add Speak subcommand to cli.rs**

In the `CliCommand` enum (after `Regen`):

```rust
/// Speak a message aloud via TTS
Speak {
    /// Message reference (last, -1, 3, etc.) or "on"/"off" for live mode
    #[arg(allow_hyphen_values = true)]
    arg: Option<String>,
},
```

- [ ] **Step 2: Add speak handler in run.rs**

In the `execute` function's `match &cli.command` block, add a new arm:

```rust
CliCommand::Speak { arg } => {
    handle_speak(&mut conn, arg.as_deref(), &character).await?;
}
```

- [ ] **Step 3: Implement handle_speak function in run.rs**

Add the function (and the necessary imports at the top of run.rs):

```rust
use shore_client::audio::AudioPlayer;
use shore_protocol::client_msg::{SetLiveSpeak, Speak as SpeakMsg};
```

```rust
async fn handle_speak(
    conn: &mut SWPConnection,
    arg: Option<&str>,
    character: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match arg {
        Some("on") => {
            conn.send(ClientMessage::SetLiveSpeak(SetLiveSpeak {
                rid: None,
                enabled: true,
            }))
            .await?;
            let _data = recv_command_data(conn).await?;
            eprintln!("Live TTS enabled");
            return Ok(());
        }
        Some("off") => {
            conn.send(ClientMessage::SetLiveSpeak(SetLiveSpeak {
                rid: None,
                enabled: false,
            }))
            .await?;
            let _data = recv_command_data(conn).await?;
            eprintln!("Live TTS disabled");
            return Ok(());
        }
        _ => {}
    }

    // On-demand speak: resolve ref to msg_id, or None for last message.
    let msg_id = match arg {
        Some(msg_ref) => {
            // Use the "get" command to resolve the ref and extract msg_id.
            conn.send_command("get", serde_json::json!({ "ref": msg_ref }))
                .await?;
            let data = recv_command_data(conn).await?;
            data["msg_id"].as_str().map(String::from)
        }
        None => None,
    };

    conn.send(ClientMessage::Speak(SpeakMsg {
        rid: None,
        msg_id,
    }))
    .await?;

    recv_audio_response(conn).await
}

/// Receive and play TTS audio from the daemon.
async fn recv_audio_response(
    conn: &mut SWPConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut player: Option<AudioPlayer> = None;

    loop {
        let msg = conn.recv().await?;
        match msg {
            ServerMessage::AudioStart(start) => {
                match AudioPlayer::new() {
                    Ok(mut p) => {
                        p.start(start.sample_rate, start.channels);
                        player = Some(p);
                    }
                    Err(e) => {
                        eprintln!("Error: failed to open audio: {e}");
                        return Ok(());
                    }
                }
            }
            ServerMessage::AudioChunk(chunk) => {
                if let Some(ref p) = player {
                    p.feed(&chunk.data);
                }
            }
            ServerMessage::AudioEnd(_) => {
                if let Some(ref p) = player {
                    p.finish();
                    p.wait_until_done();
                }
                return Ok(());
            }
            ServerMessage::AudioError(err) => {
                eprintln!("TTS error: {}", err.message);
                return Ok(());
            }
            _ => {
                // Ignore other message types during audio receive.
            }
        }
    }
}
```

- [ ] **Step 4: Update recv_streaming_response to handle audio after StreamEnd**

In the existing `recv_streaming_response` function (line 563), after the `StreamEnd` handling that currently returns, add audio handling for live mode:

```rust
ServerMessage::StreamEnd(end) => {
    spinner.stop().await;
    debug!(finish_reason = end.finish_reason, "Stream complete");
    if end.finish_reason == "tool_use" {
        spinner.restart();
        continue;
    }
    output::print_stream_end(end);
    // Don't return yet — check for live TTS audio.
    // Try to receive one more message with a short timeout.
    // If it's AudioStart, play the audio. Otherwise, we're done.
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        conn.recv(),
    )
    .await
    {
        Ok(Ok(ServerMessage::AudioStart(start))) => {
            match AudioPlayer::new() {
                Ok(mut p) => {
                    p.start(start.sample_rate, start.channels);
                    // Continue receiving audio chunks.
                    loop {
                        let msg = conn.recv().await?;
                        match msg {
                            ServerMessage::AudioChunk(chunk) => p.feed(&chunk.data),
                            ServerMessage::AudioEnd(_) => {
                                p.finish();
                                p.wait_until_done();
                                break;
                            }
                            ServerMessage::AudioError(err) => {
                                eprintln!("TTS error: {}", err.message);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => eprintln!("Warning: could not play TTS audio: {e}"),
            }
        }
        _ => {
            // No audio coming or timeout — normal completion.
        }
    }
    return Ok(());
}
```

- [ ] **Step 5: Build verification**

Run: `cargo build -p shore-cli`
Expected: Compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git add shore-cli/src/cli.rs shore-cli/src/run.rs
git commit -m "feat(cli): add shore speak subcommand with on-demand and live TTS"
```

---

## Task 7: TUI — `:speak` Command

**Files:**
- Modify: `shore-tui/src/app.rs` (add audio_player + live_speak fields, handle audio events)
- Modify: `shore-tui/src/input.rs` (add `:speak` to parse_command)
- Modify: `shore-tui/src/main.rs` (wire audio event handling)

- [ ] **Step 1: Add state to App struct in app.rs**

Read `shore-tui/src/app.rs` to find the `App` struct definition. Add fields:

```rust
use shore_client::audio::AudioPlayer;

pub struct App {
    // ... existing fields ...
    /// TTS audio player (created lazily on first use).
    pub audio_player: Option<AudioPlayer>,
    /// Whether live TTS mode is enabled in this session.
    pub live_speak: bool,
}
```

Initialize in `App::new()` or wherever `App` is constructed:

```rust
audio_player: None,
live_speak: false,
```

- [ ] **Step 2: Add audio event handling method to App**

```rust
impl App {
    /// Handle an audio-related server message.
    pub fn handle_audio_message(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::AudioStart(start) => {
                let player = self.audio_player.get_or_insert_with(|| {
                    AudioPlayer::new().expect("failed to open audio output")
                });
                player.start(start.sample_rate, start.channels);
            }
            ServerMessage::AudioChunk(chunk) => {
                if let Some(ref player) = self.audio_player {
                    player.feed(&chunk.data);
                }
            }
            ServerMessage::AudioEnd(_) => {
                if let Some(ref player) = self.audio_player {
                    player.finish();
                }
            }
            ServerMessage::AudioError(err) => {
                self.set_status(format!("TTS error: {}", err.message));
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 3: Wire audio events in the main event loop**

In `shore-tui/src/main.rs`, find where `ConnEvent::Message(msg)` is handled. Add audio message handling:

```rust
ConnEvent::Message(msg) => {
    match &msg {
        ServerMessage::AudioStart(_)
        | ServerMessage::AudioChunk(_)
        | ServerMessage::AudioEnd(_)
        | ServerMessage::AudioError(_) => {
            app.handle_audio_message(&msg);
        }
        // ... existing StreamStart/StreamChunk/etc. handling ...
```

Also, after handling `StreamEnd` (where `stream_state.active` is set to false), add live speak auto-trigger:

```rust
// After processing StreamEnd:
if app.live_speak {
    let speak_msg = ClientMessage::Speak(shore_protocol::client_msg::Speak {
        rid: None,
        msg_id: None,
    });
    let _ = cmd_tx.send(ConnCommand::Send(speak_msg)).await;
}
```

- [ ] **Step 4: Add :speak command to parse_command in input.rs**

In `shore-tui/src/input.rs`, in the `parse_command` function's match block, add:

```rust
"speak" => {
    match arg {
        "on" => {
            app.live_speak = true;
            app.set_status("Live TTS enabled");
            Action::Send(ConnCommand::Send(ClientMessage::SetLiveSpeak(
                shore_protocol::client_msg::SetLiveSpeak {
                    rid: None,
                    enabled: true,
                },
            )))
        }
        "off" => {
            app.live_speak = false;
            if let Some(ref mut player) = app.audio_player {
                player.stop();
            }
            app.set_status("Live TTS disabled");
            Action::Send(ConnCommand::Send(ClientMessage::SetLiveSpeak(
                shore_protocol::client_msg::SetLiveSpeak {
                    rid: None,
                    enabled: false,
                },
            )))
        }
        "stop" => {
            if let Some(ref mut player) = app.audio_player {
                player.stop();
            }
            app.set_status("Audio stopped");
            Action::Redraw
        }
        _ => {
            // On-demand speak: resolve ref or default to last message.
            let msg_id = if arg.is_empty() {
                None
            } else {
                app.resolve_ref_msg_id(arg)
            };
            Action::Send(ConnCommand::Send(ClientMessage::Speak(
                shore_protocol::client_msg::Speak {
                    rid: None,
                    msg_id,
                },
            )))
        }
    }
}
```

**Note:** `resolve_ref_msg_id` may need to be added to `App` — it should work similarly to the existing `resolve_ref_content` (line 617 in input.rs) but return the `msg_id` instead of content. Read `app.rs` to understand the conversation entry storage and add this method.

- [ ] **Step 5: Add live TTS indicator to status bar**

In `shore-tui/src/ui.rs`, find where the status bar is rendered. Add a `[TTS]` indicator when live speak is enabled:

```rust
// In the status bar rendering section:
if app.live_speak {
    // Append "[TTS]" to the status bar, styled distinctly.
}
```

Read `ui.rs` to find the exact location and follow the existing pattern for status indicators.

- [ ] **Step 6: Add the SetLiveSpeak import to input.rs**

Ensure the import at the top of `input.rs` includes the new types:

```rust
use shore_protocol::client_msg::{Cancel, ClientMessage, ClientMessageBody, Command, Regen};
```

becomes:

```rust
use shore_protocol::client_msg::{Cancel, ClientMessage, ClientMessageBody, Command, Regen, SetLiveSpeak, Speak as SpeakMsg};
```

(Or import them inline where used.)

- [ ] **Step 7: Build verification**

Run: `cargo build -p shore-tui`
Expected: Compiles with no errors.

- [ ] **Step 8: Commit**

```bash
git add shore-tui/src/app.rs shore-tui/src/input.rs shore-tui/src/main.rs shore-tui/src/ui.rs
git commit -m "feat(tui): add :speak command with on-demand and live TTS"
```

---

## Task 8: Integration Test — Live Verification

**Files:** None (test only)

- [ ] **Step 1: Build the full workspace**

Run: `cargo build --workspace`
Expected: Clean build, no errors.

- [ ] **Step 2: Run unit tests**

Run: `cargo test --workspace`
Expected: All tests pass (existing + new protocol/config tests).

- [ ] **Step 3: Verify with real binaries**

Prerequisites:
- ttsd running on the GPU box with at least one named voice
- `[tts]` section configured in `~/.config/shore/config.toml`:
  ```toml
  [tts]
  enabled = true
  host = "<gpu-box-ip>"
  port = 8778
  ```
- Character name matches a voice in ttsd's `--voices-dir`

Test sequence:
1. Start shore-daemon: `cargo run --release -p shore-daemon`
2. On-demand speak: `cargo run --release -p shore-cli -- speak`
   - Expected: Last assistant message is spoken aloud
3. Speak with ref: `cargo run --release -p shore-cli -- speak last`
   - Expected: Same message spoken
4. Enable live mode: `cargo run --release -p shore-cli -- speak on`
5. Send a message: `cargo run --release -p shore-cli -- send "hello"`
   - Expected: Response is printed AND spoken aloud
6. Disable live mode: `cargo run --release -p shore-cli -- speak off`
7. TUI test: `cargo run --release -p shore-tui`, then `:speak`, `:speak on`, `:speak off`

- [ ] **Step 4: Update documentation**

Add a TTS section to `docs/DECISIONS.md`:

```markdown
## TTS Integration (2026-04-10)

**Decision:** TTS implemented as a daemon-proxied relay to an external OpenAI-compatible TTS server (ttsd). Audio streamed to clients over SWP, played locally via rodio.

**Trade-offs:**
- Daemon proxies audio rather than clients calling ttsd directly → respects network topology (laptop can't reach GPU box)
- Live mode is a daemon-level flag, not per-connection → simpler implementation, sufficient for single-user usage
- WAV buffered fully before streaming to clients → simpler than true chunk-by-chunk relay, acceptable latency for typical message lengths
- Voice name = character name by convention → zero config, but requires ttsd voice names to match Shore character names

**Deferred:**
- Per-connection live mode tracking
- True streaming relay (chunk-by-chunk from ttsd → SWP)
- Multiple TTS provider backends (ElevenLabs, OpenAI) — designed for this (OpenAI-compatible API) but only ttsd implemented
- Reference text sidecars for voice cloning backends that need them
```

Add TTS to `docs/ARCHITECTURE.md` data flow section:

```markdown
### TTS Audio Flow

Client ──Speak──> Daemon ──POST /v1/audio/speech──> ttsd (GPU box)
Client <──AudioStart/Chunk/End── Daemon <──WAV stream── ttsd

Audio playback is client-side only (rodio/cpal). The daemon never plays audio.
```

- [ ] **Step 5: Commit documentation**

```bash
git add docs/DECISIONS.md docs/ARCHITECTURE.md
git commit -m "docs: add TTS integration decisions and architecture"
```
