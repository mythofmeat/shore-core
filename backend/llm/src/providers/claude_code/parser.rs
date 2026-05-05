//! Stream-json parser for the Claude Code CLI.
//!
//! Translates the line-delimited JSON event stream emitted by
//! `claude --print --output-format stream-json` into the
//! `StreamEvent` NDJSON shape that shore-llm produces for every
//! provider.
//!
//! The CLI's event types we consume:
//!
//! - `system` (subtype `init`): emitted before each turn under the
//!   long-lived subprocess pattern. We use the first one to source
//!   the model id for `Start`. Subsequent inits are ignored.
//! - `assistant`: model output. `message.content[]` is an array of
//!   `text` / `thinking` / `tool_use` / `redacted_thinking` blocks.
//! - `user`: emitted when the CLI receives a `tool_result` from an
//!   MCP server it called. Skipped — the daemon's MCP listener
//!   records the tool roundtrip independently and splices it into
//!   conversation history.
//! - `result`: end-of-turn summary with `stop_reason`, `usage`,
//!   `total_cost_usd`, `modelUsage`, `duration_ms`. Maps to `Done`.
//! - `rate_limit_event`: quota/window state. Stored on the next `Done`
//!   event's usage telemetry.
//! - `stream_event`: Anthropic-style partial message events emitted when
//!   `--include-partial-messages` is enabled. These produce live text and
//!   thinking deltas; final `assistant` blocks for those same partials are
//!   then used only as completion records and not re-emitted.
//!
//! Tool-use blocks in `assistant` events are emitted as
//! `StreamEvent::ToolUse` and recorded in `blocks`. The daemon's
//! claude_code path skips the generic tool loop, because the CLI
//! already ran tools internally via MCP; the emitted event preserves
//! the original block order for streamed persistence.

use serde::Deserialize;
use serde_json::Value;

use crate::types::{StreamEvent, Timing, Usage};
use shore_protocol::types::ContentBlock;

const PRIVATE_ATTACHED_IMAGE_TOOL: &str = "mcp__shore__shore_attached_image";

/// Outcome of feeding a single stream-json line into the parser.
#[derive(Debug, Default)]
pub(crate) struct ParseStep {
    /// Events to forward to the daemon, in order.
    pub events: Vec<StreamEvent>,
    /// Structured blocks to append to the running content_blocks record.
    pub blocks: Vec<ContentBlock>,
    /// True iff this line was a `result` event — caller should
    /// stop reading.
    pub done: bool,
}

/// Parser state across a single turn.
#[derive(Debug, Default)]
pub(crate) struct StreamJsonParser {
    /// Model id from the first `system init` event we've seen.
    model: Option<String>,
    /// Concatenated text from all assistant `text` blocks in this
    /// turn, used to populate `Done.content`.
    accumulated_text: String,
    /// Whether `Start` has been emitted yet for this turn.
    start_emitted: bool,
    /// Whether `Done` has been emitted yet.
    done_emitted: bool,
    /// Most recent rate-limit state emitted before the result event.
    rate_limit_info: Option<Value>,
    /// TTFT from the first partial message_start event, if the CLI reports it.
    ttft_ms: u32,
    /// True after we have emitted partial text/thinking deltas. Completed
    /// assistant blocks for text/thinking are cumulative and would duplicate
    /// client-visible chunks, so skip those while preserving tool_use blocks.
    partial_messages_seen: bool,
}

impl StreamJsonParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed one line of stream-json output. Returns the events to
    /// emit and content blocks to record.
    ///
    /// Unrecognized event types and parse errors are tolerated:
    /// they yield an empty `ParseStep` rather than an error,
    /// because the CLI may add new event types in future versions
    /// and we should not crash on them.
    pub(crate) fn handle_line(&mut self, line: &str) -> ParseStep {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ParseStep::default();
        }
        let envelope: RawEvent = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return ParseStep::default(),
        };
        match envelope {
            RawEvent::System(s) if s.subtype.as_deref() == Some("init") => {
                self.handle_system_init(s.model)
            }
            RawEvent::System(_) => ParseStep::default(),
            RawEvent::Assistant { message } => self.handle_assistant(message.content),
            RawEvent::User { .. } => ParseStep::default(),
            RawEvent::RateLimitEvent { rate_limit_info } => {
                self.rate_limit_info = rate_limit_info;
                ParseStep::default()
            }
            RawEvent::StreamEvent { event, ttft_ms } => self.handle_stream_event(event, ttft_ms),
            RawEvent::Result(r) => self.handle_result(r),
            RawEvent::Unknown => ParseStep::default(),
        }
    }

    fn handle_system_init(&mut self, model: Option<String>) -> ParseStep {
        let mut step = ParseStep::default();
        if self.model.is_none() {
            self.model = model;
        }
        if !self.start_emitted {
            if let Some(m) = self.model.clone() {
                step.events.push(StreamEvent::Start { model: m });
                self.start_emitted = true;
            }
        }
        step
    }

    fn handle_assistant(&mut self, blocks: Vec<RawAssistantBlock>) -> ParseStep {
        let mut step = ParseStep::default();
        for block in blocks {
            match block {
                RawAssistantBlock::Text { text } => {
                    if self.partial_messages_seen {
                        step.blocks.push(ContentBlock::Text { text });
                        continue;
                    }
                    self.accumulated_text.push_str(&text);
                    step.events.push(StreamEvent::Text { text: text.clone() });
                    step.blocks.push(ContentBlock::Text { text });
                }
                RawAssistantBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    if self.partial_messages_seen {
                        step.blocks.push(ContentBlock::Thinking {
                            thinking,
                            signature,
                        });
                        continue;
                    }
                    if !thinking.is_empty() {
                        step.events.push(StreamEvent::Thinking {
                            text: thinking.clone(),
                        });
                    }
                    if let Some(ref sig) = signature {
                        if !sig.is_empty() {
                            step.events.push(StreamEvent::ThinkingSignature {
                                signature: sig.clone(),
                            });
                        }
                    }
                    step.blocks.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
                RawAssistantBlock::RedactedThinking { data } => {
                    if self.partial_messages_seen {
                        step.blocks.push(ContentBlock::RedactedThinking { data });
                        continue;
                    }
                    step.events
                        .push(StreamEvent::RedactedThinking { data: data.clone() });
                    step.blocks.push(ContentBlock::RedactedThinking { data });
                }
                RawAssistantBlock::ToolUse { id, name, input } => {
                    if name == PRIVATE_ATTACHED_IMAGE_TOOL {
                        continue;
                    }
                    step.events.push(StreamEvent::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    step.blocks.push(ContentBlock::ToolUse { id, name, input });
                }
                RawAssistantBlock::Unknown => {}
            }
        }
        step
    }

    fn handle_stream_event(
        &mut self,
        event: RawPartialStreamEvent,
        ttft_ms: Option<u32>,
    ) -> ParseStep {
        if let Some(ttft) = ttft_ms {
            if self.ttft_ms == 0 {
                self.ttft_ms = ttft;
            }
        }

        let mut step = ParseStep::default();
        match event {
            RawPartialStreamEvent::MessageStart { message } => {
                if self.model.is_none() {
                    self.model = message.model;
                }
                if !self.start_emitted {
                    if let Some(m) = self.model.clone() {
                        step.events.push(StreamEvent::Start { model: m });
                        self.start_emitted = true;
                    }
                }
            }
            RawPartialStreamEvent::ContentBlockDelta { delta, .. } => match delta {
                RawPartialDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        self.partial_messages_seen = true;
                        self.accumulated_text.push_str(&text);
                        step.events.push(StreamEvent::Text { text });
                    }
                }
                RawPartialDelta::ThinkingDelta { thinking } => {
                    if !thinking.is_empty() {
                        self.partial_messages_seen = true;
                        step.events.push(StreamEvent::Thinking { text: thinking });
                    }
                }
                RawPartialDelta::SignatureDelta { signature } => {
                    if !signature.is_empty() {
                        self.partial_messages_seen = true;
                        step.events
                            .push(StreamEvent::ThinkingSignature { signature });
                    }
                }
                RawPartialDelta::Unknown => {}
            },
            RawPartialStreamEvent::ContentBlockStart { .. }
            | RawPartialStreamEvent::ContentBlockStop { .. }
            | RawPartialStreamEvent::MessageDelta { .. }
            | RawPartialStreamEvent::MessageStop
            | RawPartialStreamEvent::Unknown => {}
        }
        step
    }

    fn handle_result(&mut self, raw: RawResult) -> ParseStep {
        let mut step = ParseStep::default();
        if self.done_emitted {
            return step;
        }
        let usage = Usage {
            input_tokens: raw.usage.input_tokens,
            output_tokens: raw.usage.output_tokens,
            cache_read_tokens: raw.usage.cache_read_input_tokens,
            cache_creation_tokens: raw.usage.cache_creation_input_tokens,
            total_cost_usd: raw.total_cost_usd,
            model_usage: raw.model_usage,
            rate_limit_info: self.rate_limit_info.clone(),
        };
        let timing = Timing {
            total_ms: raw.duration_ms,
            time_to_first_token_ms: self.ttft_ms,
        };
        // Prefer the streaming accumulator; fall back to the
        // result event's text if accumulator was empty (defensive
        // — should always agree).
        let content = if !self.accumulated_text.is_empty() {
            std::mem::take(&mut self.accumulated_text)
        } else {
            raw.result.unwrap_or_default()
        };
        let finish_reason = normalize_stop_reason(raw.stop_reason.as_deref(), raw.is_error);
        step.events.push(StreamEvent::Done {
            content,
            finish_reason,
            usage,
            timing,
        });
        self.done_emitted = true;
        step.done = true;
        step
    }

    /// Returns the model id seen in the first `system init` event,
    /// if any. Used by the subprocess driver to populate
    /// `GenerateResponse.model`.
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Map the CLI's stop_reason / is_error to the `finish_reason`
/// string shore-llm callers expect. Normalized to the same
/// vocabulary the Anthropic provider emits so downstream code does
/// not need to special-case claude_code.
fn normalize_stop_reason(stop_reason: Option<&str>, is_error: bool) -> String {
    if is_error {
        return "error".to_string();
    }
    match stop_reason {
        Some("end_turn") | None => "end_turn".to_string(),
        Some("max_tokens") => "max_tokens".to_string(),
        Some("stop_sequence") => "stop_sequence".to_string(),
        Some("tool_use") => "tool_use".to_string(),
        Some(other) => other.to_string(),
    }
}

// ── Raw stream-json event shapes ───────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawEvent {
    System(RawSystemEvent),
    Assistant {
        message: RawAssistantMessage,
    },
    User {
        #[allow(dead_code)]
        message: Value,
    },
    RateLimitEvent {
        #[serde(default)]
        rate_limit_info: Option<Value>,
    },
    StreamEvent {
        event: RawPartialStreamEvent,
        #[serde(default)]
        ttft_ms: Option<u32>,
    },
    Result(RawResult),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct RawSystemEvent {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAssistantMessage {
    #[serde(default)]
    content: Vec<RawAssistantBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawAssistantBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawPartialStreamEvent {
    MessageStart {
        message: RawPartialMessage,
    },
    ContentBlockStart {
        #[allow(dead_code)]
        index: Option<usize>,
        #[allow(dead_code)]
        content_block: Option<Value>,
    },
    ContentBlockDelta {
        #[allow(dead_code)]
        index: Option<usize>,
        delta: RawPartialDelta,
    },
    ContentBlockStop {
        #[allow(dead_code)]
        index: Option<usize>,
    },
    MessageDelta {
        #[allow(dead_code)]
        delta: Value,
    },
    MessageStop,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct RawPartialMessage {
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawPartialDelta {
    TextDelta {
        #[serde(default)]
        text: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    usage: RawUsage,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default, rename = "modelUsage")]
    model_usage: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run_lines(lines: &[&str]) -> (Vec<StreamEvent>, Vec<ContentBlock>, bool) {
        let mut parser = StreamJsonParser::new();
        let mut events = Vec::new();
        let mut blocks = Vec::new();
        let mut done = false;
        for line in lines {
            let step = parser.handle_line(line);
            events.extend(step.events);
            blocks.extend(step.blocks);
            if step.done {
                done = true;
                break;
            }
        }
        (events, blocks, done)
    }

    #[test]
    fn vanilla_text_turn_emits_start_text_done() {
        let (events, _blocks, done) = run_lines(&[
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5","session_id":"abc"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"hi","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2},"duration_ms":150}"#,
        ]);
        assert!(done);
        assert_eq!(events.len(), 3);
        match &events[0] {
            StreamEvent::Start { model } => assert_eq!(model, "claude-sonnet-4-5"),
            other => panic!("expected Start, got {other:?}"),
        }
        match &events[1] {
            StreamEvent::Text { text } => assert_eq!(text, "hi"),
            other => panic!("expected Text, got {other:?}"),
        }
        match &events[2] {
            StreamEvent::Done {
                content,
                finish_reason,
                usage,
                timing,
            } => {
                assert_eq!(content, "hi");
                assert_eq!(finish_reason, "end_turn");
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(timing.total_ms, 150);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn thinking_block_with_signature_emits_thinking_and_signature() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"let me think","signature":"sig_xyz"},
            {"type":"text","text":"answer"}
        ]}}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert_eq!(step.events.len(), 3);
        match &step.events[0] {
            StreamEvent::Thinking { text } => assert_eq!(text, "let me think"),
            other => panic!("expected Thinking, got {other:?}"),
        }
        match &step.events[1] {
            StreamEvent::ThinkingSignature { signature } => assert_eq!(signature, "sig_xyz"),
            other => panic!("expected ThinkingSignature, got {other:?}"),
        }
        match &step.events[2] {
            StreamEvent::Text { text } => assert_eq!(text, "answer"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(step.blocks.len(), 2);
        match &step.blocks[0] {
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                assert_eq!(thinking, "let me think");
                assert_eq!(signature.as_deref(), Some("sig_xyz"));
            }
            other => panic!("expected Thinking block, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_block_recorded_and_emitted_as_event() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"text","text":"calling ping"},
            {"type":"tool_use","id":"toolu_01","name":"mcp__shore__ping","input":{"message":"hi"}}
        ]}}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert_eq!(step.events.len(), 2);
        match &step.events[1] {
            StreamEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "mcp__shore__ping");
                assert_eq!(input["message"], "hi");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
        let tool_blocks = step
            .blocks
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count();
        assert_eq!(tool_blocks, 1);
    }

    #[test]
    fn private_attachment_tool_use_is_not_emitted_or_recorded() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_image","name":"mcp__shore__shore_attached_image","input":{"index":1}}
        ]}}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert!(step.events.is_empty());
        assert!(step.blocks.is_empty());
    }

    #[test]
    fn user_frame_with_tool_result_is_skipped() {
        let line = r#"{"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_01","content":[{"type":"text","text":"pong"}]}
        ]}}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert!(step.events.is_empty());
        assert!(step.blocks.is_empty());
        assert!(!step.done);
    }

    #[test]
    fn rate_limit_event_is_skipped_silently() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1777845600,"rateLimitType":"five_hour"}}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert!(step.events.is_empty());
        assert!(!step.done);
    }

    #[test]
    fn result_carries_cost_model_usage_and_rate_limit_info() {
        let mut parser = StreamJsonParser::new();
        let _ = parser.handle_line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","overageStatus":"rejected"}}"#,
        );
        let step = parser.handle_line(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","stop_reason":"end_turn","total_cost_usd":0.0042,"modelUsage":{"claude-sonnet-4-5":{"costUSD":0.0042}},"usage":{"input_tokens":2,"output_tokens":1},"duration_ms":7}"#,
        );
        assert!(step.done);
        match &step.events[0] {
            StreamEvent::Done { usage, .. } => {
                assert_eq!(usage.total_cost_usd, Some(0.0042));
                assert!(usage.model_usage.as_ref().unwrap()["claude-sonnet-4-5"].is_object());
                assert_eq!(usage.rate_limit_info.as_ref().unwrap()["status"], "allowed");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_does_not_panic() {
        let line = r#"{"type":"some_future_event","payload":42}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert!(step.events.is_empty());
    }

    #[test]
    fn malformed_json_returns_empty_step() {
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line("not json");
        assert!(step.events.is_empty());
        let step = parser.handle_line("");
        assert!(step.events.is_empty());
    }

    #[test]
    fn second_system_init_does_not_re_emit_start() {
        let lines = &[
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}"#,
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}"#,
        ];
        let (events, _, _) = run_lines(lines);
        let start_count = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Start { .. }))
            .count();
        assert_eq!(start_count, 1);
    }

    #[test]
    fn result_with_is_error_finish_reason_is_error() {
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"out of usage","stop_reason":null,"usage":{},"duration_ms":50}"#;
        let mut parser = StreamJsonParser::new();
        let step = parser.handle_line(line);
        assert!(step.done);
        match &step.events[0] {
            StreamEvent::Done {
                finish_reason,
                content,
                ..
            } => {
                assert_eq!(finish_reason, "error");
                assert_eq!(content, "out of usage");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_result_does_not_re_emit_done() {
        let mut parser = StreamJsonParser::new();
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"x","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"duration_ms":1}"#;
        let step1 = parser.handle_line(line);
        let step2 = parser.handle_line(line);
        assert_eq!(step1.events.len(), 1);
        assert!(step2.events.is_empty());
    }

    #[test]
    fn partial_stream_events_emit_live_deltas_without_final_duplicate_text() {
        let (events, blocks, done) = run_lines(&[
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}"#,
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-4-5"}},"ttft_ms":123}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"hello","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2},"duration_ms":500}"#,
        ]);

        assert!(done);
        let text_chunks: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_chunks, vec!["hel", "lo"]);
        assert_eq!(
            blocks,
            vec![ContentBlock::Text {
                text: "hello".into()
            }]
        );
        match events.last().unwrap() {
            StreamEvent::Done {
                content, timing, ..
            } => {
                assert_eq!(content, "hello");
                assert_eq!(timing.time_to_first_token_ms, 123);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn final_assistant_tool_use_is_preserved_after_partial_text() {
        let (events, blocks, done) = run_lines(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"calling "}}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[
                {"type":"text","text":"calling "},
                {"type":"tool_use","id":"toolu_01","name":"mcp__shore__lookup","input":{"q":"x"}}
            ]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"calling ","stop_reason":"tool_use","usage":{},"duration_ms":20}"#,
        ]);

        assert!(done);
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::ToolUse { id, .. } if id == "toolu_01")));
        assert!(blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "toolu_01")));
    }

    /// Captured fixture from probe 6 (vanilla text turn). If this
    /// stops parsing cleanly the CLI's stream-json shape has
    /// drifted and the parser needs an update.
    #[test]
    fn parses_probe6_vanilla_text_fixture() {
        let fixture =
            include_str!("../../../tests/fixtures/claude_code/06-stream-json-shape.jsonl");
        let lines: Vec<&str> = fixture.lines().collect();
        let (events, _, done) = run_lines(&lines);
        assert!(done, "fixture must include a result event");
        assert!(matches!(events.first(), Some(StreamEvent::Start { .. })));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    /// Captured fixture from probe 3 (MCP tool roundtrip).
    /// Tool calls must appear both as StreamEvent::ToolUse and in
    /// the structured blocks so streamed history can preserve order.
    #[test]
    fn parses_probe3_mcp_tool_roundtrip_fixture() {
        let fixture = include_str!("../../../tests/fixtures/claude_code/03-mcp-tool-call.jsonl");
        let lines: Vec<&str> = fixture.lines().collect();
        let (events, blocks, done) = run_lines(&lines);
        assert!(done);
        let tool_use_events = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolUse { .. }))
            .count();
        assert!(tool_use_events >= 1);
        let tool_use_blocks = blocks
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count();
        assert!(tool_use_blocks >= 1);
    }
}
