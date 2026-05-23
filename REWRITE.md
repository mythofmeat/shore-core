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

4c.1 polish (done, same day):
  - `displayName` reads `app.defaults.display_name` from config; falls
    back to `$USER` then `"user"` like Rust.
  - `thinking` flows from the catalog (`reasoning_effort` /
    `budget_tokens`) and honors per-call ClientMessage `overrides`
    (temperature / top_p / thinking_budget). Priority documented in
    `buildThinkingConfig`.
  - Image messages: `msg.images` (file paths) + `msg.image_data`
    (inline base64) become `ImageRef`s on the user turn. Adapters wrap
    them as Anthropic `image` blocks (base64 source) or OpenAI
    `image_url` parts (data URLs). Size-cap + mime detection in
    `src/llm/images.ts`.
  - SWP `regen` / `cancel` / `command` frames fully wired.
    `cancel` aborts the in-flight generation via AbortController →
    `stream_end finish=cancelled`. `regen` drops the trailing
    assistant turn (and tool-loop intermediates) and re-generates,
    optionally with a system-message guidance prefix. `command`
    dispatches a minimal handler set (currently just
    `inject_system_message`); unknown commands return a clear
    "not implemented" error.

**4c.2 — real tool registry (done, 2026-05-24):**

- Full 15-tool surface ported under `backend/daemon-ts/src/tools/`:
  - workspace (7): `read`, `write`, `edit`, `list_files`, `delete`,
    `file_search`, `exec`
  - basic (3): `check_time`, `roll_dice`, `set_next_wake`
  - web (2): `web_search`, `fetch_url`
  - history (1): `conversation_search`
  - activity (1): `activity_heatmap`
  - images (1): `generate_image`
- Two name changes from Rust (the names were too easily conflated by
  both humans and the model):
  - `search` → `file_search`
  - `search_history` → `conversation_search`
  Tool_use blocks in pre-rewrite `active.jsonl` still reference the old
  names. The schema mismatch in history doesn't fail the call — the
  model adapts — but it's worth knowing during cutover.
- `ToolHandler.execute` gained a `ToolContext` parameter (dependency
  injection blob with characterName/workspaceDir/searchConfig/engine/…).
  `runToolLoop` threads it through; `defaultRegistry({characterName,
  displayName})` pre-renders `{{char}}`/`{{user}}` placeholders in
  descriptions at registration time.
- Path safety lives in `src/tools/paths.ts` — single source of truth
  for the workspace-confinement rules. Mirrors Rust's `resolve_path` +
  `is_prompt_visible_path` + `normalize_protected_path`:
  - `..` traversal rejected
  - absolute paths rejected
  - resolved paths must canonicalize inside the workspace (symlink-escape
    rejected)
  - `memory/` and `workspace/` display prefixes tolerated
- Exec sandbox is a hand-rolled POSIX shell-words splitter +
  allowlist + path-arg validation. No shell invocation (`spawn` with
  `shell: false`). Allowlist verbatim from Rust:
  `ls cat rg git wc pwd sort uniq dirname basename file stat du df which
   whoami date tree fd cargo rustc rustfmt clippy rust-analyzer npm pnpm
   yarn make cmake`.
- Graceful degradation for subsystems that don't ship until later phases:
  - `set_next_wake` is always in the schema (cache stability); handler
    refuses with "only available during heartbeat ticks" when
    `ctx.scheduleNextWake` is undefined (Phase 8 wires it).
  - `file_search` hybrid/vector modes fall back to lexical with
    `semantic_unavailable: "embedder not configured"` (Phase 6 wires
    the embedder).
  - `conversation_search` reads whatever's on disk —
    `active.jsonl` + `compaction.json` + `segments/*.jsonl` —
    so the read path works the day compaction ships in Phase 6.
  - `activity_heatmap` returns an empty 24-hour heatmap when
    `ctx.activityStats` is undefined.
  - `generate_image` requires `ctx.imageGenConfig`; errors cleanly
    otherwise. Image gen itself uses the OpenAI SDK
    `client.images.generate()` directly (most providers are
    OpenAI-compatible for image gen).
- `roll_dice` schema is `{ notation: "2d6+5" }` (matches Rust),
  replacing the Phase 4a stub's `{count, sides}` shape.
- Tests:
  - `tools_workspace.test.ts` — read/write/edit/list/delete/file_search
    happy path + safety
  - `tools_exec.test.ts` — allowlist, path-arg validation,
    shell-chaining rejection, shellSplit semantics
  - `tools_web.test.ts` — Tavily key-required, fake-server fetch_url,
    HTML stripper edge cases
  - `tools_history.test.ts` — conversation_search across synthetic
    segments + active, time range, alternatives
  - `tools_basic.test.ts` — check_time format, dice parser, set_next_wake
    gating + clamping
- Smoketest: `scripts/tools-smoketest.ts` drives the model through
  `check_time` + `roll_dice` against haiku-4.5 via OpenRouter and
  verifies both tool_call frames appear before the final stream_end.
- The Rust tool registry stays a dispatch-by-name match block; the TS
  registry is a `Map<name, ToolHandler>` and per-character-built. The
  shape difference is intentional — TS doesn't need the trait + boxed
  futures, and per-character registry build lets `{{char}}/{{user}}`
  render once instead of being re-templated at every call site.

Tools that ship with graceful-fallback shims here will need re-testing
once their dependencies land. See **Phase 6** for `file_search` (hybrid
mode), `conversation_search` (real compacted segments), and `write`/`edit`
(deferred-edits queue write); **Phase 8** for `set_next_wake` and
`activity_heatmap`. `generate_image` is the odd one out — it's a single
config wiring (`[image_generation]` table → `imageGenConfig` on
`ToolContext`) with no phase dependency. Wire it whenever the config
loader gains the `[image_generation]` section, and add a live-API test
behind an env-gated key.

### Phase 5: tools

(Subsumed by Phase 4c.2 above — full registry already ported. Retained
here as a placeholder so the original numbering still maps cleanly.)

### Phase 6: memory + dreaming + compaction

Split into 6a (memory primitives + deferred-edits queue), 6b (compaction),
6c (workspace_index + embedder + hybrid_search), 6d (dreaming). Each
sub-phase ends with a parity check.

**PORT with care.** This subsystem has subtleties (single-flight locks,
throughline carry-forward, the compaction lock fix in #30, the
MEMORY.md active-snapshot sentinel that keeps un-applied edits out of
the prompt). Read the existing code carefully and write tests that pin
the observable behavior before porting. *Do not* "rewrite from
scratch" the memory subsystem — transcribe it.

**6a — memory primitives + deferred-edits queue (done, 2026-05-24):**

- `src/memory/markdown_store.ts` ports `MarkdownMemoryStore`:
  recursive list (excludes `.dreams/` / `dreaming/` / top-level
  `DREAMS.md` / `MEMORY.md`), write/read/delete with empty-parent
  cleanup, ranked text search, traversal + symlink-escape rejection.
- `src/memory/deferred_edits.ts` ports the protected-file machinery:
  active-prompt snapshot under `<characterDataDir>/active_prompt/`,
  `queueDeferredEdit` → `deferred_edits.jsonl`, `applyDeferredEdits`
  refreshes the snapshot and clears the queue, MEMORY.md zero-byte
  sentinel blocks live activation. Re-exports the normalize* helpers
  from `tools/paths.ts` so the conceptual home matches Rust without
  duplicating the impl. Character-dir helpers
  (`characterConfigDir`/`characterWorkspaceDir`/`characterMemoryDir`/
  `characterWorkspaceFile`) inlined here rather than scaffolding a
  shore-config-style path module — promote to a shared location if
  other modules grow to need them.
- `src/engine/segments.ts` ports `SegmentReader`. `tools/history.ts`
  was carrying inline manifest+segment readers; refactored to delegate.
  The handler now propagates segment-read errors as `ToolError("Io",
  …)` — matches Rust, which uses `serde_json::from_str` straight
  through; the previous TS tolerance was wrong.
- `tools/workspace.ts` write/edit handlers now call `queueDeferredEdit`
  via `decorateForPromptVisible(_, _, ctx)`. Queue failures log + continue
  (the file write already succeeded); mirrors Rust's
  `ContextToolContext::defer_edit` warn-and-continue.
- Tests: `markdown_store.test.ts` (11), `deferred_edits.test.ts` (11),
  `segments.test.ts` (4), plus 4 new cases in `tools_workspace.test.ts`
  pinning the queue side effects. 211 pass / 0 fail across the suite;
  parity harness still green.

What 6a does NOT do:
- No write side of compaction yet — `apply_deferred_edits` exists but
  no caller invokes it (6b will, at the compaction boundary).
- The `loadMemoryIndex` / active-prompt snapshot helpers aren't yet
  wired into `engine/context.ts`'s prompt assembly — that still reads
  workspace MEMORY.md directly. Wiring lands when compaction starts
  producing snapshots in 6b; until then, direct read is correct.
- Embedder/hybrid_search still falls back to lexical (6c).

**6b — compaction (pending):**

- LLM-driven memory-write loop, archive-and-retain into `segments/`.
- Single-flight `try_begin_compaction` keyed on character data root.
- IdleTimer + activity gating, force-compact threshold.
- MEMORY.md throughline carry-forward.
- Fire `applyDeferredEdits` at the compaction boundary so 6a's queue
  entries actually activate.
- **Exit criterion:** byte-identical MEMORY.md for a deterministic test
  input.

**6c — workspace_index + embedder + hybrid_search (pending):**

- Wire `file_search` hybrid/vector modes against a real embedder hook
  on `ToolContext`. Live test gated on an embedder endpoint.

**6d — dreaming (pending):**

- `run_librarian_sweep` port + `dreams_log` audit trail.

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
- **Wire up the tool-side fallbacks landed in 4c.2:**
  - `set_next_wake` currently errors with "only available during
    heartbeat ticks" when `ctx.scheduleNextWake` is undefined. Wire the
    real scheduler onto the heartbeat context and add a test that
    asserts the hook gets called with the clamped hours + reason. The
    schema stays in the registry during normal user turns regardless —
    that's load-bearing for cache stability.
  - `activity_heatmap` currently returns an empty 24-hour heatmap when
    `ctx.activityStats` is undefined. Wire the autonomy stats hook and
    add a test that asserts real per-hour densities + classifications
    flow through (mirror the Rust
    `test_activity_heatmap_with_autonomy_data` case).
- **Exit criterion:** keepalive interval respected, no stale-cache
  rebuilds, no extra LLM calls vs. the Rust daemon under the same input.
  AND both heartbeat-dependent tools above are exercised in tests with
  the real hooks in place.

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
