use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::DuplexStream;

use super::sse::SseEvent;
use super::stream_helpers::{
    apply_common_params, build_done_event, build_start_event, build_tool_use_event,
    extract_anthropic_usage, extract_system_text, merge_anthropic_usage,
};
use crate::types::{ContentBlock, GenerateResponse, LlmRequest, Timing, Usage};
use crate::LlmError;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Effort values that map to adaptive thinking + output_config on Anthropic.
const ANTHROPIC_EFFORT_VALUES: &[&str] = &["max", "high", "medium", "low"];

fn is_effort_value(s: &str) -> bool {
    ANTHROPIC_EFFORT_VALUES.contains(&s)
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Returns true if `msg` is a user message whose content is an array where
/// every block has `"type": "tool_result"`.
fn is_tool_result_message(msg: &Value) -> bool {
    if msg.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(arr) = msg.get("content").and_then(Value::as_array) else {
        return false;
    };
    !arr.is_empty()
        && arr
            .iter()
            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
}

/// Strip all `cache_control` keys from every content block in `messages`.
fn strip_cache_control(messages: &mut [Value]) {
    for msg in messages.iter_mut() {
        let Some(arr) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in arr.iter_mut() {
            if let Some(obj) = block.as_object_mut() {
                obj.remove("cache_control");
            }
        }
    }
}

/// Check whether any message already has cache_control markers on its content
/// blocks.  Used to detect tool-loop continuations where breakpoints were
/// already placed on the first call of the turn.
fn messages_have_cache_control(messages: &[Value]) -> bool {
    messages.iter().any(|m| {
        m.get("content")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(|b| b.get("cache_control").is_some()))
    })
}

/// Find the turn-boundary position for a cache breakpoint.
///
/// A "turn" is a real user message plus everything through the final
/// assistant response (including any tool-use exchanges).  `depth` counts
/// in turns, not messages.  The breakpoint lands on the last message of the
/// turn that is `depth` turns before the current one — always at a stable
/// turn boundary regardless of how many tool exchanges occurred within each
/// turn.
fn find_turn_boundary(messages: &[Value], depth: usize) -> Option<usize> {
    let mut real_user_count = 0;
    for i in (0..messages.len()).rev() {
        if messages[i].get("role").and_then(Value::as_str) == Some("user")
            && !is_tool_result_message(&messages[i])
        {
            real_user_count += 1;
            // depth+1: skip the current turn, then count `depth` more turns.
            if real_user_count == depth + 1 && i > 0 {
                // Place the breakpoint on the message just before this turn's
                // user message — that's the last message of the preceding turn.
                return Some(i - 1);
            }
        }
    }
    None
}

/// Build a cache_control value, including TTL if non-default.
fn make_cache_control(cache_ttl: &str) -> Value {
    let mut cc = json!({ "type": "ephemeral" });
    if !cache_ttl.is_empty() && cache_ttl != "5m" {
        cc["ttl"] = json!(cache_ttl);
    }
    cc
}

/// Apply cache_control breakpoint to the last text block of a message.
fn apply_breakpoint_to_message(msg: &mut Value, cc: &Value) {
    let content = msg.get("content").cloned();
    match content {
        Some(Value::String(text)) => {
            msg["content"] = json!([{
                "type": "text",
                "text": text,
                "cache_control": cc,
            }]);
        }
        Some(Value::Array(_)) => {
            if let Some(arr) = msg.get_mut("content").and_then(Value::as_array_mut) {
                // Find last text block and apply cache_control.
                for block in arr.iter_mut().rev() {
                    if let Some(obj) = block.as_object_mut() {
                        let btype = obj.get("type").and_then(Value::as_str).unwrap_or("");
                        if btype == "text" || btype == "tool_result" {
                            obj.insert("cache_control".into(), cc.clone());
                            break;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Apply cache_control breakpoints to messages and system.
///
/// Places up to 4 breakpoints:
/// - **System breakpoint:** on the second-to-last system block (caches the
///   stable system content before the recap, which changes on compaction).
/// - **Message breakpoints (up to 3):** at depths 2, 4, and 8 from the last
///   real user message, each at a stable turn boundary.  Duplicate positions
///   are deduplicated.
/// Info returned alongside the cache-annotated payload for forensics.
struct CachePlacement {
    breakpoint_pos: Option<usize>,
    sys_blocks: usize,
    prefix_hash: u64,
}

fn apply_cache_control(messages: &[Value], system: &Value, cache_ttl: &str) -> (Vec<Value>, Value, CachePlacement) {
    if messages.is_empty() {
        return (messages.to_vec(), system.clone(), CachePlacement {
            breakpoint_pos: None,
            sys_blocks: 0,
            prefix_hash: 0,
        });
    }

    let mut result: Vec<Value> = messages.to_vec();

    // Step 1: Strip all existing cache_control from all content blocks.
    strip_cache_control(&mut result);

    // Step 2: Normalize all plain-string content to array format so the
    // serialised payload structure is identical regardless of where the
    // cache breakpoint lands.  Without this, breakpoint placement converts
    // string → array, and when the breakpoint shifts on the next turn the
    // message reverts to string — changing the prefix bytes and busting
    // every cache entry.
    for msg in result.iter_mut() {
        if let Some(Value::String(text)) = msg.get("content").cloned() {
            msg["content"] = json!([{ "type": "text", "text": text }]);
        }
    }

    // Step 3: Find the last real (non-tool-result) user message.
    let _last_real_user: Option<usize> = result.iter().rposition(|m| {
        m.get("role").and_then(Value::as_str) == Some("user") && !is_tool_result_message(m)
    });

    // Step 4: Single message breakpoint at depth 2.
    let boundary_pos = find_turn_boundary(&result, 2);
    {
        let cc = make_cache_control(cache_ttl);
        if let Some(pos) = boundary_pos {
            apply_breakpoint_to_message(&mut result[pos], &cc);
        }
    }

    // Compute a FNV-style hash of the prefix (system + messages up to breakpoint)
    // so we can detect byte-level differences between calls.
    let prefix_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Hash the system content.
        system.to_string().hash(&mut hasher);
        // Hash messages up to and including the breakpoint.
        let end = boundary_pos.map(|p| p + 1).unwrap_or(0);
        for msg in &result[..end] {
            msg.to_string().hash(&mut hasher);
        }
        hasher.finish()
    };

    let sys_blocks = system.as_array().map(|a| a.len()).unwrap_or(0);

    tracing::debug!(
        total_messages = result.len(),
        boundary_pos = ?boundary_pos,
        prefix_hash = format!("{prefix_hash:016x}"),
        "apply_cache_control: breakpoint placement"
    );

    // System breakpoint: cache the stable system content.  The recap
    // (if present) is always the last block and changes on compaction,
    // so place the breakpoint on the block just before it.  When there
    // is no recap (≤1 block, or all blocks are stable), cache everything.
    let cc_sys = make_cache_control(cache_ttl);
    let mut sys = system.clone();
    if let Some(arr) = sys.as_array_mut() {
        let target_idx = if arr.len() >= 2 {
            arr.len() - 2
        } else if arr.len() == 1 {
            0
        } else {
            usize::MAX
        };
        if let Some(obj) = arr.get_mut(target_idx).and_then(Value::as_object_mut) {
            obj.insert("cache_control".into(), cc_sys);
        }
    }

    (result, sys, CachePlacement { breakpoint_pos: boundary_pos, sys_blocks, prefix_hash })
}

/// Build the `thinking` config param from provider_options.
fn build_thinking_config(opts: &Value) -> Option<Value> {
    let effort = opts.get("reasoning_effort").and_then(Value::as_str);

    // "adaptive" or named effort values → adaptive thinking mode.
    if effort == Some("adaptive") || effort.is_some_and(is_effort_value) {
        return Some(json!({ "type": "adaptive" }));
    }

    // Explicit budget: enable thinking with the given token budget.
    let thinking_flag = opts
        .get("thinking")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let budget = opts.get("budget_tokens").and_then(Value::as_u64);

    if thinking_flag || budget.is_some() {
        return Some(json!({
            "type": "enabled",
            "budget_tokens": budget.unwrap_or(1024)
        }));
    }

    None
}

/// Build `output_config` for named effort levels.
fn build_output_config(opts: &Value) -> Option<Value> {
    let effort = opts.get("reasoning_effort").and_then(Value::as_str)?;
    if is_effort_value(effort) {
        Some(json!({ "effort": effort }))
    } else {
        None
    }
}

/// Extract plain text from a message's content (string or array of blocks).
fn extract_text_content(msg: &Value) -> String {
    extract_system_text(msg.get("content").unwrap_or(&Value::Null))
}

/// Convert inline system-role messages to user messages.
///
/// The Anthropic API does not support `role: "system"` in the messages array,
/// so we wrap the instruction in `<system_instruction>` XML tags and emit it
/// as a user message.  When the preceding message is already a user turn,
/// the instruction is merged into it to avoid consecutive user roles.
fn convert_inline_system_messages(messages: &[Value]) -> Vec<Value> {
    let has_system = messages
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("system"));
    if !has_system {
        return messages.to_vec();
    }

    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) == Some("system") {
            let text = extract_text_content(msg);
            let wrapped = format!("<system_instruction>{text}</system_instruction>");

            // If the previous message is a user message, merge to avoid
            // consecutive user roles (which the API rejects).
            if let Some(prev) = out.last_mut() {
                if prev.get("role").and_then(Value::as_str) == Some("user") {
                    let prev_text = extract_text_content(prev);
                    prev["content"] = json!(format!("{prev_text}\n\n{wrapped}"));
                    continue;
                }
            }

            out.push(json!({ "role": "user", "content": wrapped }));
        } else {
            out.push(msg.clone());
        }
    }
    out
}

/// Construct the JSON request body for the Anthropic Messages API.
///
/// Returns `(body, call_id)` where `call_id` is a forensic correlation ID
/// (0 when forensics is disabled).
fn build_body(request: &LlmRequest, streaming: bool) -> (Value, u64) {
    let opts = request.provider_options.as_ref();
    let empty_opts = json!({});
    let opts_ref = opts.unwrap_or(&empty_opts);

    // Convert inline system-role messages to user/assistant pairs before
    // any further processing (Anthropic rejects role: "system" in messages).
    let converted_messages = convert_inline_system_messages(&request.messages);

    // Cache is enabled when cache_ttl is present and non-empty.
    let cache_ttl = opts_ref
        .get("cache_ttl")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cache_enabled = !cache_ttl.is_empty();
    let has_existing_markers = messages_have_cache_control(&converted_messages);

    let msg_count = converted_messages.len();

    let (messages, system, placement) = if cache_enabled && !has_existing_markers {
        apply_cache_control(
            &converted_messages,
            request.system.as_ref().unwrap_or(&json!(null)),
            cache_ttl,
        )
    } else {
        (
            converted_messages,
            request.system.clone().unwrap_or(json!(null)),
            CachePlacement { breakpoint_pos: None, sys_blocks: 0, prefix_hash: 0 },
        )
    };

    // -- Cache forensics: log every Anthropic request --
    let call_id = if crate::cache_forensics::is_enabled() {
        let id = crate::cache_forensics::next_call_id();
        crate::cache_forensics::log_request(
            id,
            request.forensic_character.as_deref(),
            &request.model,
            msg_count,
            placement.breakpoint_pos,
            placement.sys_blocks,
            placement.prefix_hash,
            has_existing_markers,
            cache_enabled,
            request.rid.as_deref(),
        );
        id
    } else {
        0
    };

    let thinking = build_thinking_config(opts_ref);
    let output_config = build_output_config(opts_ref);

    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "messages": messages,
    });

    if streaming {
        body["stream"] = json!(true);
    }

    // Use the (possibly cache-annotated) system value.
    if !system.is_null() {
        body["system"] = system;
    }

    if let Some(ref tools) = request.tools {
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
    }

    apply_common_params(&mut body, request);

    if let Some(thinking) = thinking {
        body["thinking"] = thinking;
    }

    if let Some(output_config) = output_config {
        body["output_config"] = output_config;
    }

    (body, call_id)
}

/// Build the reqwest request with Anthropic-specific headers.
///
/// Accepts any `base_url`, enabling the Anthropic wire protocol through
/// third-party gateways (e.g. OpenRouter, Bedrock proxies).
/// Returns `(request_builder, forensic_call_id)`.
fn build_http_request(
    client: &reqwest::Client,
    request: &LlmRequest,
    streaming: bool,
) -> Result<(reqwest::RequestBuilder, u64), LlmError> {
    let base = request
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL);
    // If the base_url already includes a version path (e.g. OpenRouter's
    // default "https://openrouter.ai/api/v1"), just append /messages.
    let url = if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    };
    let (body, call_id) = build_body(request, streaming);

    // Log the post-transformation payload shape (no message content).
    {
        let model = body["model"].as_str().unwrap_or("?");
        let max_tokens = body["max_tokens"].as_u64().unwrap_or(0);
        let thinking = body.get("thinking").map(|v| v.to_string()).unwrap_or_else(|| "none".into());
        let output_cfg = body.get("output_config").map(|v| v.to_string()).unwrap_or_else(|| "none".into());
        let sys_blocks = body["system"].as_array().map(|a| a.len()).unwrap_or(0);
        let msg_count = body["messages"].as_array().map(|a| a.len()).unwrap_or(0);
        let tool_count = body.get("tools").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        tracing::debug!(
            model, max_tokens, %thinking, output_config = %output_cfg,
            sys_blocks, msg_count, tool_count,
            "Anthropic: transformed request body"
        );
    }

    let mut builder = client
        .post(&url)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .header("x-api-key", &request.api_key);

    if let Some(ref rid) = request.rid {
        builder = builder.header("X-Request-ID", rid);
    }

    Ok((builder.json(&body), call_id))
}

use super::check_response;

// ── Streaming ────────────────────────────────────────────────────────────

/// State tracked across SSE events during stream translation.
struct StreamState {
    /// Accumulated text content (concatenation of all text_delta texts).
    text_content: String,
    /// Finish reason from message_delta.
    finish_reason: String,
    /// Token usage (input from message_start, output from message_delta).
    usage: Usage,
    /// Model name from message_start.
    model: String,
    /// Start time for timing calculation.
    start_time: Instant,
    /// Time to first token.
    first_token_time: Option<Instant>,

    // Track active tool_use blocks by index.
    tool_blocks: HashMap<u64, ToolBlockState>,
    // Track thinking block indices.
    thinking_blocks: HashSet<u64>,
    // Track accumulated signatures for thinking blocks.
    thinking_signatures: HashMap<u64, String>,
    // Track redacted_thinking blocks (arrive complete at content_block_start).
    redacted_thinking_blocks: HashMap<u64, String>,
}

struct ToolBlockState {
    id: String,
    name: String,
    json_chunks: Vec<String>,
}

impl StreamState {
    fn new(model: &str) -> Self {
        Self {
            text_content: String::new(),
            finish_reason: "end_turn".into(),
            usage: Usage::default(),
            model: model.to_string(),
            start_time: Instant::now(),
            first_token_time: None,
            tool_blocks: HashMap::new(),
            thinking_blocks: HashSet::new(),
            thinking_signatures: HashMap::new(),
            redacted_thinking_blocks: HashMap::new(),
        }
    }
}

/// Send a streaming request to the Anthropic Messages API.
///
/// Returns the read half of a `DuplexStream` that yields NDJSON `StreamEvent`
/// lines. A background task reads SSE events from the HTTP response and
/// translates them into the normalized stream format.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let (http_req, _call_id) = build_http_request(client, request, true)?;
    let response = http_req.send().await.map_err(|e| LlmError::Provider {
        message: format!("HTTP request failed: {e}"),
    })?;
    let response = check_response(response).await?;

    let (read_half, mut write_half) = tokio::io::duplex(64 * 1024);

    let model = request.model.clone();
    tokio::spawn(async move {
        let mut state = StreamState::new(&model);

        let result = super::sse::read_sse_events(
            response,
            |event| handle_sse_event(event, &mut state),
            &mut write_half,
        )
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "SSE stream error in Anthropic provider");
        }
        // Drop write_half so the reader sees EOF.
    });

    Ok(read_half)
}

/// Translate an SSE event into an optional NDJSON line to write to the stream.
fn handle_sse_event(event: SseEvent, state: &mut StreamState) -> Option<String> {
    // Prefer the SSE `event:` field; fall back to the `type` field inside the
    // JSON `data:` payload.  Some proxies (e.g. OpenRouter) may strip the
    // `event:` line and only forward `data:`.
    let event_type_owned: Option<String> = event.event.clone().or_else(|| {
        serde_json::from_str::<serde_json::Value>(&event.data)
            .ok()
            .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
    });
    let event_type = event_type_owned.as_deref().unwrap_or("");

    match event_type {
        "message_start" => handle_message_start(&event.data, state),
        "content_block_start" => handle_content_block_start(&event.data, state),
        "content_block_delta" => handle_content_block_delta(&event.data, state),
        "content_block_stop" => handle_content_block_stop(&event.data, state),
        "message_delta" => {
            handle_message_delta(&event.data, state);
            None
        }
        "message_stop" => Some(handle_message_stop(state)),
        "ping" => None,
        other => {
            if !other.is_empty() {
                tracing::debug!(event_type = other, "Unknown Anthropic SSE event type");
            }
            None
        }
    }
}

fn handle_message_start(data: &str, state: &mut StreamState) -> Option<String> {
    let parsed: Value = serde_json::from_str(data).ok()?;
    let message = parsed.get("message")?;

    if let Some(model) = message.get("model").and_then(Value::as_str) {
        state.model = model.to_string();
    }

    if let Some(usage) = message.get("usage") {
        merge_anthropic_usage(&mut state.usage, usage);
    }

    Some(build_start_event(&state.model))
}

fn handle_content_block_start(data: &str, state: &mut StreamState) -> Option<String> {
    let parsed: Value = serde_json::from_str(data).ok()?;
    let index = parsed.get("index").and_then(Value::as_u64)?;
    let block = parsed.get("content_block")?;
    let block_type = block.get("type").and_then(Value::as_str)?;

    match block_type {
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            state.tool_blocks.insert(
                index,
                ToolBlockState {
                    id,
                    name,
                    json_chunks: Vec::new(),
                },
            );
            None
        }
        "thinking" => {
            state.thinking_blocks.insert(index);
            None
        }
        "redacted_thinking" => {
            let data_str = block
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // OpenRouter injects bogus redacted_thinking blocks that duplicate
            // real thinking content. Filter them at the source.
            if data_str.starts_with("openrouter.reasoning:") {
                tracing::debug!("Filtered OpenRouter redacted_thinking wrapper");
                return None;
            }
            state.redacted_thinking_blocks.insert(index, data_str);
            None
        }
        _ => None,
    }
}

fn handle_content_block_delta(data: &str, state: &mut StreamState) -> Option<String> {
    let parsed: Value = serde_json::from_str(data).ok()?;
    let index = parsed.get("index").and_then(Value::as_u64)?;
    let delta = parsed.get("delta")?;
    let delta_type = delta.get("type").and_then(Value::as_str)?;

    match delta_type {
        "text_delta" => {
            if state.first_token_time.is_none() {
                state.first_token_time = Some(Instant::now());
            }
            let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
            state.text_content.push_str(text);
            Some(json!({ "type": "text", "text": text }).to_string())
        }
        "thinking_delta" => {
            if state.first_token_time.is_none() {
                state.first_token_time = Some(Instant::now());
            }
            let thinking = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
            Some(json!({ "type": "thinking", "text": thinking }).to_string())
        }
        "signature_delta" => {
            if state.thinking_blocks.contains(&index) {
                let sig = delta.get("signature").and_then(Value::as_str).unwrap_or("");
                state
                    .thinking_signatures
                    .entry(index)
                    .or_default()
                    .push_str(sig);
            }
            None
        }
        "input_json_delta" => {
            if let Some(tool) = state.tool_blocks.get_mut(&index) {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                tool.json_chunks.push(partial.to_string());
            }
            None
        }
        _ => None,
    }
}

fn handle_content_block_stop(data: &str, state: &mut StreamState) -> Option<String> {
    let parsed: Value = serde_json::from_str(data).ok()?;
    let index = parsed.get("index").and_then(Value::as_u64)?;

    // Collect all lines to emit — we may have up to 2 (tool_use + thinking_sig,
    // or redacted_thinking + thinking_sig, etc.). We return only the first one
    // from this callback; for simplicity, since the SSE parser writes one line
    // per callback return, we combine multiple events into one if needed.
    // Actually, the callback can only return one line. We need to handle the
    // case where content_block_stop can emit multiple events. Looking at the TS,
    // it checks tool_use, then thinking, then redacted_thinking — these are
    // mutually exclusive block types, so we'll only ever emit one line per stop.

    // Check for tool_use block completion.
    if let Some(tool) = state.tool_blocks.remove(&index) {
        let raw = tool.json_chunks.join("");
        let input: Value = if raw.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw).unwrap_or(json!({}))
        };
        return Some(build_tool_use_event(&tool.id, &tool.name, &input));
    }

    // Check for thinking block completion — emit accumulated signature.
    if state.thinking_blocks.remove(&index) {
        if let Some(sig) = state.thinking_signatures.remove(&index) {
            return Some(json!({ "type": "thinking_signature", "signature": sig }).to_string());
        }
        return None;
    }

    // Check for redacted_thinking block completion.
    if let Some(redacted_data) = state.redacted_thinking_blocks.remove(&index) {
        return Some(json!({ "type": "redacted_thinking", "data": redacted_data }).to_string());
    }

    None
}

fn handle_message_delta(data: &str, state: &mut StreamState) {
    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
        return;
    };

    if let Some(delta) = parsed.get("delta") {
        if let Some(reason) = delta.get("stop_reason").and_then(Value::as_str) {
            state.finish_reason = reason.to_string();
        }
    }

    if let Some(usage) = parsed.get("usage") {
        merge_anthropic_usage(&mut state.usage, usage);
    }
}

fn handle_message_stop(state: &mut StreamState) -> String {
    let total_ms = state.start_time.elapsed().as_millis() as u32;
    let ttft_ms = state
        .first_token_time
        .map(|t| t.duration_since(state.start_time).as_millis() as u32)
        .unwrap_or(total_ms);
    build_done_event(
        &state.text_content,
        &state.finish_reason,
        &state.usage,
        total_ms,
        ttft_ms,
    )
}

// ── Non-streaming ────────────────────────────────────────────────────────

/// Send a non-streaming generate request to the Anthropic Messages API.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let start = Instant::now();
    let (http_req, call_id) = build_http_request(client, request, false)?;
    let response = http_req.send().await.map_err(|e| LlmError::Provider {
        message: format!("HTTP request failed: {e}"),
    })?;
    let response = check_response(response).await?;

    let body: Value = response.json().await.map_err(|e| LlmError::Provider {
        message: format!("failed to read response body: {e}"),
    })?;

    let total_ms = start.elapsed().as_millis() as u32;

    // Extract model.
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&request.model)
        .to_string();

    // Extract finish reason.
    let finish_reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn")
        .to_string();

    // Extract usage.
    let usage = extract_anthropic_usage(body.get("usage"));

    // Extract content blocks.
    let raw_blocks = body.get("content").and_then(Value::as_array);

    let mut text_content = String::new();
    let mut content_blocks = Vec::new();

    if let Some(blocks) = raw_blocks {
        for block in blocks {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            match block_type {
                "text" => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    text_content.push_str(text);
                    content_blocks.push(ContentBlock::Text {
                        text: text.to_string(),
                    });
                }
                "thinking" => {
                    let thinking = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let signature = block
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(String::from);
                    content_blocks.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
                "redacted_thinking" => {
                    let data = block
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !data.starts_with("openrouter.reasoning:") {
                        content_blocks.push(ContentBlock::RedactedThinking { data });
                    }
                }
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    content_blocks.push(ContentBlock::ToolUse { id, name, input });
                }
                _ => {}
            }
        }
    }

    let timing = Timing {
        total_ms,
        time_to_first_token_ms: total_ms,
    };

    // Forensic log: response side (non-streaming only — streaming is
    // logged from the ledger's record_call since usage arrives later).
    if call_id > 0 && (usage.cache_creation_tokens > 0 || usage.cache_read_tokens > 0) {
        crate::cache_forensics::log_response(
            call_id,
            &model,
            "", // character not known at this layer
            "", // call_type not known at this layer
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
        );
    }

    Ok(GenerateResponse {
        content: text_content,
        content_blocks,
        finish_reason,
        usage,
        timing,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shore_config::models::Sdk;

    fn make_request(messages: Vec<Value>, system: Option<Value>) -> LlmRequest {
        LlmRequest {
            sdk: Sdk::Anthropic,
            model: "claude-test".into(),
            api_key: "sk-test".into(),
            base_url: None,
            messages,
            system,
            tools: None,
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            provider_options: None,
            provider_key: None,
            rid: None,
            forensic_character: None,
        }
    }

    // ── apply_cache_control ────────────────────────────────────────────

    #[test]
    fn test_apply_cache_control_empty_messages() {
        let (msgs, sys, _) = apply_cache_control(&[], &json!(null), "5m");
        assert!(msgs.is_empty());
        assert_eq!(sys, json!(null));
    }

    #[test]
    fn test_apply_cache_control_places_system_breakpoint() {
        let system = json!([
            { "type": "text", "text": "stable system prompt" },
            { "type": "text", "text": "recap that changes" }
        ]);
        let messages = vec![json!({"role": "user", "content": "hi"})];
        let (_, sys, _) = apply_cache_control(&messages, &system, "5m");

        // Breakpoint on second-to-last (index 0) — the stable block.
        let blocks = sys.as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_some());
        assert!(blocks[1].get("cache_control").is_none());
    }

    #[test]
    fn test_apply_cache_control_single_system_block() {
        let system = json!([{ "type": "text", "text": "only block" }]);
        let messages = vec![json!({"role": "user", "content": "hi"})];
        let (_, sys, _) = apply_cache_control(&messages, &system, "5m");

        let blocks = sys.as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_some());
    }

    #[test]
    fn test_apply_cache_control_normalizes_string_content() {
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi there"}),
        ];
        let (result, _, _) = apply_cache_control(&messages, &json!(null), "5m");

        // All string content should be normalized to array format.
        for msg in &result {
            assert!(
                msg["content"].is_array(),
                "content should be normalized to array, got: {}",
                msg["content"]
            );
        }
    }

    #[test]
    fn test_apply_cache_control_custom_ttl() {
        let system = json!([{ "type": "text", "text": "prompt" }]);
        let messages = vec![json!({"role": "user", "content": "hi"})];
        let (_, sys, _) = apply_cache_control(&messages, &system, "10m");

        let cc = &sys.as_array().unwrap()[0]["cache_control"];
        assert_eq!(cc["type"], "ephemeral");
        assert_eq!(cc["ttl"], "10m");
    }

    // ── convert_inline_system_messages ─────────────────────────────────

    #[test]
    fn test_convert_inline_system_no_system_messages() {
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        let result = convert_inline_system_messages(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[1]["role"], "assistant");
    }

    #[test]
    fn test_convert_inline_system_standalone() {
        let messages = vec![
            json!({"role": "assistant", "content": "prev response"}),
            json!({"role": "system", "content": "be helpful"}),
            json!({"role": "user", "content": "hello"}),
        ];
        let result = convert_inline_system_messages(&messages);

        // system becomes a user message with XML wrapper; no synthetic ack.
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["role"], "assistant");
        assert_eq!(result[1]["role"], "user");
        assert!(result[1]["content"]
            .as_str()
            .unwrap()
            .contains("<system_instruction>"));
        assert!(result[1]["content"]
            .as_str()
            .unwrap()
            .contains("be helpful"));
        assert_eq!(result[2]["role"], "user");
    }

    #[test]
    fn test_convert_inline_system_merges_into_preceding_user() {
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "system", "content": "be concise"}),
        ];
        let result = convert_inline_system_messages(&messages);

        // System merged into preceding user message to avoid consecutive user roles.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
        let content = result[0]["content"].as_str().unwrap();
        assert!(content.contains("hello"));
        assert!(content.contains("<system_instruction>be concise</system_instruction>"));
    }

    /// Regression: trailing system message (the interiority case) must
    /// produce a final *user* turn so the model generates a fresh response.
    /// Previously a synthetic `assistant: "Understood."` was appended,
    /// creating an unintentional prefill that caused verbatim-identical
    /// output across interiority ticks.
    #[test]
    fn test_trailing_system_message_no_prefill() {
        // Simulates the interiority tick: conversation ends with assistant,
        // then a system message is appended as the final message.
        let messages = vec![
            json!({"role": "user", "content": "How are you?"}),
            json!({"role": "assistant", "content": "Doing well, thanks!"}),
            json!({"role": "system", "content": "This is your private moment. Reflect freely."}),
        ];
        let result = convert_inline_system_messages(&messages);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[1]["role"], "assistant");
        // Final message must be user — the model generates from here.
        assert_eq!(result[2]["role"], "user");
        assert!(result[2]["content"]
            .as_str()
            .unwrap()
            .contains("<system_instruction>"));
        assert!(result[2]["content"]
            .as_str()
            .unwrap()
            .contains("Reflect freely."));
        // No "Understood." anywhere — it would create a prefill.
        for msg in &result {
            assert_ne!(
                msg["content"], "Understood.",
                "synthetic ack must not appear — it creates an unintentional prefill"
            );
        }
    }

    // ── build_thinking_config ─────────────────────────────────────────

    #[test]
    fn test_build_thinking_config_adaptive() {
        let opts = json!({"reasoning_effort": "adaptive"});
        let config = build_thinking_config(&opts).unwrap();
        assert_eq!(config["type"], "adaptive");
    }

    #[test]
    fn test_build_thinking_config_budget() {
        let opts = json!({"thinking": true, "budget_tokens": 2048});
        let config = build_thinking_config(&opts).unwrap();
        assert_eq!(config["type"], "enabled");
        assert_eq!(config["budget_tokens"], 2048);
    }

    #[test]
    fn test_build_thinking_config_none() {
        let opts = json!({});
        assert!(build_thinking_config(&opts).is_none());
    }

    // ── build_body ────────────────────────────────────────────────────

    #[test]
    fn test_build_body_minimal() {
        let request = make_request(vec![json!({"role": "user", "content": "hi"})], None);
        let (body, _) = build_body(&request, false);

        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["max_tokens"], 4096);
        assert!(body["messages"].is_array());
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn test_build_body_with_tools_and_system() {
        let mut request = make_request(
            vec![json!({"role": "user", "content": "search for cats"})],
            Some(json!("You are helpful.")),
        );
        request.tools = Some(vec![json!({
            "name": "web_search",
            "description": "Search the web",
            "input_schema": {"type": "object"}
        })]);
        let (body, _) = build_body(&request, false);

        assert!(body.get("system").is_some());
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_build_body_streaming_flag() {
        let request = make_request(vec![json!({"role": "user", "content": "hi"})], None);
        let (body, _) = build_body(&request, true);
        assert_eq!(body["stream"], true);

        let (body_no_stream, _) = build_body(&request, false);
        assert!(body_no_stream.get("stream").is_none());
    }

    // ── SSE event handlers ────────────────────────────────────────────

    #[test]
    fn test_handle_sse_event_message_start() {
        let data = json!({
            "type": "message_start",
            "message": {
                "model": "claude-3-opus-20240229",
                "usage": {"input_tokens": 100}
            }
        })
        .to_string();

        let mut state = StreamState::new("unknown");
        let result = handle_message_start(&data, &mut state);

        assert!(result.is_some());
        let event: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(event["type"], "start");
        assert_eq!(event["model"], "claude-3-opus-20240229");
        assert_eq!(state.model, "claude-3-opus-20240229");
        assert_eq!(state.usage.input_tokens, 100);
    }

    #[test]
    fn test_handle_sse_event_tool_use_lifecycle() {
        let mut state = StreamState::new("claude-test");

        // 1. content_block_start: register tool block.
        let start_data = json!({
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_123",
                "name": "web_search"
            }
        })
        .to_string();
        let r1 = handle_content_block_start(&start_data, &mut state);
        assert!(r1.is_none(), "content_block_start should not emit");
        assert!(state.tool_blocks.contains_key(&0));

        // 2. content_block_delta: accumulate JSON.
        let delta_data = json!({
            "index": 0,
            "delta": {
                "type": "input_json_delta",
                "partial_json": "{\"query\":"
            }
        })
        .to_string();
        let r2 = handle_content_block_delta(&delta_data, &mut state);
        assert!(r2.is_none(), "input_json_delta should not emit");

        let delta_data2 = json!({
            "index": 0,
            "delta": {
                "type": "input_json_delta",
                "partial_json": "\"cats\"}"
            }
        })
        .to_string();
        handle_content_block_delta(&delta_data2, &mut state);

        // 3. content_block_stop: emit tool_use event.
        let stop_data = json!({"index": 0}).to_string();
        let r3 = handle_content_block_stop(&stop_data, &mut state);
        assert!(r3.is_some());
        let event: Value = serde_json::from_str(&r3.unwrap()).unwrap();
        assert_eq!(event["type"], "tool_use");
        assert_eq!(event["id"], "toolu_123");
        assert_eq!(event["name"], "web_search");
        assert_eq!(event["input"]["query"], "cats");
    }

    // ── handle_sse_event dispatch ────────────────────────────────────

    #[test]
    fn test_handle_sse_event_unknown_type_returns_none() {
        let mut state = StreamState::new("claude-test");
        let event = SseEvent {
            event: Some("totally_unknown".into()),
            data: "{}".into(),
        };
        assert!(handle_sse_event(event, &mut state).is_none());
    }

    #[test]
    fn test_handle_sse_event_ping_returns_none() {
        let mut state = StreamState::new("claude-test");
        let event = SseEvent {
            event: Some("ping".into()),
            data: "{}".into(),
        };
        assert!(handle_sse_event(event, &mut state).is_none());
    }

    #[test]
    fn test_handle_sse_event_falls_back_to_json_type_field() {
        // OpenRouter proxy case: no event: field, type inside data payload.
        let mut state = StreamState::new("claude-test");
        let data = json!({
            "type": "message_start",
            "message": {
                "model": "claude-proxy",
                "usage": {"input_tokens": 50}
            }
        })
        .to_string();
        let event = SseEvent { event: None, data };
        let result = handle_sse_event(event, &mut state);
        assert!(result.is_some());
        let parsed: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["type"], "start");
        assert_eq!(state.model, "claude-proxy");
    }

    // ── handle_content_block_delta ───────────────────────────────────

    #[test]
    fn test_text_delta_emits_and_accumulates() {
        let mut state = StreamState::new("claude-test");
        assert!(state.first_token_time.is_none());

        let data = json!({
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hello " }
        })
        .to_string();
        let result = handle_content_block_delta(&data, &mut state);

        assert!(result.is_some());
        let event: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(event["type"], "text");
        assert_eq!(event["text"], "Hello ");
        assert_eq!(state.text_content, "Hello ");
        assert!(state.first_token_time.is_some());

        // Second delta accumulates.
        let data2 = json!({
            "index": 0,
            "delta": { "type": "text_delta", "text": "world" }
        })
        .to_string();
        handle_content_block_delta(&data2, &mut state);
        assert_eq!(state.text_content, "Hello world");
    }

    #[test]
    fn test_thinking_delta_emits() {
        let mut state = StreamState::new("claude-test");
        state.thinking_blocks.insert(0);

        let data = json!({
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "Let me consider..." }
        })
        .to_string();
        let result = handle_content_block_delta(&data, &mut state);

        assert!(result.is_some());
        let event: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(event["type"], "thinking");
        assert_eq!(event["text"], "Let me consider...");
        assert!(state.first_token_time.is_some());
    }

    #[test]
    fn test_signature_delta_accumulates_without_emitting() {
        let mut state = StreamState::new("claude-test");
        state.thinking_blocks.insert(0);

        let data = json!({
            "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig_part1" }
        })
        .to_string();
        let result = handle_content_block_delta(&data, &mut state);
        assert!(result.is_none());
        assert_eq!(state.thinking_signatures.get(&0).unwrap(), "sig_part1");

        // Second chunk appends.
        let data2 = json!({
            "index": 0,
            "delta": { "type": "signature_delta", "signature": "_part2" }
        })
        .to_string();
        handle_content_block_delta(&data2, &mut state);
        assert_eq!(
            state.thinking_signatures.get(&0).unwrap(),
            "sig_part1_part2"
        );
    }

    // ── handle_content_block_start ───────────────────────────────────

    #[test]
    fn test_content_block_start_redacted_thinking() {
        let mut state = StreamState::new("claude-test");
        let data = json!({
            "index": 0,
            "content_block": { "type": "redacted_thinking", "data": "encrypted_blob" }
        })
        .to_string();

        let result = handle_content_block_start(&data, &mut state);
        assert!(result.is_none());
        assert_eq!(
            state.redacted_thinking_blocks.get(&0).unwrap(),
            "encrypted_blob"
        );
    }

    #[test]
    fn test_content_block_start_openrouter_redacted_filtered() {
        let mut state = StreamState::new("claude-test");
        let data = json!({
            "index": 0,
            "content_block": {
                "type": "redacted_thinking",
                "data": "openrouter.reasoning: some duplicate content"
            }
        })
        .to_string();

        let result = handle_content_block_start(&data, &mut state);
        assert!(result.is_none());
        assert!(
            state.redacted_thinking_blocks.is_empty(),
            "OpenRouter bogus redacted_thinking should be filtered"
        );
    }

    // ── handle_content_block_stop ────────────────────────────────────

    #[test]
    fn test_content_block_stop_thinking_emits_signature() {
        let mut state = StreamState::new("claude-test");
        state.thinking_blocks.insert(0);
        state.thinking_signatures.insert(0, "sig_complete".into());

        let data = json!({"index": 0}).to_string();
        let result = handle_content_block_stop(&data, &mut state);

        assert!(result.is_some());
        let event: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(event["type"], "thinking_signature");
        assert_eq!(event["signature"], "sig_complete");
        assert!(!state.thinking_blocks.contains(&0));
        assert!(!state.thinking_signatures.contains_key(&0));
    }

    #[test]
    fn test_content_block_stop_redacted_thinking_emits() {
        let mut state = StreamState::new("claude-test");
        state
            .redacted_thinking_blocks
            .insert(0, "opaque_data".into());

        let data = json!({"index": 0}).to_string();
        let result = handle_content_block_stop(&data, &mut state);

        assert!(result.is_some());
        let event: Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(event["type"], "redacted_thinking");
        assert_eq!(event["data"], "opaque_data");
        assert!(!state.redacted_thinking_blocks.contains_key(&0));
    }

    // ── handle_message_delta ─────────────────────────────────────────

    #[test]
    fn test_message_delta_sets_finish_reason_and_usage() {
        let mut state = StreamState::new("claude-test");
        let data = json!({
            "delta": { "stop_reason": "tool_use" },
            "usage": { "output_tokens": 42 }
        })
        .to_string();

        handle_message_delta(&data, &mut state);
        assert_eq!(state.finish_reason, "tool_use");
        assert_eq!(state.usage.output_tokens, 42);
    }

    // ── handle_message_stop ──────────────────────────────────────────

    #[test]
    fn test_message_stop_produces_done_event() {
        let mut state = StreamState::new("claude-test");
        state.text_content = "Final answer.".into();
        state.finish_reason = "end_turn".into();
        state.usage.input_tokens = 100;
        state.usage.output_tokens = 50;

        let result = handle_message_stop(&mut state);
        let event: Value = serde_json::from_str(&result).unwrap();

        assert_eq!(event["type"], "done");
        assert_eq!(event["content"], "Final answer.");
        assert_eq!(event["finish_reason"], "end_turn");
        assert_eq!(event["usage"]["input_tokens"], 100);
        assert_eq!(event["usage"]["output_tokens"], 50);
        // Timing fields exist but values are non-deterministic.
        assert!(event.get("timing").is_some());
    }

    // ── is_tool_result_message ──────────────────────────────────────

    #[test]
    fn test_is_tool_result_user_with_tool_results() {
        let msg = json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
            ]
        });
        assert!(is_tool_result_message(&msg));
    }

    #[test]
    fn test_is_tool_result_user_with_text() {
        let msg = json!({"role": "user", "content": "hello"});
        assert!(!is_tool_result_message(&msg));
    }

    #[test]
    fn test_is_tool_result_assistant_message() {
        let msg = json!({
            "role": "assistant",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
            ]
        });
        assert!(!is_tool_result_message(&msg));
    }

    #[test]
    fn test_is_tool_result_mixed_blocks() {
        let msg = json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
                {"type": "text", "text": "also some text"}
            ]
        });
        assert!(!is_tool_result_message(&msg));
    }

    #[test]
    fn test_is_tool_result_empty_content_array() {
        let msg = json!({"role": "user", "content": []});
        assert!(!is_tool_result_message(&msg));
    }

    // ── find_turn_boundary ──────────────────────────────────────────

    #[test]
    fn test_find_turn_boundary_empty_messages() {
        assert_eq!(find_turn_boundary(&[], 2), None);
    }

    #[test]
    fn test_find_turn_boundary_single_user_message() {
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        assert_eq!(find_turn_boundary(&msgs, 2), None);
    }

    #[test]
    fn test_find_turn_boundary_skips_tool_result_messages() {
        // Tool-result-only user messages should not count as "real" user turns.
        let msgs = vec![
            json!({"role": "user", "content": "first question"}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "search", "input": {}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "result"}]}),
            json!({"role": "assistant", "content": "answer 1"}),
            json!({"role": "user", "content": "second question"}),
            json!({"role": "assistant", "content": "answer 2"}),
            json!({"role": "user", "content": "third question"}),
            json!({"role": "assistant", "content": "answer 3"}),
        ];
        // depth=1: skip current turn, then 1 more → breakpoint before "second question" (index 4)
        // That means the boundary is at index 3 (last message of the turn before).
        let result = find_turn_boundary(&msgs, 1);
        assert_eq!(result, Some(3));
    }

    #[test]
    fn test_find_turn_boundary_exact_depth() {
        let msgs = vec![
            json!({"role": "user", "content": "q1"}),
            json!({"role": "assistant", "content": "a1"}),
            json!({"role": "user", "content": "q2"}),
            json!({"role": "assistant", "content": "a2"}),
            json!({"role": "user", "content": "q3"}),
            json!({"role": "assistant", "content": "a3"}),
        ];
        // depth=2: skip current turn (q3), then count 2 real user messages back.
        // That's q2 (1) and q1 (2). depth+1=3 matches q1 at index 0.
        // i > 0 check passes? No, i=0, so i > 0 is false. Returns None.
        assert_eq!(find_turn_boundary(&msgs, 2), None);

        // depth=1: skip q3, count 1 back → q2 at index 2, return i-1=1.
        assert_eq!(find_turn_boundary(&msgs, 1), Some(1));
    }

    #[test]
    fn test_find_turn_boundary_only_tool_result_users() {
        let msgs = vec![
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "r1"}]}),
            json!({"role": "assistant", "content": "a1"}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "r2"}]}),
            json!({"role": "assistant", "content": "a2"}),
        ];
        // No real user messages → None.
        assert_eq!(find_turn_boundary(&msgs, 1), None);
    }

    // ── base_url acceptance ───────────────────────────────────────────

    #[test]
    fn build_http_request_accepts_custom_base_url() {
        let client = reqwest::Client::new();
        let mut request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );
        request.base_url = Some("https://openrouter.ai/api".into());

        // Should NOT return an error — custom base_url is now accepted.
        let result = build_http_request(&client, &request, false);
        assert!(result.is_ok(), "custom base_url should be accepted");

        let (builder, _) = result.unwrap();
        let built = builder.build().unwrap();
        assert_eq!(
            built.url().as_str(),
            "https://openrouter.ai/api/v1/messages"
        );
    }

    #[test]
    fn build_http_request_uses_default_base_url() {
        let client = reqwest::Client::new();
        let request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );

        let result = build_http_request(&client, &request, false);
        assert!(result.is_ok());

        let (builder, _) = result.unwrap();
        let built = builder.build().unwrap();
        assert_eq!(
            built.url().as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn build_http_request_base_url_with_v1_suffix() {
        let client = reqwest::Client::new();
        let mut request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );
        // OpenRouter's default base_url ends with /v1 — should not double it.
        request.base_url = Some("https://openrouter.ai/api/v1".into());

        let result = build_http_request(&client, &request, false);
        assert!(result.is_ok());

        let (builder, _) = result.unwrap();
        let built = builder.build().unwrap();
        assert_eq!(
            built.url().as_str(),
            "https://openrouter.ai/api/v1/messages"
        );
    }

    #[test]
    fn build_http_request_localhost_still_works() {
        let client = reqwest::Client::new();
        let mut request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );
        request.base_url = Some("http://127.0.0.1:8080".into());

        let result = build_http_request(&client, &request, false);
        assert!(result.is_ok());

        let (builder, _) = result.unwrap();
        let built = builder.build().unwrap();
        assert_eq!(
            built.url().as_str(),
            "http://127.0.0.1:8080/v1/messages"
        );
    }
}
