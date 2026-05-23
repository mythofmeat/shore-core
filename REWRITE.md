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
- Validate it runs on Arch (PKGBUILD-friendly). Linux/x86_64 only — no Mac
  to test against.
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

Split into 4a (cache regression killed) and 4b/4c (full prompt + tool
surface). 4a is the load-bearing one — it proves the SDK route fixes
the bug this rewrite exists to kill.

**4a — cache regression validated (done, 2026-05-23):**

- `@anthropic-ai/sdk` adapter at `backend/daemon-ts/src/llm/providers/anthropic.ts`.
  Pointed at OpenRouter's `/v1/messages` by stripping the trailing `/v1`
  from the configured base URL.
- `openai` SDK adapter at `.../providers/openai.ts`. Live-validated
  via OpenRouter against `openai/gpt-5.4-mini` (the OpenAI-compatible
  endpoint is what every gateway in this family — OpenRouter, DeepSeek,
  xAI, NanoGPT — exposes). Scenarios green: single tool call, 3-
  iteration dependent-roll tool loop, automatic prompt caching (turn-2
  `cacheReadInputTokens > 0` via `prompt_tokens_details.cached_tokens`).
  OpenAI direct + the other gateway variants are base-URL swaps and
  reuse the same code path; not separately exercised here.
- Generic tool loop at `.../tool_loop.ts` that preserves block ordering
  verbatim across iterations (thinking → tool_use → tool_result → ...).
  This is what kills the cache regression.
- 4 cache_control breakpoints placed by the Anthropic adapter: last
  system block, last tool def, last stable assistant turn, last message.
- Live tests at `backend/daemon-ts/tests/cache_regression.test.ts`,
  gated on `OPENROUTER_API_KEY` / `ANTHROPIC_API_KEY`. All scenarios
  green on **haiku-4.5, sonnet-4.5, and sonnet-4.6**:
    - plain chat 2-turn cache hit
    - 1-iteration tool loop (loop-entry cache + loop-exit cache)
    - **adaptive thinking + 3-iteration dependent-roll tool loop**,
      with an explicit assertion that the assistant turns mix
      thinking-emitting and thinking-skipping shapes — the exact
      transition that broke the Rust daemon's prefix hash
- ONE inline tool (`roll_dice`) registered so the loop has something to
  iterate against. Full tool registry deferred to 4c.

One OpenRouter-specific gotcha is in the adapter:

- **Provider routing pin.** When the base URL points at OpenRouter we
  send `provider: { order: ["anthropic"], allow_fallbacks: false }` so
  the request is guaranteed to hit Anthropic directly. Without this
  pin OpenRouter can route to Bedrock/Vertex, which handle
  `cache_control` differently.

We deliberately **do NOT** filter OpenRouter's
`openrouter.reasoning:`-prefixed `redacted_thinking` blocks (the Rust
impl did, and that was wrong). Echoing them back verbatim is correct;
the cache prefix hash is unaffected by them, and the model needs the
reasoning context across turns. The empirical proof: with the
adapter unchanged but the prompt padded above haiku's real cache
threshold (~4096 tokens — Anthropic docs say 2048 but reality is
higher), cache holds across every thinking-shape transition.

Test-design notes worth preserving (both load-bearing):

- **Per-run cache nonce.** Each test run injects a fresh UUID into the
  system prompt so a warmed cache from a prior run can't hide a
  regression. Without this, `cache_read > 0` is meaningless — it could
  just be yesterday's cache.
- **Prompt size well above documented threshold.** Anthropic docs say
  haiku-4.5 caches prompts ≥2048 input tokens. In practice via
  OpenRouter, ~4000 input tokens still returns `cache_creation=0` on
  the first call. The test pads to ~11k tokens to stay clear of that
  gray zone.

- **Exit criterion (met):** in the live test, `cache_read_input_tokens > 0`
  on every provider call after the first within a turn, including
  across tool_use/tool_result boundaries AND across adaptive-thinking
  shape transitions (the assistant turn emitting a thinking block on
  iteration 1 but not iteration 2). The Rust regression dropped to 0
  here; the TS impl holds.

**4b — full prompt assembly (done, 2026-05-23):**

- `engine/prompt.rs` ported as `src/engine/prompt.ts`. 1:1 surface —
  `assemblePrompt`, `renderTemplate`, `xmlTagFromName`, `estimateTokens`,
  `estimateMessageTokens`, `trimMessages`, `formatTimeMarker`,
  `relativeGapPhrase` plus types. Builtin system template inlined verbatim
  (same trailing-newline-strip behavior as Rust).
- Anthropic adapter gained `convertInlineSystemMessages`. Hard requirement:
  the Messages API rejects `role:"system"` in the messages array, so
  heartbeat recaps and compaction prompts ride as `<system_instruction>`
  text blocks inside a user turn (merged into the preceding user turn when
  there is one). Wrap sentinel single-sourced in
  `wrapInlineSystemInstruction`.
- OpenAI adapter passes `role:"system"` through verbatim. The Rust impl
  wrapped defensively in case OpenRouter routed to a non-OpenAI backend
  that rejects it; we don't — picking the wrong SDK for an upstream is a
  config concern, addressed by the catalog change below.
- Catalog: `defaultSdkForOpenRouterModel` auto-routes by `model_id`
  prefix on OpenRouter — `anthropic/*` → Anthropic SDK, `google/*` →
  gemini (adapter pending), `z-ai/*` → zai (adapter pending), everything
  else → openai-compat. Per-model TOML `sdk = "..."` still wins. The
  speculative `gemini`/`zai` entries land now so users get the right
  routing intent the day the adapters ship; until then, requests for those
  models fail at provider construction, not catalog resolution.
- Tests: ~50 Bun unit tests under `tests/prompt.test.ts` mirroring Rust's
  `mod tests`, ~9 tests for `convertInlineSystemMessages`, ~12 for the new
  catalog SDK resolution path.
- Parity harness: `backend/daemon/examples/dump_assemble_prompt.rs`
  dumps `AssembledPrompt` JSON for the Rust port, and
  `backend/daemon-ts/scripts/parity-check-prompt.ts` runs both ports
  against the same fixture set in `tests/fixtures/prompt/` (TZ pinned to
  America/Los_Angeles for reproducible time markers). 10 fixtures green.

Not in 4b:

- Wiring `assemblePrompt` into the actual conversation flow — slots into
  4c alongside the tool registry, since the engine wants real tools to
  call before there's anything to assemble for.
- Surfacing the resolved `sdk` in `shore model settings` — TS daemon has
  no model-settings command surface yet (greenfield); deferred to the
  command-dispatch phase. The catalog already exposes `sdk` on
  `ResolvedModel`, so the surface change is a downstream display
  addition.

**4c — engine integration + full tool registry.** Split into 4c.1
(engine wiring, done) and 4c.2 (real tools, pending).

**4c.1 — engine wiring (done, 2026-05-23):**

- SWP stream frame types ported: `stream_start`, `stream_chunk`,
  `stream_end`, `tool_call`, `tool_result`, `new_message`,
  `TokenCounts` / `TimingInfo` / `StreamMetadata`. The TS server
  understands the same wire shape the Rust daemon emits.
- `src/engine/context.ts` reads SOUL/USER/AGENTS/TOOLS/MEMORY from
  `<config>/characters/<name>/workspace/` and calls `assemblePrompt`.
  No deferred-edits snapshot dance yet — direct reads are stable
  within a single turn; the snapshot path lands with Phase 6
  compaction.
- `src/llm/generate.ts` is the orchestrator. On a `client.message`,
  it: resolves the active model via the catalog, picks the SDK,
  reads the API key from `process.env`, calls `runToolLoop`, emits
  stream frames per `ChatEvent`, and persists each new turn via
  `engine.appendMessage` (which fans out the canonical `history`
  broadcast).
- End-to-end smoketest at `scripts/generate-smoketest.ts` against
  haiku-4.5 via OpenRouter: handshake → user msg → stream_start →
  stream_chunk × N → stream_end (is_final=true) → history (with
  assistant turn). First successful real-character generation from
  the TS daemon.

Known gaps to close before 4c.2 (deliberate stubs, not deferred):
  - `displayName` defaults to `$USER` — should read
    `app.defaults.display_name` from config like Rust.
  - `thinking` config hardcoded off — should flow through the
    catalog (and accept the per-call `overrides` field).
  - Image messages still ignored (`msg.images`, `msg.image_data`).
  - SWP `regen` / `cancel` / `command` frames still rejected with
    "not implemented" errors.

**4c.2 — real tool registry (pending):** port the 9 real tools with
path-traversal / symlink-escape protections (originally Phase 5).
Until this lands, `defaultRegistry()` exposes only `roll_dice`.

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
