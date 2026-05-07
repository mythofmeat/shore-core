# Changelog

## Unreleased

- Fixed Matrix avatar sync to read character avatars from
  `characters/<Character>/avatar.{png,jpg,jpeg,webp}` in the Shore config
  directory.
- Added a SillyTavern JSONL history importer and made `search_history` search
  stored alternate assistant replies, not just the currently selected body.
- Removed runtime memory-gate behavior (`memory`, `memory_read`,
  `memory_write`) so workspace tools treat `memory/...` as ordinary workspace
  paths. Private mode now hides only `search_history` and `exec`, and legacy
  memory gate keys are inert compatibility toggles.
- Expanded `[memory.retrieval]` with workspace indexing/search bounds
  (`max_file_bytes`, `max_indexed_files`, `max_total_indexed_bytes`,
  `max_embed_chars_per_file`) plus configurable binary handling (`skip`,
  `metadata`, `try_embed`), and made workspace indexing operate over the full
  workspace tree.
- Moved legacy diagnostic dream phase reports from workspace
  `memory/dreaming/**` into data-dir
  `$XDG_DATA_HOME/shore/<Character>/dreams/reports/**` so generated artifacts
  stay out of user memory files.
- Added SillyTavern-style alternates for regenerated assistant replies: regen
  now preserves prior alternatives, activates the newest response, and lets CLI
  users switch with `shore alt prev`, `shore alt 2`, or `shore alt list`.
  The TUI exposes `:alt` as a picker with instant local preview before Enter
  persists the selected alternate.
- Refined the TUI command picker so model and character submenus mark the
  active row consistently, scroll the selected item into view, and keep the
  active model out of the input border.
- Switched compaction `<write path="...">` ops to workspace-rooted paths so the
  scheme matches the runtime workspace tools. The model now writes
  `memory/people/foo.md` instead of `people/foo.md`; `MEMORY.md` continues to
  land at the workspace root. Compaction-generated paths must start with
  `memory/` (or be `MEMORY.md`); other workspace files (`SOUL.md`, `USER.md`,
  …) remain off-limits to compaction.
- Fixed hybrid workspace search to batch large embedding refreshes, validate
  embedding response shapes, honor `[memory.retrieval].mode` when choosing the
  default search mode, and return more useful body-line excerpts for semantic
  hits.
- Removed the bundled local ONNX embedder and the `local-embeddings` Cargo
  feature. Embeddings now go through the OpenAI-compatible client only;
  configure an `[embedding.<name>]` block pointing at a hosted or
  self-hosted endpoint (e.g. text-embedding-inference, llama.cpp's
  `/v1/embeddings`). When nothing is configured, hybrid/vector search
  degrades to lexical-only.
- Added the `claude_code` provider, which drives the local `claude` CLI through
  OAuth-backed Claude subscription usage while Shore hosts MCP tools in the
  daemon.
- Added `[daemon.http]` for the daemon-hosted MCP listener, Claude Code config
  doctor checks, usage telemetry for would-be API cost and rate-limit events,
  and an ignored live test that exercises the real CLI and MCP tool path.
- Added startup policy and documentation for non-loopback `[daemon.http]`
  exposure; the MCP listener is bearer-by-URL and has no auth or TLS.
- Serialized Claude Code keyed MCP sessions before provider dispatch so
  concurrent turns for one character cannot cross-wire tool callbacks.
- Moved Claude Code parser fixtures into tracked test data, repopulated
  `StreamResult.tool_uses` from the MCP ledger splice, and routed background
  heartbeat, compaction, dreaming, and keepalive calls through the same Claude
  Code MCP session setup.
- Hardened Claude Code subprocess handling so partial streams without a final
  `result` event fail as incomplete, and chat MCP sessions are torn down as
  soon as their tool ledger is spliced.
- Kept automatic post-turn compaction alive after one-shot CLI clients
  disconnect, so Claude Code sessions survive the compaction/reload boundary.
- Auto-enabled the daemon HTTP MCP listener when a `claude_code` chat model is
  configured, and documented remaining Claude Code parity gaps in
  `docs/claude-code-parity.md`.
- Extended Claude Code cached subprocess idle retention to one hour so native
  conversation context survives ordinary pauses between turns.
- Added progressive Claude Code client streaming via
  `--include-partial-messages`, including parser coverage for partial text
  chunks and a delayed-result forwarding test.
- Added Claude Code current-turn image input via a private Shore MCP attachment
  tool, avoiding broad Claude Code filesystem reads while working around the
  CLI's text-only stream-json input.
- Added native Claude Code session replay for cold starts with prior Shore
  history by writing Claude JSONL session files and spawning with `--resume`;
  the live suite verifies a token present only in replayed history survives.
