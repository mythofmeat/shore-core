# Daemon Rewrite — Rust → TypeScript (Bun)

**Status:** active, started 2026-05-23.

## Why

Months of bugs in the prompt path. We keep reimplementing provider semantics
(`reasoning_details` consolidation, signed-thinking replay, extended-thinking
+ tool-use ordering, cache_control placement) and getting subtle things
wrong. Each patch ships with strong empirical claims, then live chat
discovers a new corner case. The fix isn't another careful Rust pass — it's
to stop competing with the providers' own SDK teams for correctness.

The TypeScript SDKs (`@anthropic-ai/sdk`, `openai`) are maintained by the
provider companies and battle-tested by their other tools. Using them
removes an entire category of bug from our codebase.

## Architecture

- **Daemon:** TypeScript, runs on Bun. Owns SWP server, LLM calls, tools,
  memory, ledger, autonomy, everything behind the SWP boundary.
- **CLI and external clients:** unchanged. SWP is the contract.
- **Protocol crate (`core/protocol`):** stays in Rust. It's the spec; the TS
  daemon mirrors its types in TS-land.
- **Runtime:** Bun. We get TypeScript without a transpile step, built-in
  sqlite, fast startup, and `bun build --compile` for single-binary
  distribution.

## Hard constraints (the "did we break anything" test)

These are frozen. Anything that touches them is a regression.

- **SWP wire format.** Newline-delimited JSON, types tagged by `type` field,
  protocol version `1`. Schemas in `core/protocol/src/{client_msg,server_msg,types}.rs`.
- **Handshake.** Daemon sends `ServerHello` on connect, client sends
  `ClientHello`, daemon sends `History`. Same sequence, same fields.
- **Instance registry.** `$SHORE_RUNTIME_DIR/instances.json`, JSON array of
  `{id, pid, addr, started_at, data_dir, config_dir}`. CLI discovers
  daemons via this file. Locking semantics in `backend/swp-server/src/registry.rs`.
- **Character workspace files.** Markdown files under `characters/<name>/workspace/**`.
  Same layout, same paths, same content.
- **Conversation log.** Whatever format the Rust daemon writes. We read what
  the Rust daemon left us; we write back in the same format.
- **`ledger.db` SQLite schema.** Read with `bun:sqlite`. Same tables, same
  columns.
- **Config file format.** `config.toml` + `conf.d/*.toml`. Parse with a TOML
  library, present the same effective config.

## Non-goals

- Don't rewrite the Rust CLI. Don't touch `clients/cli`.
- Don't change SWP.
- Don't change on-disk formats (data migration is out of scope).
- Don't add features during the rewrite. Bug-for-bug parity first, *then*
  improvements.

## Phased plan

Each phase ends with a runnable artifact and a parity check against the
Rust daemon. We don't ship the TS daemon until the final cutover phase.

### Phase 0: scaffold (this phase)

- `backend/daemon-ts/` Bun project: `package.json`, `tsconfig.json`,
  `.gitignore`. Entry at `src/main.ts`.
- **Exit criterion:** `bun run src/main.ts` starts a TCP listener on a
  port, accepts a connection from the Rust CLI, completes the SWP
  handshake, sends an empty `History`. CLI gets a session and exits cleanly.

### Phase 1: distribution story

- Validate `bun build --compile` produces a single-binary `shore-daemon`.
- Validate it runs on Arch (PKGBUILD-friendly) and macOS.
- Confirm size, startup time, and dynamic-lib dependencies are acceptable.
- **Exit criterion:** the compiled binary passes Phase 0's handshake test.

### Phase 2: config + workspace read

- Parse `config.toml` + `conf.d/*.toml` (TOML lib in TS).
- Read character workspace files.
- Read conversation log from disk.
- `History` snapshot includes real messages, not just an empty list.
- Effective-catalog logic (model resolution, provider defaults, per-character
  overrides) ported.
- **Exit criterion:** Rust CLI handshake against the TS daemon returns the
  same `History` JSON as the Rust daemon does, for an existing character.

### Phase 3: message append + persistence

- Engine: accept `ClientMessage::Message`, append to in-memory state and
  conversation log, broadcast `History` updates.
- Message IDs, timestamps, alt_index/alt_count, alternatives — same shape.
- Single-flight locks per character data root (from Rust's compaction
  code — preserve this).
- **Exit criterion:** send a user message via CLI, restart the daemon, see
  the message in the next handshake's `History`.

### Phase 4: LLM calls via real SDKs

- `@anthropic-ai/sdk` for Anthropic.
- `openai` SDK for OpenAI, OpenRouter, DeepSeek, Moonshot, xAI — anyone
  who exposes the OpenAI-compatible surface.
- `@google/generative-ai` for Gemini direct.
- Streaming responses translated to SWP `StreamStart` / `StreamChunk` /
  `StreamEnd` frames.
- Tool-use loop driven by the SDK (loop until the model stops requesting
  tools), daemon handles tool execution and feeds results back.
- Prompt caching, extended thinking, reasoning_details — all handled by
  the SDK with appropriate config flags.
- **Exit criterion:** Send a message via CLI to an Anthropic-via-OpenRouter
  character with extended thinking and a tool that requires a couple of
  iterations. Model finishes the loop, replies coherently, no signature
  errors, no model-loop pathology, cache continuity through the tool loop.
  THIS is the test the Rust daemon currently fails.

### Phase 5: tools

- `read`, `write`, `edit`, `list_files`, `search`, `delete`, `exec`,
  `web_search`, `fetch_url`, `check_time`, `roll_dice`, `activity_heatmap`,
  `generate_image`, `search_history`, memory tools.
- Path-traversal / symlink-escape protections preserved (load-bearing per
  AGENTS.md).
- `exec` must not invoke a shell.
- **Exit criterion:** each tool tested via a CLI message that invokes it,
  parity check against the Rust daemon's output.

### Phase 6: memory + dreaming + compaction

- Markdown memory under `characters/<C>/workspace/memory/`.
- Dreaming reorganizes `MEMORY.md`; compaction adds carry-forward
  throughlines.
- **PORT with care.** This subsystem has subtleties (single-flight locks,
  throughline carry-forward, the compaction lock fix in #30). Read the
  existing code carefully and write tests that pin the observable
  behavior before porting. *Do not* "rewrite from scratch" the memory
  subsystem — transcribe it.
- Semantic search via embeddings (HTTP, no language preference).
- **Exit criterion:** trigger a compaction via the same conditions that
  trigger it in the Rust daemon. Resulting `MEMORY.md` is byte-identical
  for a deterministic test input.

### Phase 7: ledger + cache forensics

- `bun:sqlite` against the existing `ledger.db`.
- Record every LLM call with token counts, cache reads/writes, finish
  reason. Same columns the Rust ledger writes.
- Cache anomaly detection (`unexpected_write` etc.).
- `cache_forensics.jsonl` append-only log.
- **Exit criterion:** `shore usage` command output matches Rust daemon for
  the same conversation.

### Phase 8: heartbeat + autonomy

- Per-character autonomy state machine.
- Keepalive pings to maintain the prompt cache TTL.
- Heartbeat tick that rebuilds the warmed prefix.
- **Exit criterion:** keepalive interval respected, no stale-cache
  rebuilds, no extra LLM calls vs. the Rust daemon under the same input.

### Phase 9: cutover

- `shore-daemon-ts` ships alongside `shore-daemon` for one release. Users
  opt in via config flag or env var.
- Once stable in the wild, `shore-daemon-ts` becomes the default.
- Rust daemon code moves to `attic/` or is deleted — decide at cutover.
- **Exit criterion:** no live failures reported on the TS daemon for one
  release cycle. Rust daemon retired.

## Things to specifically preserve from the Rust impl

These took multiple attempts to get right in Rust. Transcribe, don't
reinvent.

- Single-flight compaction locks keyed on character data root (#30).
- Heartbeat keepalive semantics (interval calculation, cache TTL anchoring).
- Cache anomaly detection thresholds (lazy threshold init on first non-zero
  cache_w; see PR #29 for the gotchas).
- Path traversal / symlink escape checks in workspace tools.
- `exec` shell-free invocation and path-confinement to the character workspace.
- Inline-system positioning (mid-history `<system_instruction>` wrapping).
- `cache_ttl` defaulting to "1h" for the Anthropic SDK so users get caching
  without explicit config.

## Things the SDKs solve for us — don't re-implement

- `reasoning_details` streaming chunk consolidation.
- Signed `thinking` block replay across turn-pairs.
- Tool-use loop with extended thinking ordering.
- Prompt caching `cache_control` placement.
- Redacted thinking handling.
- Image content blocks.
- Streaming SSE parsing.
- Provider-specific quirks (DeepSeek `reasoning_content`, Moonshot Kimi,
  Z.ai `zai_clear_thinking`).

## Risks

- **Distribution.** If `bun build --compile` doesn't produce an acceptable
  single binary, we have a problem. Mitigation: validate in Phase 1 before
  porting further.
- **Memory subsystem.** Subtleties accumulated over months. Mitigation: port
  with tests pinning the existing behavior; don't redesign.
- **Performance.** Probably fine at our scale (one user at a time). If not,
  hot paths can drop to native via `bun:ffi`.
- **Type safety on the wire.** TS doesn't enforce serde-style strict
  field validation. Mitigation: zod schemas at the SWP boundary + tests
  that reject extra/missing fields.
- **Two-language repo during the migration.** Both daemons coexist for
  Phase 9. Mitigation: kept on parallel branches/dirs, single Rust daemon
  remains shippable until cutover.

## Cross-check methodology

For every phase, parity against the Rust daemon is the test:

- Record SWP traces from the Rust daemon for representative scenarios.
- Replay the trace against the TS daemon, diff the emitted frames.
- Same input → same SWP output (modulo LLM response content itself).

This is more reliable than unit tests, which is the trap PR #29 fell into.

## First move

Phase 0 scaffold + handshake echo. See `backend/daemon-ts/`.
