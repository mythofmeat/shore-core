# OpenAI-Compatible Reasoning Tool Use

Status: completed
Owner: agent
Started: 2026-04-29

## Goal

Fix OpenAI-compatible tool-loop continuation requests for providers that require
assistant reasoning content to be replayed with tool calls.

## Context

- `TODO.md` records DeepSeek and Moonshot 400s complaining that
  `reasoning_content` is missing in assistant tool-call messages.
- `backend/llm/src/providers/openai.rs` captures provider reasoning into
  `ContentBlock::Thinking` but currently drops those blocks when converting
  persisted assistant tool-use turns back to OpenAI chat messages.
- `docs/dev-info/PROMPT_CACHING.md` says in-progress tool loops preserve
  provider-required thinking blocks.

## Work Items

- [x] Preserve assistant `thinking` blocks in OpenAI-compatible message replay.
- [x] Map Moonshot reasoning field selection to `reasoning_content`.
- [x] Add focused provider translator regression tests.
- [x] Run focused tests and harness checks relevant to docs/provider changes.

## Validation

- [x] `cargo test -p shore-llm providers::openai`
- [x] `cargo test -p shore-llm providers::context`
- [x] `cargo test -p shore-daemon content_util`
- [x] `cargo test -p shore-daemon engine::tools` (rerun with local socket
  permissions after sandbox blocked the mock SSE server)
- [x] `cargo fmt --all --check`
- [x] `python3 scripts/harness-check.py`
- [x] Live OpenCode Kimi smoke test with `/home/eshen/Downloads/.env`:
  `cargo run -p shore-llm --example live_reasoning_replay -- opencode-kimi`
- [x] Live NanoGPT DeepSeek-v4-pro thinking smoke test with
  `/home/eshen/Downloads/.env`:
  `cargo run -p shore-llm --example live_reasoning_replay -- nanogpt-deepseek`

## Decisions

- 2026-04-29: Keep this in the provider context and OpenAI adapter rather than
  adding a new public content block shape; Shore already normalizes provider
  reasoning as `ContentBlock::Thinking`.

## Handoff Notes

Live provider checks use `backend/llm/examples/live_reasoning_replay.rs`.
Set `SHORE_ENV_FILE=/path/to/.env` to point it at a specific credential file.
