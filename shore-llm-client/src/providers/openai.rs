use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::DuplexStream;

use shore_protocol::types::ContentBlock;

use crate::types::{GenerateResponse, ImageGenerateResponse, LlmRequest, Timing, Usage};
use crate::LlmError;

use super::sse::{read_sse_events, SseEvent};
use super::stream_helpers::{build_done_event, build_start_event, build_tool_use_event, StreamTiming};

// ── Default base URLs ───────────────────────────────────────────────

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

// ── Message & tool translation ──────────────────────────────────────

/// Translate Anthropic-format messages into OpenAI chat completion messages.
fn translate_messages(request: &LlmRequest) -> Vec<Value> {
    let mut out = Vec::new();

    // Inject system prompt if present.
    if let Some(system) = &request.system {
        if let Some(s) = system.as_str() {
            out.push(json!({"role": "system", "content": s}));
        } else if let Some(arr) = system.as_array() {
            // System may be an array of content blocks; concatenate text.
            let text: String = arr
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                out.push(json!({"role": "system", "content": text}));
            }
        }
    }

    for msg in &request.messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content");

        match content {
            // String content — simple pass-through.
            Some(Value::String(s)) => match role {
                "system" => out.push(json!({"role": "system", "content": s})),
                "user" => out.push(json!({"role": "user", "content": s})),
                "assistant" => out.push(json!({"role": "assistant", "content": s})),
                _ => {}
            },

            // Array content — needs block-level translation.
            Some(Value::Array(blocks)) => match role {
                "assistant" => {
                    let text_parts: Vec<&Value> = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .collect();
                    let tool_parts: Vec<&Value> = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                        .collect();

                    let content_str: String = text_parts
                        .iter()
                        .map(|b| b.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                        .collect();
                    let content_val = if content_str.is_empty() {
                        Value::Null
                    } else {
                        Value::String(content_str)
                    };

                    let tool_calls: Vec<Value> = tool_parts
                        .iter()
                        .map(|b| {
                            let id = b
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = b
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = b.get("input").cloned().unwrap_or(json!({}));
                            let arguments =
                                serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
                            json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments,
                                }
                            })
                        })
                        .collect();

                    let mut msg_obj = json!({"role": "assistant", "content": content_val});
                    if !tool_calls.is_empty() {
                        msg_obj["tool_calls"] = Value::Array(tool_calls);
                    }
                    out.push(msg_obj);
                }

                "user" => {
                    let tool_results: Vec<&Value> = blocks
                        .iter()
                        .filter(|b| {
                            b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        })
                        .collect();
                    let other_blocks: Vec<&Value> = blocks
                        .iter()
                        .filter(|b| {
                            let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            ty != "tool_result"
                        })
                        .collect();

                    // Emit tool result messages first.
                    for tr in &tool_results {
                        let tool_call_id = tr
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let content = match tr.get("content") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => {
                                serde_json::to_string(other).unwrap_or_default()
                            }
                            None => String::new(),
                        };
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": content,
                        }));
                    }

                    // Emit remaining blocks as a user message.
                    if !other_blocks.is_empty() {
                        let parts: Vec<Value> = other_blocks
                            .iter()
                            .filter_map(|b| {
                                let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match ty {
                                    "text" => {
                                        let text = b
                                            .get("text")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("");
                                        Some(json!({"type": "text", "text": text}))
                                    }
                                    "image" => {
                                        let source = b.get("source")?;
                                        let source_type =
                                            source.get("type").and_then(|t| t.as_str())?;
                                        if source_type == "base64" {
                                            let media_type = source
                                                .get("media_type")
                                                .and_then(|m| m.as_str())
                                                .unwrap_or("image/png");
                                            let data =
                                                source.get("data").and_then(|d| d.as_str())?;
                                            Some(json!({
                                                "type": "image_url",
                                                "image_url": {
                                                    "url": format!("data:{media_type};base64,{data}")
                                                }
                                            }))
                                        } else {
                                            None
                                        }
                                    }
                                    _ => None,
                                }
                            })
                            .collect();
                        if !parts.is_empty() {
                            out.push(json!({"role": "user", "content": parts}));
                        }
                    }
                }

                "system" => {
                    // System messages with array content: extract text blocks.
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                b.get("text").and_then(|t| t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        out.push(json!({"role": "system", "content": text}));
                    }
                }

                _ => {}
            },

            _ => {}
        }
    }

    out
}

/// Translate Anthropic-format tool definitions to OpenAI function-calling format.
fn translate_tools(tools: &Option<Vec<Value>>) -> Option<Vec<Value>> {
    let tools = tools.as_ref()?;
    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                        "description": t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                        "parameters": t.get("input_schema").cloned().unwrap_or(json!({})),
                    }
                })
            })
            .collect(),
    )
}

// ── Finish reason mapping ───────────────────────────────────────────

fn normalize_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("stop") => "end_turn",
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "content_filter",
        _ => "end_turn",
    }
}

// ── Usage extraction ───────────────────────────────────────────────

/// Extract token usage from an OpenAI usage object, including cached tokens
/// from `prompt_tokens_details.cached_tokens`.
fn extract_usage(u: &Value) -> Usage {
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    Usage {
        input_tokens: u
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    }
}

// ── Reasoning field selection ───────────────────────────────────────

/// Return the JSON field name used for reasoning/thinking content.
///
/// DeepSeek uses `reasoning_content`; all other OpenAI-compatible providers
/// (including OpenAI itself, OpenRouter, xAI) use `reasoning`.
fn reasoning_field(request: &LlmRequest) -> &'static str {
    let pk = request
        .provider_key
        .as_deref()
        .unwrap_or(request.provider.as_str());
    match pk {
        "deepseek" => "reasoning_content",
        _ => "reasoning",
    }
}

// ── Request body builder ────────────────────────────────────────────

/// Resolve base URL for an OpenAI-compatible request.
fn resolve_base_url(request: &LlmRequest) -> &str {
    request
        .base_url
        .as_deref()
        .unwrap_or(OPENAI_BASE_URL)
}

/// Provider key, falling back to the `provider` field.
fn provider_key(request: &LlmRequest) -> &str {
    request
        .provider_key
        .as_deref()
        .unwrap_or(request.provider.as_str())
}

/// Helper to extract a string value from provider_options.
fn provider_opt_str<'a>(request: &'a LlmRequest, key: &str) -> Option<&'a str> {
    request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get(key))
        .and_then(|v| v.as_str())
}

/// Build common request headers.
fn build_headers(request: &LlmRequest) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", request.api_key)
            .parse()
            .expect("valid header value"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );

    // OpenRouter-specific headers.
    if provider_key(request) == "openrouter" {
        if let Some(referer) = provider_opt_str(request, "http_referer") {
            if let Ok(val) = referer.parse() {
                headers.insert("HTTP-Referer", val);
            }
        }
        if let Some(title) = provider_opt_str(request, "x_title") {
            if let Ok(val) = title.parse() {
                headers.insert("X-Title", val);
            }
        }
    }

    headers
}

/// Build the JSON body for chat completions (shared by stream and generate).
fn build_chat_body(request: &LlmRequest, streaming: bool) -> Value {
    let messages = translate_messages(request);
    let tools = translate_tools(&request.tools);

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": streaming,
    });

    if streaming {
        body["stream_options"] = json!({"include_usage": true});
    }

    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
    }
    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }

    // reasoning_effort for providers that support it.
    let pk = provider_key(request);
    if matches!(pk, "deepseek" | "openrouter" | "xai" | "openai") {
        if let Some(effort) = provider_opt_str(request, "reasoning_effort") {
            body["reasoning_effort"] = json!(effort);
        }
    }

    // OpenRouter provider routing body param.
    // The value is a TOML table (e.g. {order = ["anthropic"]}), serialized as a
    // JSON object — use it directly rather than reading as a string.
    if pk == "openrouter" {
        if let Some(or_provider) = request
            .provider_options
            .as_ref()
            .and_then(|opts| opts.get("openrouter_provider"))
        {
            let mut provider = or_provider.clone();
            // When order is specified, default allow_fallbacks to false so
            // OpenRouter actually respects the preferred provider list.
            if let Some(obj) = provider.as_object_mut() {
                if obj.contains_key("order") {
                    obj.entry("allow_fallbacks".to_string())
                        .or_insert(json!(false));
                }
            }
            body["provider"] = provider;
        }
    }

    body
}

// ── Streaming ───────────────────────────────────────────────────────

/// Send a streaming chat completion request to an OpenAI-compatible API.
///
/// Returns a `DuplexStream` that yields NDJSON `StreamEvent` lines. A background
/// tokio task reads SSE from the upstream API and writes translated events.
pub async fn stream(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let base_url = resolve_base_url(request);
    let url = format!("{base_url}/chat/completions");
    let headers = build_headers(request);
    let body = build_chat_body(request, true);

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let reasoning_field_name = reasoning_field(request).to_string();
    let model_fallback = request.model.clone();

    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;

        let mut timing = StreamTiming::new();
        let mut text_content = String::new();
        let mut finish_reason: &str = "end_turn";
        let mut usage = Usage::default();
        let mut model = model_fallback;
        let mut start_sent = false;

        // Tool call accumulation: index → (id, name, argument_chunks).
        let mut tool_calls: HashMap<u64, (String, String, Vec<String>)> = HashMap::new();

        let result = read_sse_events(
            response,
            |event: SseEvent| {
                let data = event.data.trim();

                // [DONE] sentinel — return None, we emit done after the loop.
                if data == "[DONE]" {
                    return None;
                }

                let chunk: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => return None,
                };

                // Emit start event on first chunk.
                if !start_sent {
                    if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                        model = m.to_string();
                    }
                    start_sent = true;
                    return Some(build_start_event(&model));
                }

                let choice = chunk.get("choices").and_then(|c| c.get(0));
                let mut lines_out: Vec<String> = Vec::new();

                if let Some(choice) = choice {
                    let delta = choice.get("delta");

                    // Reasoning / thinking content.
                    if let Some(delta) = delta {
                        if let Some(reasoning) = delta
                            .get(&reasoning_field_name)
                            .and_then(|r| r.as_str())
                        {
                            if !reasoning.is_empty() {
                                timing.record_first_token();
                                let ev = json!({"type": "thinking", "text": reasoning});
                                if let Ok(line) = serde_json::to_string(&ev) {
                                    lines_out.push(line);
                                }
                            }
                        }
                    }

                    // Text content.
                    if let Some(content) = delta
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        if !content.is_empty() {
                            timing.record_first_token();
                            text_content.push_str(content);
                            let ev = json!({"type": "text", "text": content});
                            if let Ok(line) = serde_json::to_string(&ev) {
                                lines_out.push(line);
                            }
                        }
                    }

                    // Tool calls (streamed in fragments).
                    if let Some(tcs) = delta
                        .and_then(|d| d.get("tool_calls"))
                        .and_then(|t| t.as_array())
                    {
                        for tc in tcs {
                            let index = tc
                                .get("index")
                                .and_then(|i| i.as_u64())
                                .unwrap_or(0);
                            let entry = tool_calls
                                .entry(index)
                                .or_insert_with(|| (String::new(), String::new(), Vec::new()));
                            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                if !id.is_empty() {
                                    entry.0 = id.to_string();
                                }
                            }
                            if let Some(name) = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                            {
                                if !name.is_empty() {
                                    entry.1 = name.to_string();
                                }
                            }
                            if let Some(args) = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                            {
                                entry.2.push(args.to_string());
                            }
                        }
                    }

                    // Finish reason.
                    if let Some(reason) =
                        choice.get("finish_reason").and_then(|r| r.as_str())
                    {
                        finish_reason = normalize_finish_reason(Some(reason));
                    }
                }

                // Usage (final chunk with stream_options.include_usage).
                if let Some(u) = chunk.get("usage") {
                    usage = extract_usage(u);
                }

                if lines_out.is_empty() { None } else { Some(lines_out.join("\n")) }
            },
            &mut writer,
        )
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "SSE stream read error");
        }

        // Ensure start was emitted (empty stream edge case).
        if !start_sent {
            let line = build_start_event(&model);
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        }

        // Emit accumulated tool calls.
        let mut indices: Vec<u64> = tool_calls.keys().copied().collect();
        indices.sort();
        for idx in indices {
            if let Some((id, name, arg_chunks)) = tool_calls.remove(&idx) {
                let raw = arg_chunks.join("");
                let input: Value = if raw.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&raw).unwrap_or(json!({}))
                };
                let line = build_tool_use_event(&id, &name, &input);
                let _ = writer.write_all(line.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        }

        // Done event.
        let done = build_done_event(&text_content, finish_reason, &usage, timing.total_ms(), timing.ttft_ms());
        let _ = writer.write_all(done.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;

        // Drop writer to signal EOF to the reader half.
        drop(writer);
    });

    Ok(reader)
}

// ── Non-streaming generate ──────────────────────────────────────────

/// Send a non-streaming chat completion request to an OpenAI-compatible API.
pub async fn generate(
    client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let base_url = resolve_base_url(request);
    let url = format!("{base_url}/chat/completions");
    let headers = build_headers(request);
    let body = build_chat_body(request, false);

    let start = Instant::now();

    let response = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let total_ms = start.elapsed().as_millis() as u32;

    let resp_body: Value = response
        .json()
        .await
        .map_err(LlmError::Request)?;

    let choice = resp_body
        .get("choices")
        .and_then(|c| c.get(0));
    let message = choice.and_then(|c| c.get("message"));

    let field_name = reasoning_field(request);

    // Build content blocks.
    let mut typed_blocks: Vec<ContentBlock> = Vec::new();

    // Reasoning / thinking.
    if let Some(reasoning) = message
        .and_then(|m| m.get(field_name))
        .and_then(|r| r.as_str())
    {
        if !reasoning.is_empty() {
            typed_blocks.push(ContentBlock::Thinking {
                thinking: reasoning.to_string(),
                signature: None,
            });
        }
    }

    // Text content.
    let text_content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if !text_content.is_empty() {
        typed_blocks.push(ContentBlock::Text {
            text: text_content.to_string(),
        });
    }

    // Tool calls.
    if let Some(tcs) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        for tc in tcs {
            let tc_type = tc.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if tc_type != "function" {
                continue;
            }
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = func
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            let input: Value =
                serde_json::from_str(args_str).unwrap_or(json!({}));
            typed_blocks.push(ContentBlock::ToolUse { id, name, input });
        }
    }

    let finish_reason_raw = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str());

    let usage = resp_body
        .get("usage")
        .map(extract_usage)
        .unwrap_or_default();

    let resp_model = resp_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(&request.model);

    Ok(GenerateResponse {
        content: text_content.to_string(),
        content_blocks: typed_blocks,
        finish_reason: normalize_finish_reason(finish_reason_raw).to_string(),
        usage,
        timing: Timing {
            total_ms,
            time_to_first_token_ms: total_ms,
        },
        model: resp_model.to_string(),
    })
}

// ── Embeddings ──────────────────────────────────────────────────────

/// Generate embeddings via an OpenAI-compatible embeddings API.
pub async fn embed(
    client: &reqwest::Client,
    _provider: &str,
    model: &str,
    api_key: &str,
    base_url: Option<&str>,
    input: &[&str],
) -> Result<Vec<Vec<f32>>, LlmError> {
    let base = base_url.unwrap_or(OPENAI_BASE_URL);
    let url = format!("{base}/embeddings");

    let body = json!({
        "model": model,
        "input": input,
    });

    let response = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let resp: Value = response.json().await.map_err(LlmError::Request)?;

    let embeddings = resp
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("embedding").and_then(|e| {
                        e.as_array().map(|nums| {
                            nums.iter()
                                .filter_map(|n| n.as_f64().map(|f| f as f32))
                                .collect()
                        })
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(embeddings)
}

// ── Image generation ────────────────────────────────────────────────

/// Generate an image via an OpenAI-compatible API.
///
/// For OpenRouter, images are generated through the chat completions endpoint
/// with `modalities`. For other providers, the standard `/images/generations`
/// endpoint is used.
pub async fn image_generate(
    client: &reqwest::Client,
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: Option<&str>,
    prompt: &str,
    size: Option<&str>,
    quality: Option<&str>,
    aspect_ratio: Option<&str>,
    image_size: Option<&str>,
) -> Result<ImageGenerateResponse, LlmError> {
    let start = Instant::now();

    if provider == "openrouter" {
        let base = base_url.unwrap_or("https://openrouter.ai/api/v1");
        let result = openrouter_image_generate(
            client,
            base,
            api_key,
            model,
            prompt,
            aspect_ratio,
            image_size,
        )
        .await?;
        let total_ms = start.elapsed().as_millis() as u32;
        return Ok(ImageGenerateResponse {
            url: result.0,
            revised_prompt: result.1,
            timing: crate::types::ImageGenerateTiming { total_ms },
        });
    }

    // Standard OpenAI images/generations endpoint.
    let base = base_url.unwrap_or(OPENAI_BASE_URL);
    let url = format!("{base}/images/generations");

    let mut body = json!({
        "model": model,
        "prompt": prompt,
    });
    if let Some(s) = size {
        body["size"] = json!(s);
    }
    if let Some(q) = quality {
        body["quality"] = json!(q);
    }

    let response = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let total_ms = start.elapsed().as_millis() as u32;
    let resp: Value = response.json().await.map_err(LlmError::Request)?;

    let image = resp.get("data").and_then(|d| d.get(0));
    let url_str = image
        .and_then(|i| {
            i.get("url")
                .and_then(|u| u.as_str())
                .or_else(|| i.get("b64_json").and_then(|b| b.as_str()))
        })
        .unwrap_or("")
        .to_string();
    let revised_prompt = image
        .and_then(|i| i.get("revised_prompt"))
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();

    Ok(ImageGenerateResponse {
        url: url_str,
        revised_prompt,
        timing: crate::types::ImageGenerateTiming { total_ms },
    })
}

/// Try image generation via OpenRouter's chat completions with modalities.
///
/// Tries `["image", "text"]` first, then falls back to `["image"]` on 404
/// with "output modalities" error.
async fn openrouter_image_generate(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    aspect_ratio: Option<&str>,
    image_size: Option<&str>,
) -> Result<(String, String), LlmError> {
    // Try text+image first.
    match try_openrouter_image(
        client,
        base_url,
        api_key,
        model,
        prompt,
        &["image", "text"],
        aspect_ratio,
        image_size,
    )
    .await
    {
        Ok(result) => return Ok(result),
        Err(LlmError::HttpStatus { status: 404, ref body }) if body.contains("output modalities") => {
            // Fall through to image-only mode.
        }
        Err(e) => return Err(e),
    }

    // Retry with image-only modality.
    try_openrouter_image(
        client,
        base_url,
        api_key,
        model,
        prompt,
        &["image"],
        aspect_ratio,
        image_size,
    )
    .await
}

async fn try_openrouter_image(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    modalities: &[&str],
    aspect_ratio: Option<&str>,
    image_size: Option<&str>,
) -> Result<(String, String), LlmError> {
    let url = format!("{base_url}/chat/completions");

    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "modalities": modalities,
    });

    // Only include image_config when at least one field is set.
    let mut image_config = serde_json::Map::new();
    if let Some(ar) = aspect_ratio {
        image_config.insert("aspect_ratio".into(), json!(ar));
    }
    if let Some(is) = image_size {
        image_config.insert("image_size".into(), json!(is));
    }
    if !image_config.is_empty() {
        body["image_config"] = Value::Object(image_config);
    }

    let response = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;

    let response = super::check_response(response).await?;

    let resp: Value = response.json().await.map_err(LlmError::Request)?;

    let message = resp
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"));

    let image_url = message
        .and_then(|m| m.get("images"))
        .and_then(|imgs| imgs.get(0))
        .and_then(|img| img.get("image_url"))
        .and_then(|iu| iu.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("");

    if image_url.is_empty() {
        return Err(LlmError::Provider {
            message: "OpenRouter response contained no image data".into(),
        });
    }

    let revised_prompt = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    Ok((image_url.to_string(), revised_prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(messages: Vec<Value>, system: Option<Value>) -> LlmRequest {
        LlmRequest {
            provider: "openai".into(),
            model: "gpt-4".into(),
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
        }
    }

    // ── translate_messages ─────────────────────────────────────────────

    #[test]
    fn test_translate_messages_system_string() {
        let request = make_request(vec![], Some(json!("Be helpful.")));
        let msgs = translate_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be helpful.");
    }

    #[test]
    fn test_translate_messages_system_array() {
        let request = make_request(
            vec![],
            Some(json!([
                {"type": "text", "text": "Part one. "},
                {"type": "text", "text": "Part two."}
            ])),
        );
        let msgs = translate_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Part one. Part two.");
    }

    #[test]
    fn test_translate_messages_assistant_with_tool_use() {
        let request = make_request(
            vec![json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Let me search."},
                    {"type": "tool_use", "id": "call_1", "name": "search", "input": {"q": "cats"}}
                ]
            })],
            None,
        );
        let msgs = translate_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"], "Let me search.");
        let tool_calls = msgs[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "search");
        // Arguments are serialized JSON string.
        let args: Value = serde_json::from_str(
            tool_calls[0]["function"]["arguments"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(args["q"], "cats");
    }

    #[test]
    fn test_translate_messages_user_with_tool_result() {
        let request = make_request(
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "Found 5 results"}
                ]
            })],
            None,
        );
        let msgs = translate_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "call_1");
        assert_eq!(msgs[0]["content"], "Found 5 results");
    }

    #[test]
    fn test_translate_messages_user_with_image() {
        let request = make_request(
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "iVBORw0KGgo="
                        }
                    }
                ]
            })],
            None,
        );
        let msgs = translate_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        let parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        let url = parts[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    // ── translate_tools ───────────────────────────────────────────────

    #[test]
    fn test_translate_tools_maps_format() {
        let tools = Some(vec![json!({
            "name": "web_search",
            "description": "Search the web",
            "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}
        })]);
        let result = translate_tools(&tools).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["function"]["name"], "web_search");
        assert_eq!(result[0]["function"]["description"], "Search the web");
        assert!(result[0]["function"]["parameters"]["properties"]["q"].is_object());
    }

    #[test]
    fn test_translate_tools_none_and_empty() {
        assert!(translate_tools(&None).is_none());
        assert!(translate_tools(&Some(vec![])).is_none());
    }

    // ── normalize_finish_reason & extract_usage ────────────────────────

    #[test]
    fn test_normalize_finish_reason() {
        assert_eq!(normalize_finish_reason(Some("stop")), "end_turn");
        assert_eq!(normalize_finish_reason(Some("tool_calls")), "tool_use");
        assert_eq!(normalize_finish_reason(Some("length")), "max_tokens");
        assert_eq!(normalize_finish_reason(Some("content_filter")), "content_filter");
        assert_eq!(normalize_finish_reason(None), "end_turn");
        assert_eq!(normalize_finish_reason(Some("unknown")), "end_turn");
    }

    #[test]
    fn test_extract_usage_with_cached_tokens() {
        let usage_json = json!({
            "prompt_tokens": 500,
            "completion_tokens": 200,
            "prompt_tokens_details": {
                "cached_tokens": 350
            }
        });
        let usage = extract_usage(&usage_json);
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.cache_read_tokens, 350);
        assert_eq!(usage.cache_creation_tokens, 0);

        // Without prompt_tokens_details.
        let usage_simple = json!({"prompt_tokens": 100, "completion_tokens": 50});
        let usage2 = extract_usage(&usage_simple);
        assert_eq!(usage2.cache_read_tokens, 0);
    }

    // ── build_chat_body ───────────────────────────────────────────────

    #[test]
    fn test_build_chat_body_basic() {
        let request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );
        let body = build_chat_body(&request, true);

        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["stream"], true);
        assert!(body["stream_options"]["include_usage"].as_bool().unwrap());
        assert!(body["messages"].is_array());

        // Non-streaming should not have stream_options.
        let body_ns = build_chat_body(&request, false);
        assert_eq!(body_ns["stream"], false);
        assert!(body_ns.get("stream_options").is_none());
    }

    #[test]
    fn test_build_chat_body_openrouter_provider() {
        let mut request = make_request(
            vec![json!({"role": "user", "content": "hi"})],
            None,
        );
        request.provider = "openai".into();
        request.provider_key = Some("openrouter".into());
        request.provider_options = Some(json!({
            "openrouter_provider": {
                "order": ["anthropic"]
            }
        }));

        let body = build_chat_body(&request, false);
        let provider = &body["provider"];
        assert_eq!(provider["order"], json!(["anthropic"]));
        // allow_fallbacks defaults to false when order is specified.
        assert_eq!(provider["allow_fallbacks"], false);
    }

    #[test]
    fn test_build_headers_openrouter() {
        let mut request = make_request(vec![], None);
        request.provider_key = Some("openrouter".into());
        request.provider_options = Some(json!({
            "http_referer": "https://shore.ai",
            "x_title": "Shore"
        }));

        let headers = build_headers(&request);
        assert_eq!(headers.get("HTTP-Referer").unwrap(), "https://shore.ai");
        assert_eq!(headers.get("X-Title").unwrap(), "Shore");
    }
}
