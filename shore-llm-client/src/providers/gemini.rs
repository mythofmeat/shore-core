use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::DuplexStream;

use crate::types::{ContentBlock, GenerateResponse, LlmRequest, Timing, Usage};
use crate::LlmError;

use super::sse::{read_sse_events, SseEvent};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Safety categories to explicitly disable filtering on.  We use "OFF" rather
/// than "BLOCK_NONE" because newer Gemini models (2.5+) default to OFF, and
/// setting BLOCK_NONE actually re-enables the safety evaluation system —
/// which has been observed to sporadically block content despite the
/// permissive threshold, causing silent blank responses.
const SAFETY_CATEGORIES: &[&str] = &[
    "HARM_CATEGORY_HARASSMENT",
    "HARM_CATEGORY_HATE_SPEECH",
    "HARM_CATEGORY_SEXUALLY_EXPLICIT",
    "HARM_CATEGORY_DANGEROUS_CONTENT",
    "HARM_CATEGORY_CIVIC_INTEGRITY",
];

/// Detect the Gemini generation from a model name string.
///
/// Returns the major version number (e.g. 3 for "gemini-3-flash-preview"),
/// or 0 if the generation cannot be determined.
fn detect_generation(model: &str) -> u32 {
    // Look for "gemini-<digits>" pattern.
    let Some(idx) = model.find("gemini-") else {
        return 0;
    };
    let after = &model[idx + "gemini-".len()..];
    // Extract digits from the start.
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().unwrap_or(0)
}

// ── Message translation ──────────────────────────────────────────────

/// Translate Anthropic-format messages into Gemini `contents` array.
///
/// Extracts the system prompt separately (returned via the second tuple element)
/// since Gemini takes it as `systemInstruction` rather than a message.
fn translate_messages(request: &LlmRequest) -> Vec<Value> {
    // Build a map from tool_use_id -> tool name so that tool_result blocks
    // can reference the function name (Gemini requires it).
    let mut tool_id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for msg in &request.messages {
        if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let (Some(id), Some(name)) = (
                        block.get("id").and_then(|v| v.as_str()),
                        block.get("name").and_then(|v| v.as_str()),
                    ) {
                        tool_id_to_name.insert(id.to_string(), name.to_string());
                    }
                }
            }
        }
    }

    let mut contents = Vec::new();

    for msg in &request.messages {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some("assistant") => "model",
            Some("system") => continue, // system messages handled separately
            Some(r) => r,
            None => continue,
        };

        let parts = if let Some(content_str) = msg.get("content").and_then(|c| c.as_str()) {
            // String content -> single text part.
            vec![json!({"text": content_str})]
        } else if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
            // Array of content blocks -> translate each.
            let mut parts = Vec::new();
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        parts.push(json!({"text": text}));
                    }
                    Some("tool_use") => {
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        let args = block.get("input").cloned().unwrap_or(json!({}));
                        parts.push(json!({
                            "functionCall": {
                                "name": name,
                                "args": args
                            }
                        }));
                    }
                    Some("tool_result") => {
                        // Look up the tool name from previous tool_use blocks.
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let name = tool_id_to_name
                            .get(tool_use_id)
                            .map(|s| s.as_str())
                            .unwrap_or(tool_use_id);
                        let content = &block["content"];
                        let response = if let Some(s) = content.as_str() {
                            json!({"result": s})
                        } else if content.is_null() {
                            json!({})
                        } else {
                            content.clone()
                        };
                        parts.push(json!({
                            "functionResponse": {
                                "name": name,
                                "response": response
                            }
                        }));
                    }
                    _ => {
                        // Unknown block type -- skip.
                    }
                }
            }
            parts
        } else {
            continue;
        };

        if !parts.is_empty() {
            contents.push(json!({
                "role": role,
                "parts": parts
            }));
        }
    }

    contents
}

/// Translate Anthropic-format tool definitions into Gemini `tools` array.
fn translate_tools(tools: &Option<Vec<Value>>) -> Option<Value> {
    let tools = tools.as_ref()?;
    if tools.is_empty() {
        return None;
    }
    let declarations: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.get("name").cloned().unwrap_or(Value::Null),
                "description": t.get("description").cloned().unwrap_or(Value::Null),
                "parameters": t.get("input_schema").cloned().unwrap_or(json!({}))
            })
        })
        .collect();
    Some(json!([{"functionDeclarations": declarations}]))
}

/// Build the Gemini `systemInstruction` from the request's system prompt.
fn translate_system(request: &LlmRequest) -> Option<Value> {
    let system = request.system.as_ref()?;
    // The daemon sends system as either a string or an array of {type:"text", text:"..."} blocks.
    if let Some(s) = system.as_str() {
        if s.is_empty() {
            return None;
        }
        Some(json!({"parts": [{"text": s}]}))
    } else if let Some(blocks) = system.as_array() {
        let parts: Vec<Value> = blocks
            .iter()
            .filter_map(|b| {
                b.get("text")
                    .and_then(|t| t.as_str())
                    .map(|t| json!({"text": t}))
            })
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(json!({"parts": parts}))
        }
    } else {
        None
    }
}

/// Merge consecutive same-role messages in the `contents` array.
///
/// Gemini rejects requests with consecutive messages of the same role.
/// The tool loop can naturally produce these (e.g. consecutive "model" messages
/// when the LLM responds with tool calls followed by text).
///
/// For text parts: find the last text part in the preceding message and append
/// "\n\n" + new text.  For non-text parts (functionCall, functionResponse):
/// append directly to the preceding message's parts array.
fn merge_consecutive_roles(contents: &mut Vec<Value>) {
    let mut merged: Vec<Value> = Vec::with_capacity(contents.len());

    for msg in contents.drain(..) {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let parts = msg.get("parts").and_then(|p| p.as_array()).cloned().unwrap_or_default();

        let should_merge = if let Some(prev) = merged.last() {
            prev.get("role").and_then(|r| r.as_str()) == Some(role)
        } else {
            false
        };

        if should_merge {
            let prev = merged.last_mut().unwrap();
            let prev_parts = prev.get_mut("parts").and_then(|p| p.as_array_mut()).unwrap();
            for part in parts {
                // Check if this is a plain text part (not thought)
                let is_text = part.get("text").is_some()
                    && part.get("thought").and_then(|t| t.as_bool()) != Some(true);
                if is_text {
                    let new_text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    // Find the last plain text part in prev_parts to merge into
                    let existing_text_idx = prev_parts.iter().rposition(|p| {
                        p.get("text").is_some()
                            && p.get("thought").and_then(|t| t.as_bool()) != Some(true)
                    });
                    if let Some(idx) = existing_text_idx {
                        let old = prev_parts[idx].get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let combined = format!("{}\n\n{}", old, new_text);
                        prev_parts[idx]["text"] = json!(combined);
                    } else {
                        prev_parts.push(part);
                    }
                } else {
                    prev_parts.push(part);
                }
            }
        } else {
            merged.push(msg);
        }
    }

    *contents = merged;
}

/// Build the full request body for the Gemini REST API.
fn build_request_body(request: &LlmRequest) -> Value {
    let mut contents = translate_messages(request);

    // Merge consecutive same-role messages (Fix 2).
    merge_consecutive_roles(&mut contents);

    let mut generation_config = json!({
        "maxOutputTokens": request.max_tokens,
    });
    if let Some(temp) = request.temperature {
        generation_config["temperature"] = json!(temp);
    }
    if let Some(top_p) = request.top_p {
        generation_config["topP"] = json!(top_p);
    }

    // Thinking config from provider_options (Fix 4: generation-aware).
    if let Some(opts) = &request.provider_options {
        // Determine the Gemini generation.
        let manual_gen = opts
            .get("gemini_generation")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let generation = if manual_gen > 0 {
            manual_gen
        } else {
            detect_generation(&request.model)
        };

        let budget = opts.get("budget_tokens").and_then(|v| v.as_u64());
        let effort_str = opts
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(budget) = budget {
            // Explicit budget_tokens always uses thinkingBudget.
            generation_config["thinkingConfig"] = json!({"thinkingBudget": budget});
        } else if !effort_str.is_empty() {
            if generation >= 3 {
                // Gemini 3.x: use thinkingLevel mapped from reasoning_effort strings.
                let level = match effort_str.to_lowercase().as_str() {
                    "minimal" => Some("THINKING_LEVEL_MINIMAL"),
                    "low" => Some("THINKING_LEVEL_LOW"),
                    "medium" => Some("THINKING_LEVEL_MEDIUM"),
                    "high" => Some("THINKING_LEVEL_HIGH"),
                    _ => None,
                };
                if let Some(level) = level {
                    generation_config["thinkingConfig"] = json!({"thinkingLevel": level});
                } else {
                    // Unknown effort string for gen3+, fall back to budget-based with -1 (dynamic).
                    generation_config["thinkingConfig"] = json!({"thinkingBudget": -1});
                }
            } else {
                // Gemini 2.x or unknown: use thinkingBudget=-1 (dynamic).
                generation_config["thinkingConfig"] = json!({"thinkingBudget": -1});
            }
        } else {
            // Check if reasoning_effort was provided as a numeric value.
            let effort_num = opts.get("reasoning_effort").and_then(|v| v.as_u64());
            if let Some(budget) = effort_num {
                generation_config["thinkingConfig"] = json!({"thinkingBudget": budget});
            }
        }
    }

    // Safety settings (Fix 1): disable all safety filters with "OFF".
    let safety_settings: Vec<Value> = SAFETY_CATEGORIES
        .iter()
        .map(|cat| json!({"category": cat, "threshold": "OFF"}))
        .collect();

    let mut body = json!({
        "contents": contents,
        "generationConfig": generation_config,
        "safetySettings": safety_settings,
    });

    if let Some(tools) = translate_tools(&request.tools) {
        body["tools"] = tools;
    }

    if let Some(system_instruction) = translate_system(request) {
        body["systemInstruction"] = system_instruction;
    }

    body
}

/// Resolve the base URL, stripping any trailing slash.
fn base_url(request: &LlmRequest) -> &str {
    request
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
}

// ── Finish reason mapping ────────────────────────────────────────────

fn normalize_finish_reason(reason: Option<&str>) -> &str {
    match reason {
        Some("STOP") => "end_turn",
        Some("MAX_TOKENS") => "max_tokens",
        Some("SAFETY") => "safety",
        Some("RECITATION") => "recitation",
        Some("MALFORMED_FUNCTION_CALL") => "tool_use",
        Some(other) => {
            // Return a static str for known lowercase values; otherwise fall through.
            // We leak a small string here for uncommon unknown reasons.
            // In practice Gemini returns a small fixed set.
            match other {
                "end_turn" => "end_turn",
                "max_tokens" => "max_tokens",
                _ => "end_turn",
            }
        }
        None => "end_turn",
    }
}

// ── Streaming ────────────────────────────────────────────────────────

/// Send a streaming request to the Gemini REST API.
///
/// Returns a `DuplexStream` that yields NDJSON `StreamEvent` lines.
/// A background task reads SSE events from the HTTP response and writes
/// translated NDJSON to the stream.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let body = build_request_body(request);
    let body_str = serde_json::to_string(&body).map_err(LlmError::Serialize)?;

    let url = format!(
        "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
        base_url(request),
        request.model,
        request.api_key,
    );

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .map_err(LlmError::Request)?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::HttpStatus {
            status: status.as_u16(),
            body,
        });
    }

    let model = request.model.clone();
    let (writer, reader) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut writer = writer;
        let start = Instant::now();
        let mut first_token_ms: Option<u32> = None;
        let mut text_content = String::new();
        let mut finish_reason = "end_turn".to_string();
        let mut usage = Usage::default();
        let mut function_calls: Vec<(String, Value)> = Vec::new();
        let mut started = false;

        let model_clone = model.clone();

        let result = read_sse_events(
            response,
            |event: SseEvent| {
                // Parse the SSE data as JSON.
                let chunk: Value = match serde_json::from_str(&event.data) {
                    Ok(v) => v,
                    Err(_) => return None,
                };

                // Emit start event on first chunk.
                if !started {
                    started = true;
                    let start_line = serde_json::to_string(&json!({
                        "type": "start",
                        "model": model_clone,
                    }))
                    .ok()?;
                    // We can only return one line per callback, so we need to
                    // buffer extra lines. Instead, we'll write the start event
                    // and accumulate the content events to return.
                    // Actually the SSE callback returns a single Option<String>.
                    // We need to emit multiple lines. Let's concatenate with \n.
                    let mut lines = start_line;
                    lines.push('\n');

                    // Process this chunk's parts.
                    if let Some(parts) = chunk
                        .get("candidates")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("content"))
                        .and_then(|c| c.get("parts"))
                        .and_then(|p| p.as_array())
                    {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                if first_token_ms.is_none() {
                                    first_token_ms =
                                        Some(start.elapsed().as_millis() as u32);
                                }
                                if part.get("thought").and_then(|t| t.as_bool()) == Some(true)
                                {
                                    if let Ok(line) = serde_json::to_string(&json!({
                                        "type": "thinking",
                                        "text": text,
                                    })) {
                                        lines.push_str(&line);
                                        lines.push('\n');
                                    }
                                    // Emit thought signature if present (Fix 3).
                                    if let Some(sig) = part.get("thoughtSignature").and_then(|s| s.as_str()) {
                                        if let Ok(sig_line) = serde_json::to_string(&json!({
                                            "type": "thinking_signature",
                                            "signature": sig,
                                        })) {
                                            lines.push_str(&sig_line);
                                            lines.push('\n');
                                        }
                                    }
                                } else {
                                    text_content.push_str(text);
                                    if let Ok(line) = serde_json::to_string(&json!({
                                        "type": "text",
                                        "text": text,
                                    })) {
                                        lines.push_str(&line);
                                        lines.push('\n');
                                    }
                                }
                            } else if let Some(fc) = part.get("functionCall") {
                                let name = fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let args = fc.get("args").cloned().unwrap_or(json!({}));
                                function_calls.push((name, args));
                            }
                        }
                    }

                    // Update finish reason if present.
                    if let Some(reason) = chunk
                        .get("candidates")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("finishReason"))
                        .and_then(|r| r.as_str())
                    {
                        finish_reason = normalize_finish_reason(Some(reason)).to_string();
                    }

                    // Update usage if present.
                    if let Some(meta) = chunk.get("usageMetadata") {
                        usage.input_tokens =
                            meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
                                as u32;
                        usage.output_tokens = meta
                            .get("candidatesTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        usage.cache_read_tokens = meta
                            .get("cachedContentTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                    }

                    // The callback returns a single string that gets written
                    // followed by \n. But we already embedded \n separators.
                    // The sse reader writes callback result + \n, so strip trailing \n.
                    let trimmed = lines.trim_end_matches('\n').to_string();
                    return Some(trimmed);
                }

                // Subsequent chunks.
                let mut lines = String::new();

                if let Some(parts) = chunk
                    .get("candidates")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("content"))
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if first_token_ms.is_none() {
                                first_token_ms = Some(start.elapsed().as_millis() as u32);
                            }
                            if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
                                if let Ok(line) = serde_json::to_string(&json!({
                                    "type": "thinking",
                                    "text": text,
                                })) {
                                    if !lines.is_empty() {
                                        lines.push('\n');
                                    }
                                    lines.push_str(&line);
                                }
                                // Emit thought signature if present (Fix 3).
                                if let Some(sig) = part.get("thoughtSignature").and_then(|s| s.as_str()) {
                                    if let Ok(sig_line) = serde_json::to_string(&json!({
                                        "type": "thinking_signature",
                                        "signature": sig,
                                    })) {
                                        if !lines.is_empty() {
                                            lines.push('\n');
                                        }
                                        lines.push_str(&sig_line);
                                    }
                                }
                            } else {
                                text_content.push_str(text);
                                if let Ok(line) = serde_json::to_string(&json!({
                                    "type": "text",
                                    "text": text,
                                })) {
                                    if !lines.is_empty() {
                                        lines.push('\n');
                                    }
                                    lines.push_str(&line);
                                }
                            }
                        } else if let Some(fc) = part.get("functionCall") {
                            let name = fc
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args = fc.get("args").cloned().unwrap_or(json!({}));
                            function_calls.push((name, args));
                        }
                    }
                }

                // Update finish reason if present.
                if let Some(reason) = chunk
                    .get("candidates")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("finishReason"))
                    .and_then(|r| r.as_str())
                {
                    finish_reason = normalize_finish_reason(Some(reason)).to_string();
                }

                // Update usage if present (use the last one that has it).
                if let Some(meta) = chunk.get("usageMetadata") {
                    usage.input_tokens =
                        meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
                            as u32;
                    usage.output_tokens = meta
                        .get("candidatesTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    usage.cache_read_tokens = meta
                        .get("cachedContentTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                }

                if lines.is_empty() {
                    None
                } else {
                    Some(lines)
                }
            },
            &mut writer,
        )
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "Gemini SSE reader error");
        }

        // Emit accumulated tool_use events.
        for (i, (name, args)) in function_calls.iter().enumerate() {
            let line = serde_json::to_string(&json!({
                "type": "tool_use",
                "id": format!("gemini_call_{i}"),
                "name": name,
                "input": args,
            }));
            if let Ok(line) = line {
                use tokio::io::AsyncWriteExt;
                let _ = writer.write_all(line.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        }

        // Emit done event.
        let total_ms = start.elapsed().as_millis() as u32;
        let done = serde_json::to_string(&json!({
            "type": "done",
            "content": text_content,
            "finish_reason": finish_reason,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_tokens": usage.cache_read_tokens,
                "cache_creation_tokens": usage.cache_creation_tokens,
            },
            "timing": {
                "total_ms": total_ms,
                "time_to_first_token_ms": first_token_ms.unwrap_or(total_ms),
            },
        }));
        if let Ok(line) = done {
            use tokio::io::AsyncWriteExt;
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        }

        // Drop writer so the reader sees EOF.
        drop(writer);
    });

    Ok(reader)
}

// ── Non-streaming ────────────────────────────────────────────────────

/// Send a non-streaming request to the Gemini REST API.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let body = build_request_body(request);
    let body_str = serde_json::to_string(&body).map_err(LlmError::Serialize)?;

    let url = format!(
        "{}/v1beta/models/{}:generateContent?key={}",
        base_url(request),
        request.model,
        request.api_key,
    );

    let start = Instant::now();

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .map_err(LlmError::Request)?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::HttpStatus {
            status: status.as_u16(),
            body,
        });
    }

    let resp_text = response.text().await.map_err(LlmError::Request)?;
    let resp: Value =
        serde_json::from_str(&resp_text).map_err(LlmError::Deserialize)?;

    let total_ms = start.elapsed().as_millis() as u32;

    let candidate = resp
        .get("candidates")
        .and_then(|c| c.get(0));

    let parts = candidate
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array());

    let mut text_content = String::new();
    let mut content_blocks = Vec::new();

    if let Some(parts) = parts {
        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
                    // Fix 3: preserve thoughtSignature from Gemini response.
                    let signature = part
                        .get("thoughtSignature")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                    content_blocks.push(ContentBlock::Thinking {
                        thinking: text.to_string(),
                        signature,
                    });
                } else {
                    text_content.push_str(text);
                    content_blocks.push(ContentBlock::Text {
                        text: text.to_string(),
                    });
                }
            } else if let Some(fc) = part.get("functionCall") {
                let name = fc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = fc.get("args").cloned().unwrap_or(json!({}));
                content_blocks.push(ContentBlock::ToolUse {
                    id: format!("gemini_{name}"),
                    name,
                    input: args,
                });
            }
        }
    }

    let finish_reason = candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str());
    let finish_reason = normalize_finish_reason(finish_reason).to_string();

    let meta = resp.get("usageMetadata");
    let usage = Usage {
        input_tokens: meta
            .and_then(|m| m.get("promptTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: meta
            .and_then(|m| m.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: meta
            .and_then(|m| m.get("cachedContentTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_creation_tokens: 0,
    };

    Ok(GenerateResponse {
        content: text_content,
        content_blocks,
        finish_reason,
        usage,
        timing: Timing {
            total_ms,
            time_to_first_token_ms: total_ms,
        },
        model: request.model.clone(),
    })
}
