use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::DuplexStream;

use super::sse::SseEvent;
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
        && arr.iter().all(|b| {
            b.get("type").and_then(Value::as_str) == Some("tool_result")
        })
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
        m.get("content").and_then(Value::as_array).map_or(false, |arr| {
            arr.iter().any(|b| b.get("cache_control").is_some())
        })
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
fn apply_cache_control(messages: &[Value], system: &Value, cache_ttl: &str) -> (Vec<Value>, Value) {
    if messages.is_empty() {
        return (messages.to_vec(), system.clone());
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
    let _last_real_user: Option<usize> = result
        .iter()
        .rposition(|m| {
            m.get("role").and_then(Value::as_str) == Some("user")
                && !is_tool_result_message(m)
        });

    // Step 4: Single message breakpoint at depth 2.
    {
        let cc = make_cache_control(cache_ttl);
        if let Some(pos) = find_turn_boundary(&result, 2) {
            apply_breakpoint_to_message(&mut result[pos], &cc);
        }
    }

    // System breakpoint: cache the stable system content.  The recap
    // (if present) is always the last block and changes on compaction,
    // so place the breakpoint on the block just before it.  When there
    // is no recap (≤1 block, or all blocks are stable), cache everything.
    let cc_sys = make_cache_control(cache_ttl);
    let mut sys = system.clone();
    if let Some(arr) = sys.as_array_mut() {
        let target_idx = if arr.len() >= 2 { arr.len() - 2 } else if arr.len() == 1 { 0 } else { usize::MAX };
        if let Some(obj) = arr.get_mut(target_idx).and_then(Value::as_object_mut) {
            obj.insert("cache_control".into(), cc_sys);
        }
    }

    (result, sys)
}

/// Build the `thinking` config param from provider_options.
fn build_thinking_config(opts: &Value) -> Option<Value> {
    let effort = opts.get("reasoning_effort").and_then(Value::as_str);

    // "adaptive" or named effort values → adaptive thinking mode.
    if effort == Some("adaptive") || effort.map_or(false, is_effort_value) {
        return Some(json!({ "type": "adaptive" }));
    }

    // Explicit budget: enable thinking with the given token budget.
    let thinking_flag = opts.get("thinking").and_then(Value::as_bool).unwrap_or(false);
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

/// Construct the JSON request body for the Anthropic Messages API.
fn build_body(request: &LlmRequest, streaming: bool) -> Value {
    let opts = request.provider_options.as_ref();
    let empty_opts = json!({});
    let opts_ref = opts.unwrap_or(&empty_opts);

    // Cache is enabled when cache_ttl is present and non-empty.
    let cache_ttl = opts_ref
        .get("cache_ttl")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cache_enabled = !cache_ttl.is_empty();
    let has_existing_markers = messages_have_cache_control(&request.messages);

    // On the first call of a turn (no existing markers), apply cache
    // breakpoints.  On tool-loop continuations (markers already present),
    // keep existing breakpoints immutable.
    let (messages, system) = if cache_enabled && !has_existing_markers {
        apply_cache_control(
            &request.messages,
            request.system.as_ref().unwrap_or(&json!(null)),
            cache_ttl,
        )
    } else {
        (
            request.messages.clone(),
            request.system.clone().unwrap_or(json!(null)),
        )
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

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }

    if let Some(thinking) = thinking {
        body["thinking"] = thinking;
    }

    if let Some(output_config) = output_config {
        body["output_config"] = output_config;
    }

    // Debug: dump body (after all modifications)
    {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        if let Ok(s) = serde_json::to_string_pretty(&body) {
            let _ = std::fs::write(format!("/tmp/shore_body_{:04}.json", n), s);
        }
    }

    body
}

/// Build the reqwest request with Anthropic-specific headers.
///
/// Always targets the native Anthropic API. Proxying Anthropic requests
/// through OpenRouter is not supported — use the OpenAI-compatible SDK
/// for OpenRouter models instead.
fn build_http_request(
    client: &reqwest::Client,
    request: &LlmRequest,
    streaming: bool,
) -> Result<reqwest::RequestBuilder, LlmError> {
    let url = match &request.base_url {
        Some(url) if url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") => {
            format!("{url}/v1/messages")
        }
        Some(_) => {
            return Err(LlmError::Provider {
                message: "Anthropic SDK does not support custom base_url. \
                          Use the openrouter SDK for OpenRouter models."
                    .into(),
            });
        }
        None => format!("{DEFAULT_BASE_URL}/v1/messages"),
    };
    let body = build_body(request, streaming);

    let builder = client
        .post(&url)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .header("x-api-key", &request.api_key);

    Ok(builder.json(&body))
}

/// Check the HTTP response status, returning the response on success or
/// an `HttpStatus` error with the body on failure.
async fn check_response(response: reqwest::Response) -> Result<reqwest::Response, LlmError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let status_code = status.as_u16();
    let body = response.text().await.unwrap_or_default();
    Err(LlmError::HttpStatus {
        status: status_code,
        body,
    })
}

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
    let http_req = build_http_request(client, request, true)?;
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
        state.usage.input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        state.usage.cache_read_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        state.usage.cache_creation_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
    }

    Some(json!({ "type": "start", "model": state.model }).to_string())
}

fn handle_content_block_start(data: &str, state: &mut StreamState) -> Option<String> {
    let parsed: Value = serde_json::from_str(data).ok()?;
    let index = parsed.get("index").and_then(Value::as_u64)?;
    let block = parsed.get("content_block")?;
    let block_type = block.get("type").and_then(Value::as_str)?;

    match block_type {
        "tool_use" => {
            let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            state.tool_blocks.insert(index, ToolBlockState {
                id,
                name,
                json_chunks: Vec::new(),
            });
            None
        }
        "thinking" => {
            state.thinking_blocks.insert(index);
            None
        }
        "redacted_thinking" => {
            let data_str = block.get("data").and_then(Value::as_str).unwrap_or("").to_string();
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
        return Some(
            json!({
                "type": "tool_use",
                "id": tool.id,
                "name": tool.name,
                "input": input,
            })
            .to_string(),
        );
    }

    // Check for thinking block completion — emit accumulated signature.
    if state.thinking_blocks.remove(&index) {
        if let Some(sig) = state.thinking_signatures.remove(&index) {
            return Some(
                json!({ "type": "thinking_signature", "signature": sig }).to_string(),
            );
        }
        return None;
    }

    // Check for redacted_thinking block completion.
    if let Some(redacted_data) = state.redacted_thinking_blocks.remove(&index) {
        return Some(
            json!({ "type": "redacted_thinking", "data": redacted_data }).to_string(),
        );
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
        // OpenRouter sends actual input_tokens in message_delta (0 in message_start).
        if let Some(inp) = usage.get("input_tokens").and_then(Value::as_u64) {
            if inp > 0 {
                state.usage.input_tokens = inp as u32;
            }
        }
        if let Some(out) = usage.get("output_tokens").and_then(Value::as_u64) {
            state.usage.output_tokens = out as u32;
        }
        if let Some(cr) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
            state.usage.cache_read_tokens = cr as u32;
        }
        if let Some(cc) = usage.get("cache_creation_input_tokens").and_then(Value::as_u64) {
            state.usage.cache_creation_tokens = cc as u32;
        }
    }
}

fn handle_message_stop(state: &mut StreamState) -> String {
    let elapsed = state.start_time.elapsed();
    let total_ms = elapsed.as_millis() as u32;
    let ttft_ms = state
        .first_token_time
        .map(|t| t.duration_since(state.start_time).as_millis() as u32)
        .unwrap_or(total_ms);

    json!({
        "type": "done",
        "content": state.text_content,
        "finish_reason": state.finish_reason,
        "usage": {
            "input_tokens": state.usage.input_tokens,
            "output_tokens": state.usage.output_tokens,
            "cache_read_tokens": state.usage.cache_read_tokens,
            "cache_creation_tokens": state.usage.cache_creation_tokens,
        },
        "timing": {
            "total_ms": total_ms,
            "time_to_first_token_ms": ttft_ms,
        },
    })
    .to_string()
}

// ── Non-streaming ────────────────────────────────────────────────────────

/// Send a non-streaming generate request to the Anthropic Messages API.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let start = Instant::now();
    let http_req = build_http_request(client, request, false)?;
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
    let usage_val = body.get("usage");
    let usage = Usage {
        input_tokens: usage_val
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        output_tokens: usage_val
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_read_tokens: usage_val
            .and_then(|u| u.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_creation_tokens: usage_val
            .and_then(|u| u.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    };

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

    Ok(GenerateResponse {
        content: text_content,
        content_blocks,
        finish_reason,
        usage,
        timing,
        model,
    })
}
