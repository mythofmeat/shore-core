//! Claude Code provider — drives the local `claude` CLI subprocess
//! to bill against a Claude subscription via OAuth instead of an API
//! key.
//!
//! See `docs/exec-plans/active/claude-code-provider.md` for the design,
//! and `dev/spikes/claude-code-probe/FINDINGS.md` for the empirical
//! findings that drove it.
//!
//! Pattern 3 (hybrid) is the target: long-lived `claude -p` subprocess
//! per active conversation for faithful turn-pair preservation,
//! fresh-spawn fallback for cold starts and post-compaction. Both
//! paths are not yet implemented; this is the type-system-only
//! scaffolding.

use tokio::io::DuplexStream;

use crate::types::{GenerateResponse, LlmRequest};
use crate::LlmError;

/// Streaming entry point. Wired through `providers::stream` dispatch.
///
/// Will spawn `claude --print --input-format stream-json
/// --output-format stream-json ...`, drive the CLI subprocess, and
/// translate stream-json events to `StreamEvent` NDJSON.
pub async fn stream(
    _client: &reqwest::Client,
    _request: &LlmRequest,
) -> Result<DuplexStream, LlmError> {
    Err(LlmError::Provider {
        message: "claude_code provider is not yet implemented (Phase B WIP)".into(),
    })
}

/// Non-streaming entry point.
pub async fn generate(
    _client: &reqwest::Client,
    _request: &LlmRequest,
) -> Result<GenerateResponse, LlmError> {
    Err(LlmError::Provider {
        message: "claude_code provider is not yet implemented (Phase B WIP)".into(),
    })
}
