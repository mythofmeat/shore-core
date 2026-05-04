//! Claude Code provider — drives the local `claude` CLI subprocess
//! to bill against a Claude subscription via OAuth instead of an API
//! key.
//!
//! See `docs/exec-plans/active/claude-code-provider.md` for the design,
//! and `dev/spikes/claude-code-probe/FINDINGS.md` for the empirical
//! findings that drove it.
//!
//! Pattern 3 (hybrid): long-lived `claude -p` subprocess per active
//! conversation when `provider_options.subprocess_key` is present,
//! fresh-spawn fallback otherwise.

mod cache;
mod driver;
mod parser;
mod quota;
mod recipe;

use serde_json::json;
use shore_protocol::types::ContentBlock;
use tokio::io::{AsyncWriteExt, DuplexStream};

use crate::types::{GenerateResponse, LlmRequest, StreamEvent, Timing, Usage};
use crate::LlmError;

use crate::providers::stream_helpers::{build_done_event, build_start_event};

/// Streaming entry point.
///
/// Spawns the CLI, parses stream-json output, then writes the events
/// as NDJSON to a `DuplexStream`. The "streaming" here is materialized
/// after the CLI completes — progressive deltas are not exposed
/// because Claude Code's stream-json emits whole assistant blocks per
/// event rather than text deltas. This matches the user direction
/// that streaming is acceptable to lose given the cost savings.
pub async fn stream(
    _client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    let output = run_driver(request).await?;
    let (read_half, mut write_half) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        for ev in output.events {
            let line = serialize_event(&ev);
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if write_half.write_all(b"\n").await.is_err() {
                break;
            }
        }
        // Drop write_half on return so the reader sees EOF.
    });
    Ok(read_half)
}

/// Non-streaming entry point.
pub async fn generate(
    _client: &reqwest::Client,
    request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    let output = run_driver(request).await?;
    Ok(build_generate_response(output, &request.model))
}

async fn run_driver(request: &LlmRequest) -> Result<driver::DriverOutput, LlmError> {
    if request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("subprocess_key"))
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        cache::run_long_lived(request).await
    } else {
        driver::run_fresh_spawn(request).await
    }
}

fn build_generate_response(output: driver::DriverOutput, request_model: &str) -> GenerateResponse {
    let model = if !output.model.is_empty() {
        output.model.clone()
    } else {
        request_model.to_string()
    };
    let mut content = String::new();
    let mut finish_reason = "end_turn".to_string();
    let mut usage = Usage::default();
    let mut timing = Timing::default();
    for ev in &output.events {
        if let StreamEvent::Done {
            content: c,
            finish_reason: f,
            usage: u,
            timing: t,
        } = ev
        {
            content = c.clone();
            finish_reason = f.clone();
            usage = u.clone();
            timing = t.clone();
        }
    }
    // Fall back to text-only blocks when Done.content is empty.
    if content.is_empty() {
        content = output
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
    }
    GenerateResponse {
        content,
        content_blocks: output.blocks,
        finish_reason,
        usage,
        timing,
        model,
    }
}

fn serialize_event(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::Start { model } => build_start_event(model),
        StreamEvent::Text { text } => json!({ "type": "text", "text": text }).to_string(),
        StreamEvent::Thinking { text } => json!({ "type": "thinking", "text": text }).to_string(),
        StreamEvent::ThinkingSignature { signature } => {
            json!({ "type": "thinking_signature", "signature": signature }).to_string()
        }
        StreamEvent::RedactedThinking { data } => {
            json!({ "type": "redacted_thinking", "data": data }).to_string()
        }
        StreamEvent::ToolUse { id, name, input } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": input }).to_string()
        }
        StreamEvent::Done {
            content,
            finish_reason,
            usage,
            timing,
        } => build_done_event(
            content,
            finish_reason,
            usage,
            timing.total_ms,
            timing.time_to_first_token_ms,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_text_event_matches_wire_shape() {
        let line = serialize_event(&StreamEvent::Text {
            text: "hello".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["text"], "hello");
    }

    #[test]
    fn serialize_thinking_event_matches_wire_shape() {
        let line = serialize_event(&StreamEvent::Thinking {
            text: "let me think".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "thinking");
        assert_eq!(v["text"], "let me think");
    }

    #[test]
    fn serialize_thinking_signature_event_matches_wire_shape() {
        let line = serialize_event(&StreamEvent::ThinkingSignature {
            signature: "sig_abc".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "thinking_signature");
        assert_eq!(v["signature"], "sig_abc");
    }

    #[test]
    fn serialize_redacted_thinking_event_matches_wire_shape() {
        let line = serialize_event(&StreamEvent::RedactedThinking {
            data: "opaque".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "redacted_thinking");
        assert_eq!(v["data"], "opaque");
    }

    #[test]
    fn serialize_round_trips_through_streamevent_deserialize() {
        // Regression pin: the wire shape we produce must be readable
        // back as a StreamEvent. If types::StreamEvent's serde tags
        // ever drift from what serialize_event emits, this fails.
        let original = vec![
            StreamEvent::Start {
                model: "claude-sonnet-4-5".into(),
            },
            StreamEvent::Text { text: "hi".into() },
            StreamEvent::Thinking {
                text: "thinking".into(),
            },
            StreamEvent::ThinkingSignature {
                signature: "sig".into(),
            },
            StreamEvent::RedactedThinking {
                data: "opaque".into(),
            },
            StreamEvent::Done {
                content: "answer".into(),
                finish_reason: "end_turn".into(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    ..Default::default()
                },
                timing: Timing {
                    total_ms: 100,
                    time_to_first_token_ms: 0,
                },
            },
        ];
        for ev in &original {
            let line = serialize_event(ev);
            let parsed: StreamEvent = serde_json::from_str(&line).unwrap_or_else(|e| {
                panic!("could not round-trip event {ev:?} from line {line}: {e}")
            });
            // Just check the discriminant matches.
            assert_eq!(
                std::mem::discriminant(ev),
                std::mem::discriminant(&parsed),
                "discriminant drift for {ev:?}"
            );
        }
    }

    #[test]
    fn build_generate_response_pulls_done_event_fields() {
        let output = driver::DriverOutput {
            model: "claude-sonnet-4-5".into(),
            events: vec![
                StreamEvent::Start {
                    model: "claude-sonnet-4-5".into(),
                },
                StreamEvent::Text { text: "hi".into() },
                StreamEvent::Done {
                    content: "hi".into(),
                    finish_reason: "end_turn".into(),
                    usage: Usage {
                        input_tokens: 5,
                        output_tokens: 1,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                        ..Default::default()
                    },
                    timing: Timing {
                        total_ms: 50,
                        time_to_first_token_ms: 0,
                    },
                },
            ],
            blocks: vec![ContentBlock::Text { text: "hi".into() }],
            stderr: String::new(),
        };
        let resp = build_generate_response(output, "fallback-model");
        assert_eq!(resp.model, "claude-sonnet-4-5");
        assert_eq!(resp.content, "hi");
        assert_eq!(resp.finish_reason, "end_turn");
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.timing.total_ms, 50);
        assert_eq!(resp.content_blocks.len(), 1);
    }

    #[test]
    fn build_generate_response_falls_back_to_request_model() {
        let output = driver::DriverOutput {
            events: vec![StreamEvent::Done {
                content: "x".into(),
                finish_reason: "end_turn".into(),
                usage: Usage::default(),
                timing: Timing::default(),
            }],
            ..Default::default()
        };
        let resp = build_generate_response(output, "fallback-model");
        assert_eq!(resp.model, "fallback-model");
    }

    #[test]
    fn build_generate_response_falls_back_content_to_text_blocks() {
        // If Done.content is empty (defensive), fall back to text
        // blocks so non-streaming consumers still see something.
        let output = driver::DriverOutput {
            events: vec![StreamEvent::Done {
                content: String::new(),
                finish_reason: "end_turn".into(),
                usage: Usage::default(),
                timing: Timing::default(),
            }],
            blocks: vec![
                ContentBlock::Text { text: "a".into() },
                ContentBlock::Text { text: "b".into() },
            ],
            ..Default::default()
        };
        let resp = build_generate_response(output, "m");
        assert_eq!(resp.content, "ab");
    }
}
