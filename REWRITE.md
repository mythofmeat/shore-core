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

**6b — compaction (done, 2026-05-24):**

- `src/memory/compaction/types.ts` ports the trait-equivalent interfaces
  + `CompactionConfig` (inlined from `core/config/src/app.rs` — the TS
  loader doesn't yet expose `app.memory.compaction`; defaults match Rust
  byte-for-byte). `CompactionLlm` keeps the `cachedRequest?: ChatRequest`
  hook on the signature even though the fresh-prefix path is the only
  one wired today (see notes below). `CompactionError` mirrors the Rust
  error tags.
- `src/memory/compaction/parser.ts` ports `parse_compaction_response`,
  `extract_xml_tag`, and the bundled `DEFAULT_COMPACT_SYSTEM` /
  `DEFAULT_COMPACT_PROMPT` templates (inlined verbatim from
  `prompts/memory/compaction/*.md` so the daemon bundles as a single
  binary).
- `src/memory/compaction/lock.ts` ports `try_begin_compaction` +
  `CompactionRunGuard`. JS is single-threaded, so the lock is a
  `Set<string>` of locked keys with a release callback — no
  `try_lock_owned` race semantics to mirror. Same keying as Rust
  (character data root, so test instances reusing the same name in
  different data dirs don't collide). `withCompactionLock` is the safe
  helper.
- `src/memory/compaction/idle_timer.ts` ports `IdleTimer` +
  `ActivityNotify` (the minimal Tokio `Notify` equivalent the timer
  needs). `waitForIdle` resets on `notify()` exactly like Rust's
  `tokio::select!` arm.
- `src/memory/compaction/manager.ts` ports `CompactionManager`. Static
  helpers (`findTurnSplit`, `countTurns`, `isToolLoopMessage`,
  `shouldForceCompact`, `hasEnoughTurns`, `buildSystem`,
  `buildFinalMessage`, `buildMessages`, `buildPrompt`,
  `buildExistingMemoryContext`, `dedupeFileOps`, `writeAllowedPath`,
  `filterFileOps`, `isMemoryIndexPath`) all match the Rust shapes 1:1.
  `compact()` implements the full pass: split, build prompt, call LLM,
  parse, filter, write markdown files into the workspace (handling the
  MEMORY.md special case via `noteMemoryIndexDeferred`), archive via the
  ConversationManager, append a dreams-log entry — and rolls back the
  markdown writes on archive failure via compensating deletes.
- `src/memory/compaction/conversation_manager.ts` ports
  `RealConversationManager`. Reads the pre-captured `activeContent`,
  splits at `keepLastN`, writes the head into a numbered
  `segments/NNNN.jsonl` file, updates `compaction.json`, and atomically
  rewrites `active.jsonl`. Mirrors Rust's archive-with-malformed-jsonl
  semantics (line-count counts, JSON validity isn't policed).
- `src/memory/compaction/llm.ts` ports `RealCompactionLlm` as a thin
  wrapper around the existing `AnthropicProvider` / `OpenAIProvider`
  adapters. Single-shot, no tools, collects text deltas. The cached-
  prefix optimization the Rust impl wires through `LedgerClient::
  build_request_with_provider_keys` + `convert_inline_system_messages`
  is **deferred** — it needs the autonomy manager's
  `cached_last_request` hook (Phase 8) and the ledger (Phase 7), neither
  of which exist in TS-land yet. The fresh path is semantically
  correct, just suboptimal for cache. The `cachedRequest` parameter
  stays on the interface so the optimization can land later without an
  ABI change.
- `src/memory/compaction/background.ts` ports `run_compaction`. Single
  read of `active.jsonl` (parse + raw content for the segment archive,
  eliminating the TOCTOU window). Acquires the guard, loads templates,
  calls `compact()`, and — critically — fires `applyDeferredEdits` at
  the compaction boundary so the MEMORY.md sentinel produced by 6a's
  workspace writes plus the new MEMORY.md throughline from this pass
  become prompt-active.
- `src/engine/context.ts` now reads MEMORY.md via `loadMemoryIndex`
  (active snapshot if present, else canonical) instead of reading
  workspace/MEMORY.md directly. SOUL/USER/AGENTS/TOOLS still come
  straight from workspace — protected-file edits also queue, but those
  four don't yet route through the active-snapshot reader (deferred to
  when there's an ergonomic helper for the non-MEMORY slots, or when
  the heartbeat flow lands in Phase 8 and the routing matters).
- `src/memory/dreams_log.ts` ports `dreams_log.rs`. Lives outside
  `compaction/` because dreaming uses it too (Phase 6d will).
- Tests:
  - `compaction_parser.test.ts` — 13 cases mirroring Rust's `parser::tests`
  - `compaction_lock.test.ts` — 5 cases pinning the single-flight semantics
  - `compaction_idle_timer.test.ts` — 2 cases (real-time, not faked)
  - `compaction_manager.test.ts` — 23 cases mirroring Rust's
    `compaction::tests`: prompt building, turn-split + tool-loop,
    force-compact gating, the end-to-end compact() table (writes,
    archive, dream log, deferred-edit queue, dry run, private skip,
    insufficient messages, rollback)
  - `compaction_conversation_manager.test.ts` — 5 cases mirroring the
    `RealConversationManager` test block (archive split, keep-all,
    segment numbering, malformed-line behavior, empty-content no-op)
  - `compaction_background.test.ts` — 3 cases pinning the lock +
    apply-deferred boundary end-to-end
  - `context_memory_snapshot.test.ts` — 3 cases pinning the
    `engine/context.ts` → snapshot wiring (canonical when no snapshot,
    deferred edits stay invisible, sentinel-blocked writes activate
    after apply)

What 6b does NOT do (intentionally deferred):

- **No autonomy manager wiring.** Rust's `handler/task.rs` calls
  `should_compact_now`, then `spawn_inline_compaction`, then
  `run_compaction` after each generation; on completion it reloads the
  engine + applies deferred edits + notifies the autonomy state machine.
  TS land doesn't have the autonomy manager yet — that lives in Phase 8
  alongside heartbeat. `runCompaction` is a callable that can be wired
  in once the manager lands.
- **No cached-prefix LLM path.** The fresh-prefix path is wired; the
  cache-preserving path that the Rust regression test
  `cached_compaction_request_matches_chat_prefix_byte_for_byte` pins is
  deferred until the autonomy manager grows a `cached_last_request`
  hook and the ledger lands the `LlmRequest` mirror. The interface
  keeps `cachedRequest` so the optimization is a drop-in later.
- **No idle-timer task spawned by the engine.** `IdleTimer` is ported
  and tested standalone; nothing in the engine actually spawns the
  wait-for-idle loop yet. That's an autonomy-manager job and lands
  with Phase 8.
- **Config loader doesn't expose `[app.memory.compaction]`.** Callers
  build `CompactionConfig` directly today. The loader extension is a
  one-liner once Phase 6c/8 needs it.

- **Exit criterion:** byte-identical MEMORY.md for a deterministic test
  input (the `writes MEMORY.md but refuses generated/protected/dreaming
  paths` case in `compaction_manager.test.ts` pins this).

**6c — workspace_index + embedder + hybrid_search (done, 2026-05-24):**

- `src/memory/workspace_index.ts` ports the Rust workspace embedding
  index:
  - persisted JSON cache at
    `$SHORE_CACHE_DIR/characters/<Character>/workspace_index.json`
  - per-index async lock around load → mutate → save
  - stale detection by size, mtime, embedder model id, and
    `max_embed_chars_per_file`
  - symlink skip, max-file/max-indexed-files/max-total-bytes caps,
    oversized/non-UTF8 skip entries, vector refresh batching, cosine
    scoring, lexical/semantic score fusion, path-scoped filtering
- `src/llm/embed.ts` ports the dyn-compatible embedder abstraction and
  OpenAI-compatible `/v1/embeddings` provider. Resolution mirrors Rust:
  `defaults.embedding` wins, otherwise the first `[embedding.*]` profile;
  `provider = "local"` returns the migration error; instances are cached
  by provider/model/api-key-env/base-url/dimensions.
- Config loader now exposes `defaults.embedding`, raw `[embedding.*]`
  profiles, and `[memory.retrieval]` caps. `main.ts` resolves the
  embedder per generation (falling back cleanly when missing/unusable)
  and threads both the embedder and `workspaceIndexPath` into
  `ToolContext`.
- `file_search` now uses real hybrid/vector search when `ctx.embedder`
  and `ctx.workspaceIndexPath` are present. Transient index/embedder
  failures fall back to lexical with `semantic_unavailable = <error>`.
  No-embedder hybrid/vector still falls back to lexical with
  `semantic_unavailable: "embedder not configured"`.
- Tests:
  - `workspace_index.test.ts` pins cosine edge cases, semantic ranking,
    cached-vector reuse, and oversize skip entries.
  - `tools_workspace.test.ts` covers real vector-mode dispatch and
    path-scoped hybrid search through `file_search`.
  - `config_loader.test.ts` covers embedding profile + retrieval config
    parsing.
  - `embed.test.ts` covers OpenAI-compatible response parsing,
    resolution errors, and a live smoke test gated by
    `SHORE_EMBED_LIVE=1` plus `SHORE_EMBED_*` endpoint env.
- Verification: `bun run typecheck` green; `bun test` green
  (276 pass / 7 provider-live skips).

What 6c does NOT do:

- No background/autonomy usage of the index yet. Heartbeat and dreaming
  will pass the same embedder/index path once Phase 8/6d wire their
  contexts.
- The live embedder smoke test is intentionally opt-in and was not
  exercised in the ordinary suite; set `SHORE_EMBED_LIVE=1` with an
  OpenAI-compatible endpoint to run it.

**6d — dreaming (done, 2026-05-24):**

- `src/memory/dreaming.ts` ports the production AI-librarian
  `run_librarian_sweep` path:
  - builds the private librarian prompt from the Rust template, including
    active-prompt `SOUL.md` / `USER.md` identity blocks
  - exposes `[memory.dreaming]` config shape (`enabled`, `frequency`,
    `max_tool_rounds`) through the TS config loader
  - runs a private tool loop with the librarian tool subset
    (`read`, `write`, `edit`, `list_files`, `file_search`,
    `conversation_search`, `check_time`)
  - blocks `exec` and all non-librarian tools during the pass; dry-run
    also blocks `write` / `edit`
  - records inspected paths/searches, changed tool paths, tools used,
    tool rounds, and the final internal report
  - writes canonical `workspace/MEMORY.md` fallback when the model leaves
    it missing/empty, and queues `MEMORY.md` through deferred edits so it
    does not become prompt-active before the next compaction/reload
  - writes daemon-owned `DREAMS.md` audit entries in the character data
    dir via the already-ported `dreams_log` module
  - writes machine state to
    `$SHORE_DATA_DIR/<Character>/dreams/state.json`, not legacy
    workspace `.dreams/`
- Tests at `tests/dreaming.test.ts` cover successful tool-driven
  librarian passes, dry-run write blocking, MEMORY.md fallback + audit,
  zero-tool-round fallback behavior, and protected prompt-file deferred
  edits. Config parsing for `[memory.dreaming]` is covered in
  `config_loader.test.ts`.
- Verification: `bun run typecheck` green; `bun test` green
  (281 pass / 7 provider-live skips).

What 6d does NOT do:

- No autonomy/scheduler integration yet. Phase 8 owns cron due checks,
  background task spawning, and wake/heartbeat interaction.
- No cached-request prefix reuse yet. Like compaction's cached-prefix
  optimization, this needs the Phase 8 autonomy manager's warmed request
  hook and the Phase 7 ledger/request mirror.
- The legacy deterministic diagnostic sweep remains unported. Rust keeps
  it only for dry-run diagnostics and fallback-oriented unit coverage;
  production dreaming uses the AI librarian path above.

### Phase 7: ledger + cache forensics (done, 2026-05-24)

- [x] `bun:sqlite` against the existing `ledger.db`, with the Rust-compatible
  `calls`, `pricing`, and `usage_budget_warnings` tables plus best-effort
  migrations for `cache_ttl`, `api_key_name`, and `cost_source`.
- [x] Chat generation records one row per provider round-trip, including
  message vs. tool-loop call type, token counts, cache reads/writes, cache
  TTL, timing, finish reason, and thinking flag.
- [x] Cache anomaly detection (`unexpected_write`, `keepalive_miss`) is
  ported for ledger writes and cache-health summaries.
- [x] `shore usage` command payloads are backed by the TS ledger for summary,
  by-call-type, by-kind, by-api-key, anomaly, CSV, and TSV modes.
- [x] `cache_forensics.jsonl` request-side breakpoint logging and
  response-side cache metric logging are wired when
  `[advanced].cache_forensics = true`.
- [x] Compaction and AI-librarian dreaming provider calls can write
  `compaction`/`dreaming` ledger rows when those paths are given the shared
  ledger sink.
- [x] `PricingEngine` ported at `src/ledger/pricing.ts`: memory + DB cache,
  `getOrFetch`/`storePricing`/`getCachedPricing`/`clearCache`,
  `toOpenRouterId` (`-` → `.` minor-version dotting for Anthropic and
  passthrough for `<provider>/<model>` ids), and `calculateCost` honoring
  the Anthropic 1h `cache_write` 1.6× multiplier. Catalog fetch hits
  `https://openrouter.ai/api/v1/models` with an injectable fetcher for
  tests.
- [x] Per-component costs populate at write time from the cached catalog
  when `Ledger.recordCall` is given a `PricingEngine`. A new
  `Ledger.recalculateCosts(modelId, pricing)` rewrites every
  `pricing_catalog`-sourced row for a model when pricing refreshes; rows
  marked `provider_reported` are left alone. `shore usage --refresh-pricing`
  and `--recalculate` paths in `usage.ts` walk the distinct
  `(provider, model)` set from the ledger and dispatch accordingly.
- [x] Budget evaluation + spike warnings ported at `src/ledger/budget.ts`:
  `budgetStatuses`, `enforceBudgetForCall`, `spikeWarnings`, and
  `newlyCrossedBudgetWarnings` (with `usage_budget_warnings` row-dedup +
  over-limit re-fire). UTC and local period windows with `reset_hour`,
  `reset_day_of_week`, and `reset_day_of_month` anchors and short-month
  clamping. Wired through the message + regen handlers so an active
  block-action budget short-circuits the LLM call with an `error` frame
  (`code: "usage_budget_blocked"`) and newly crossed thresholds emit a
  `usage_budget_warning` command_output frame after each generation.
- [x] Config loader exposes the full `[usage]` table including
  `[[usage.budgets]]` and `[usage.spike_warnings]` (anchor fields,
  `usage_kind` filters, per-budget `allow_compaction_over_budget`
  override).
- Tests: `tests/pricing.test.ts` (14 cases), `tests/budget.test.ts`
  (10 cases), plus expanded `tests/config_loader.test.ts` coverage for
  `[usage]` parsing. `bun test` green: 317 pass / 7 provider-live skips.

What 7 does NOT do (intentionally deferred to Phase 8):

- Autonomy/heartbeat ledger rows
  (`heartbeat`/`heartbeat_tool_loop`/`keepalive` call types). Compaction +
  dreaming already share the ledger sink, but the production scheduler
  wiring lives with the autonomy state machine.

- **Exit criterion:** `shore usage` command output matches Rust daemon for
  the same conversation. Met as of 2026-05-24 — summary, budget, spike,
  refresh-pricing, and recalculate modes share row shapes with the Rust
  port.

### Phase 8: heartbeat + autonomy

Split into 8a → 8d in dependency order so each slice can land + ship on
its own. The full state machine — keepalive, heartbeat ticks, scheduler
— lands in 8b/8c; 8a only covers the rhythm-tracker substrate and the
first tool that consumes it.

#### Phase 8a: activity tracker + heatmap wiring (done, 2026-05-24)

- [x] `src/autonomy/activity.ts` — 1:1 port of
  `backend/daemon/src/autonomy/activity.rs`. Same constants
  (`SESSION_GAP_SECS=1800`, `SUFFICIENT_DATA_MSGS=5`,
  `SUFFICIENT_HEATMAP_MSGS=20`, `WEEKDAY_HEATMAP_MIN=5`,
  `PEAK/TROUGH_HOUR_THRESHOLD`, `STATS_CACHE_TTL_SECS=60`,
  `SESSION_MEDIANS_WINDOW=30`, `SESSION_TEMPO_WINDOW=10`,
  `ANOMALY_Z_SCORE=1.5`); `performance.now()` stands in for tokio's
  monotonic `Instant`; chrono `num_days_from_monday` is mirrored via
  `(getDay()+6)%7` so the Monday-indexed weekday histogram lines up
  byte-for-byte with the Rust output on the same input.
- [x] `src/autonomy/registry.ts` — `AutonomyRegistry` wrapping
  `Map<string, ActivityTracker>`. `ensureState(engine)` is idempotent
  and back-fills from `active.jsonl` + all segments on first call
  (90-day cutoff, skip tool-result-only user turns). Exposes the
  `activity_stats(name) -> (stats, message_count)` API the Rust
  heartbeat/keepalive loop needs in 8b.
- [x] Wired through `src/llm/generate.ts` + `src/main.ts`: the daemon
  instantiates `const autonomy = new AutonomyRegistry()`, calls
  `autonomy.ensureState(engine)` + `autonomy.notifyUserMessage(...)` on
  each fresh user turn, and passes an `activityStats` adapter into the
  `ToolContext` that maps the autonomy struct → the tools-side
  `ActivityStats` shape (turnCount sourced from `snap.messageCount`).
- [x] Tests: `tests/activity.test.ts` mirrors the full Rust
  `activity.rs::tests` block (tempo-score logistic at 30s/5min/15min/30min/
  empty, median odd/even/empty, hour-histogram weekday filtering + global
  fallback, peak/trough/all-zero classification, session detection +
  single-session, engagement score, sufficient/insufficient data,
  z-score anomaly, cache invalidation, session-medians window cap,
  backfill noop/empty/sort/then-record). Plus the integration check
  `activity_heatmap tool > returns real density + classifications when
  the autonomy hook is wired` covers the empty-shape vs. wired-shape
  divergence end-to-end through the tool handler. `bun test` 344 pass /
  7 skip / 0 fail.
- **Exit criterion:** `activity_heatmap` returns non-empty densities
  + classifications for a character that's had `≥WEEKDAY_HEATMAP_MIN`
  messages on the current weekday (or `≥SUFFICIENT_DATA_MSGS` overall
  via global fallback). Met as of 2026-05-24.

What 8a does NOT do (deferred to 8b/8c):

- Heartbeat/keepalive ticks — the autonomy *state machine* (idle
  detection, due checks, warm-prefix rebuild scheduling) isn't started.
- `set_next_wake` still errors with "only available during heartbeat
  ticks" because `ctx.scheduleNextWake` stays undefined during user
  turns. The schema stays in the registry regardless — that's
  load-bearing for cache stability.
- Autonomy/heartbeat ledger rows
  (`heartbeat`/`heartbeat_tool_loop`/`keepalive` call types — the
  Phase-7 deferred item).

#### Phase 8b: heartbeat state machine + `set_next_wake` wiring (done, 2026-05-24)

- [x] `src/autonomy/heartbeat.ts` — 1:1 port of
  `backend/daemon/src/autonomy/heartbeat.rs`. Same constants
  (`MIN_WAKE_INTERVAL_MS=1h`, `MAX_WAKE_INTERVAL_MS=48h`); same
  `HeartbeatAction.{None,RunTick}` enum; same `HeartbeatClock` surface
  (`tick`, `schedule`, `onUserMessage`, `restore`, `forceWake`,
  `forceDormant`, `forceActive`, `nextWake`, `ticksWithoutUser`,
  `maxIdleTicks`, `lastUserAt`, `default/minWake/maxSilent` accessors,
  `isDormant`, `stateAt`). `performance.now()`-style monotonic ms
  stands in for tokio's `Instant`. Preserves the four-step `tick()`
  ordering verbatim (bootstrap-when-undefined → not-due → abandonment
  guard → fire-and-clear), the `schedule()` clamp-to-bounds, and
  `onUserMessage()`'s `max(existing, now+minWake)` deadline push.
- [x] `src/autonomy/registry.ts` — `AutonomyRegistry` now owns a
  `Map<string, HeartbeatClock>` alongside the activity trackers.
  `ensureState(engine)` creates the clock if absent;
  `notifyUserMessage(name)` forwards to `clock.onUserMessage(now)`;
  new `scheduleNextWake(name, hours, reason)` is the implementation
  behind the `ctx.scheduleNextWake` hook that `set_next_wake` consumes.
  Returns the same plain-string payload as Rust's
  `schedule_next_wake_in_state` (`json!(format!("Scheduled next moment
  in {h:.1} hours."))`) so wire-shape parity is preserved through the
  tool's downstream `JSON.stringify(result)`. Default `HeartbeatConfig`
  lives in the registry until the config-loader hookup in 8c.
- [x] `src/tools/registry.ts` — widened `ScheduleNextWake` return type
  from `Record<string, unknown>` to `unknown` so the registry's string
  return matches the Rust wire format. No other changes to
  `set_next_wake` — the 4c.2 refusal path (throws when
  `ctx.scheduleNextWake === undefined`) remains intact for user turns.
- [x] Tests:
  - `tests/heartbeat.test.ts` mirrors the full Rust
    `heartbeat.rs::tests` block (21 tests: lifecycle, tick-count guard,
    silent-duration guard, schedule clamping below/above bounds,
    on_user_message reset/preserve/push/bootstrap-from-none/wake-from-
    abandoned, restore with future and past wakes, state_at labels).
  - `tests/autonomy_registry.test.ts` (5 tests) covers the integration:
    ghost-character throw, end-to-end set_next_wake → clock mutation
    with wire-shape assertion, below-minimum clamping, notifyUserMessage
    forwarding into the clock, and the 4c.2 refusal path still firing.
  - `bun test` 370 pass / 7 skip / 0 fail; `bun run typecheck` clean.
- **Exit criterion:** `set_next_wake` is callable end-to-end through a
  test harness with a real clock-backed `ctx.scheduleNextWake` hook —
  no more "only available during heartbeat ticks" rejection when a
  driver supplies the hook. Met as of 2026-05-24.

What 8b does NOT do (deferred to 8c/8d):

- The actual LLM dispatch when `tick()` returns `RunTick` (the private
  heartbeat call with tools + system marker) — 8d.
- `HeartbeatLog` JSONL events and the autonomy/heartbeat ledger rows
  (`heartbeat`/`heartbeat_tool_loop`/`keepalive` call types — the
  Phase-7 deferred item) — 8d.

#### Phase 8c: ticker + keepalive substrate (done, 2026-05-24)

- [x] `src/autonomy/cache_keepalive.ts` ports `cache_keepalive.rs`:
  same 55-minute default ping interval (with
  `SHORE_KEEPALIVE_INTERVAL_SECS` override), same 18-hour breakeven,
  same retry backoff, and the same "caller confirms success with
  `onCacheWarmed`" contract.
- [x] `AutonomyRegistry` now restores/saves `autonomy_state.json`
  version 4 in each character data dir, converting RFC3339 wall-clock
  deadlines to/from the monotonic-ms clock used by `HeartbeatClock`.
- [x] `AutonomyRegistry` owns a per-character `CacheKeepalive` and
  mirrors wake deadlines from `notifyUserMessage` and `scheduleNextWake`.
  Guard trips clear the keepalive wake so dormant characters stop
  pinging, matching Rust's propagation.
- [x] Production ticker substrate wired: `main.ts` constructs the
  registry with the loaded autonomy config and `autoStartTicker`, and
  handshakes ensure state for the selected character. `tickCharacter()`
  drives `clock.tick()` + `keepalive.tick()` and returns the resulting
  actions for the async 8d driver.
- [x] Config loader exposes `[behavior.autonomy]` and
  `[behavior.autonomy.heartbeat]` with Rust defaults
  (`enabled=false` for autonomy, heartbeat enabled, 1h fallback, 3
  idle ticks, 48h idle duration, 1h minimum latency, 12 tool rounds,
  3 wrap-up rounds).
- Tests: `cache_keepalive.test.ts` mirrors the Rust cache-keepalive
  block; `autonomy_registry.test.ts` adds persistence/restore +
  keepalive-mirror coverage; `config_loader.test.ts` covers heartbeat
  TOML parsing. Verification: targeted Bun tests and `bun run
  typecheck` green.

What 8c does NOT do:

- No async execution for returned `HeartbeatAction.RunTick` or
  `CacheKeepaliveAction.Ping`; 8d consumes those actions.

#### Phase 8d: heartbeat + keepalive LLM dispatch (done, 2026-05-24)

- [x] `src/autonomy/dispatch.ts` consumes ticker actions. `RunTick`
  builds the same private heartbeat suffix shape (active HEARTBEAT.md
  guidance + dynamic current-time affordance prompt), runs the full tool
  registry without persisting ephemeral heartbeat/tool-loop turns, wires
  `set_next_wake` through the registry-backed scheduler hook, and
  persists only extracted `<sendMessage>...</sendMessage>` content as
  an autonomous assistant message.
- [x] `GenerateOptions` now supports the heartbeat call shape:
  `systemSuffix`, `persistTurns=false`, `maxIterations`,
  `ledgerCallTypes`, `onPreparedRequest`, and a wrap-up nudge inserted
  after `max_tool_rounds` before `wrap_up_grace_rounds` begins. Normal
  user-message generation keeps the old defaults.
- [x] `AutonomyRegistry` stores the cache-stable last chat request from
  user/regen calls. Keepalive pings clone that request, set
  `maxTokens=1`, append the minimal `"."` user turn, and record
  `call_type='keepalive'`. If the process restarted and no cached
  request exists, the dispatcher rebuilds from disk only when history is
  between turns (last message is assistant), avoiding mid-turn cache
  divergence.
- [x] `src/autonomy/heartbeat_log.ts` ports the JSONL ring buffer
  persistence shape (100-event cap, malformed-line skip, atomic rewrite
  on flush). Ticks, message sent/skipped events, wrap-up nudges,
  dormant guard trips, and keepalive success/skip/failure are appended
  to `<character data>/heartbeat.jsonl`.
- [x] Ledger rows are written with
  `call_type='heartbeat'`, `'heartbeat_tool_loop'`, and `'keepalive'`,
  preserving the Phase-7 usage-kind mapping. Usage budgets are checked
  before heartbeat and keepalive calls; over-budget background work is
  skipped and logged.
- Tests: `generate_ledger.test.ts` covers private heartbeat-style
  generation (system suffix, no persistence, wrap-up nudge, heartbeat
  ledger call types, and pre-suffix cached-request capture).
  `heartbeat_log.test.ts` covers JSONL load/flush behavior. Full
  `bun test` and `bun run typecheck` should stay green after this slice.
- **Exit criterion:** keepalive interval/action math is pinned by
  `cache_keepalive.test.ts`; request reuse/rebuild is wired through
  the dispatcher. Live provider parity is still opt-in with the
  existing env-gated cache tests; ordinary verification does not spend
  API credits.

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
