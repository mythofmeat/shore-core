# Daemon Rewrite — Rust → TypeScript (Bun)

**Status:** active, started 2026-05-23.

## Parity audit (2026-05-24)

The phase status lines below claim more than the code holds. This section is
the corrective: every gap found in a top-to-bottom audit of
`backend/daemon/src/` vs. `backend/daemon-ts/src/`, ordered by cutover impact.
Phase 9b is **not** ready for soak until the cutover blockers below land.

The per-phase notes after this section have been annotated in place to point at
this audit where their original "done" claims overshoot.

### Cutover blockers (Phase 9b cannot proceed)

1. **Autonomous compaction is not wired.** `runCompaction`
   (`backend/daemon-ts/src/memory/compaction/background.ts:77`) is implemented
   and unit-tested, but no production code path imports `memory/compaction`.
   Rust calls it from two sites: post-generation
   (`backend/daemon/src/handler/task.rs:367` → `spawn_inline_compaction`, gated
   on `AutonomyManager::should_compact_now`) and the per-character autonomy
   tick (`backend/daemon/src/autonomy/manager.rs:1172` →
   `execute_idle_compaction`). The TS handler (`src/llm/generate.ts`) has
   neither. Net effect on a TS-only deployment: `active.jsonl` grows
   unbounded, MEMORY.md is never refreshed from new turns, and the
   `compact`/`memory` CLI commands don't exist.
2. **Autonomous dreaming is not wired.** `runLibrarianSweep`
   (`backend/daemon-ts/src/memory/dreaming.ts`) is implemented and
   unit-tested; no production code path calls it. Rust drives it from
   `autonomy/manager.rs:1290` (cron tick) and `commands/state/memory.rs:175`
   (manual `shore memory dream`). No TS analogue for either. No `DREAMS.md`
   audit entries are ever appended.
3. **CLI command surface is ~6% implemented.** Rust's `commands/mod.rs:73`
   dispatches **35** commands; the TS dispatcher
   (`backend/daemon-ts/src/main.ts:697`) handles **2** (`inject_system_message`,
   `usage`). Everything else returns `command "X" not implemented`. The
   missing surface includes everything the CLI/TUI actually uses:
   - Navigation: `list_characters`, `switch_character`, `character_info`
   - Conversation: `log`, `history_page`, `get`, `edit`, `delete`, `alt`,
     `list_alternatives`, `inject_system` (note also that the one inject
     command TS ships is named `inject_system_message` — name drift from
     Rust's `inject_system`)
   - State: `status`, `list_models`, `model_info`, `switch_model`,
     `reset_model`, `set_model_setting`, `model_settings`,
     `memory_changelog`, `memory_dream`, `memory_dreams`, `memory`, `compact`,
     `config`, `config_check`, `config_reset`, `diagnostics`, `heartbeat_log`,
     `heartbeat_tick_now`, `heartbeat_set_dormant`, `heartbeat_set_active`
   - Providers: `list_providers`, `refresh_provider_models`,
     `refresh_all_provider_models`, `list_provider_models`
4. **Preferences module is not ported.** Rust's `backend/daemon/src/preferences/mod.rs`
   (1648 lines) owns global + per-character `models.toml` (selected model,
   sampler defaults, per-`(provider, model)` temperature/top_p/thinking
   overrides) and is the durable backing store for `switch_model`,
   `set_model_setting`, `model_settings`, `reset_model`, plus the
   `apply_sampler_overlay` call that overlays saved settings onto the
   resolved model at request time (`handler/task.rs:134`). Nothing in TS
   reads or writes those files; per-call overrides via SWP `overrides` are
   the only way to bias a generation, and no settings survive a restart.
5. **Multi-key credential fallback is not ported.** Rust wraps every LLM
   call in `stream_with_credential_fallback`
   (`backend/daemon/src/handler/key_fallback.rs`, 460 lines, invoked from
   `handler/task.rs:283`). When a key fails with a credential-scoped
   classifier (missing, invalid, exhausted quota, account rate limit), it
   rotates non-stickily to the next configured key with the same
   transient-retry budget. The TS adapters
   (`backend/daemon-ts/src/llm/providers/*.ts`) resolve a single env var per
   call and propagate failures unchanged. Anyone running multiple keys per
   provider loses redundancy.

### Major behavioral divergences (degraded relative to Rust)

6. **Smart image resize + disk cache not ported.**
   `backend/daemon/src/handler/resize.rs` (658 lines) does alpha detection
   (transparent PNG stays PNG, opaque → JPEG), dimension floors at 2048px, an
   XDG disk cache keyed on SHA-256, and async pre-warm via `spawn_blocking`
   wired through `warm_image_cache` (`handler/task.rs:235`). TS
   `backend/daemon-ts/src/llm/images.ts` (82 lines) does inline base64
   conversion and a size cap — no resize, no format conversion, no caching.
7. **Push notifications not ported.** `backend/daemon/src/notifications.rs`
   (252 lines) ships notify-send / ntfy / custom shell backends gated on a
   per-event toggle (autonomous message, cache warning, compaction complete,
   error, message complete, usage warning). Threaded through the autonomy
   manager, the handler, and the compaction task. TS has no equivalent.
8. **Provider auto-discovery loop not ported.** Rust spawns a background task
   on boot (`auto_discovery.rs`, 227 lines) that refreshes every enabled
   provider's model catalog cache on a 24h cadence. TS catalog has a one-shot
   `openrouter.ai/api/v1/models` fetcher (`llm/catalog.ts`) with no scheduler.
9. **Config hot reload not ported.** Rust watches the config dir and reloads
   on supported file changes (`hot_reload.rs`, 264 lines). TS reads config
   once at startup; changing TOML requires a restart.
10. **Prompt template upgrade manifest not ported.** Rust tracks SHA-256 of
    stock templates in `templates.rs` (452 lines) so the daemon auto-upgrades
    stock prompts without clobbering user edits in
    `$XDG_CONFIG_HOME/shore/prompts/`. TS inlines templates as TS string
    literals — there is no on-disk template override path at all, which is a
    design regression, not just a port gap.
11. **Diagnostics ring buffer absent.** Rust threads
    `Arc<Mutex<Diagnostics>>` through every `CommandContext` for the
    `diagnostics` command and key-fallback observability records. TS has
    none, which is partly why `diagnostics` is in the missing-commands list
    above.
12. **Cached-prefix compaction LLM path is not threaded through.** The
    `CompactionLlm.summarize(_, _, cachedRequest)` interface accepts a
    cached request, but `runCompaction` never reads it from
    `autonomy.cachedLastRequest(name)`. Even when compaction is wired
    (blocker #1), it will start cold every time — the byte-for-byte cache
    prefix reuse that Rust's `cached_compaction_request_matches_chat_prefix_byte_for_byte`
    test pins won't fire.

### Engine API surface

13. **TS `ConversationEngine` is missing ~17 methods that Rust exposes.** The
    TS class (`backend/daemon-ts/src/engine/engine.ts`) implements `name`,
    `dataDir`, `historySnapshot`, `appendMessage`, and `rewindLastAssistantTurn`.
    Rust additionally exposes `messages`, `message_count`, `turn_count`,
    `segments`, `display_history`, `current_revision`,
    `history_rewrite_generation`, `insert_message_by_timestamp`,
    `edit_message`, `delete_message`, `truncate_after_last_user_turn`,
    `messages_through_last_user_turn`, `pending_regen_alt`,
    `replace_after_last_user_turn`, `set_alt`, `add_alt_candidate`,
    `select_alt`, `reset`, `reload`, `broadcast_history`, `history_snapshot`
    (with config). The conversation commands (edit/delete/alt/get/log)
    cannot be wired without these.

### Autonomy manager surface

14. **`AutonomyManager` orchestration is mostly absent.** Rust's
    `backend/daemon/src/autonomy/manager.rs` is 3582 lines. The TS
    `AutonomyRegistry` (`src/autonomy/registry.ts`) is ~500 lines and ports
    the activity tracker + heartbeat clock + cache-keepalive mirror only.
    Rust additionally owns:
    - `set_resources(llm_client, push_tx, loaded_config, notifier)` — the
      dependency injection that lets the manager call into LLM, broadcast
      to clients, and emit notifications. TS manager has no such handle.
    - `reload_runtime_config(...)` — pumps in a fresh `LoadedConfig` after
      `config_reset`; the autonomy + compaction subconfigs swap atomically.
    - `notify_assistant_message` (TS only has `notifyUserMessage`).
    - `notify_compaction_complete` / `notify_compaction_failed` — invalidate
      cached request, reset trigger flags, log to heartbeat log, fire
      notification.
    - `should_compact_now(character, turn_count, context_tokens)` —
      max_turns + max_context_tokens + idle pending gate.
    - `heartbeat_tick_now` / `heartbeat_set_dormant` / `heartbeat_set_active`
      / `set_paused` — debug commands.
    - `status(character)` — the snapshot that powers the `status` command.
    - `heartbeat_log(character, limit)` — the snapshot for `heartbeat_log`.
    - `shutdown` — graceful drain of all tick tasks.
    - The per-character tick loop body
      (`character_tick_loop` → `tick_character`, ~250 lines): drives
      compaction triggers (max_turns + idle), dreaming cron with
      exponential backoff (`next_dream_attempt_at` / `dream_failure_count`),
      dormant ping execution, post-action state save. None of this is in TS.

### Catalog completeness (unverified gap)

15. **`llm/catalog.ts` is 326 lines vs `effective_catalog.rs` at 1029 lines.**
    The Rust file merges static catalog + provider registry + discovery cache
    with explicit conflict rules (static wins by short or qualified name,
    static-by-upstream-id wins over discovered for explicit fields).
    Discovery cache refresh, `discovery.ignore`, and provider-key resolution
    paths likely have gaps in TS. Needs a separate function-by-function
    follow-up before cutover.

### Subsystems intentionally or N/A-deferred (not blockers but flag)

16. **`supervisor.rs`** (shore-matrix child process supervisor). N/A unless
    the user is running the Matrix bridge.
17. **`engine/tools.rs` (Rust, 890 lines)** is the in-engine tool dispatcher.
    TS folded the equivalent surface into `tools/registry.ts` + the
    `runToolLoop` orchestrator; behavior parity needs spot-checking but the
    surface isn't a separate module.
18. **`memory/markdown_query.rs`** and **`memory/retrieval.rs`** (267 lines
    combined). Lexical+vector query helpers. TS folded these into
    `memory/workspace_index.ts` and the tool-side dispatcher; spot-check
    that the lexical fallback ranking matches.

### Phase status corrections

Where a phase below claims "done" but the audit found unwired or missing
work, the phase section now carries an **Audit (2026-05-24)** note pointing
at the specific gap above. Phases 4a–4c.1 and the cache-regression goal that
motivated the rewrite remain genuinely done; the gaps cluster in 6b/6d
(modules ported, wiring deferred and never landed) and 8a–8d (autonomy
substrate ported, full state machine not).

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
  - **Audit (2026-05-24): still unwired.** Phase 8 landed the heartbeat +
    keepalive substrate but did not extend `tick_character` to call
    `runCompaction`, and `handler/task.rs::spawn_inline_compaction` has no
    TS counterpart. See parity audit, blocker #1 + #12. The
    `should_compact_now` callable, the `notify_compaction_complete` /
    `notify_compaction_failed` notify hooks, and the cached-prefix LLM path
    are all still missing on the TS side.
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
  - **Audit (2026-05-24): still unwired.** Phase 8 did not land the cron
    integration for dreaming. `runLibrarianSweep` has no production caller in
    TS. Rust drives it from `autonomy/manager.rs:1290` (cron) and
    `commands/state/memory.rs:175` (manual). See parity audit, blocker #2.
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

#### Phase 8a–8d audit note (2026-05-24)

The substrate landed (activity tracker, heartbeat clock, keepalive, async
heartbeat dispatch with system suffix + ledger rows + heartbeat.jsonl).
What did NOT land, despite Phase 6b/6d implying these would be done here:

- The 10s `tick_character` body in TS (`AutonomyRegistry.tickCharacter` /
  `runTick`) drives heartbeat + keepalive only. It does not detect
  compaction triggers (`max_turns`, `max_context_tokens`, `idle_trigger`),
  does not check the dreaming cron window with backoff, and does not
  execute `runCompaction` or `runLibrarianSweep`. See parity audit,
  blockers #1, #2 and major item #14.
- The notification surface (`NotificationService`) is not ported. Heartbeat
  send / dormant / compaction-complete / cache-warning events have no fan-out.
- The `set_resources(llm_client, push_tx, loaded_config, notifier)` shape
  and `reload_runtime_config` are absent — there is no way for `config_reset`
  to swap autonomy/compaction config without restart.
- The autonomy-related commands (`heartbeat_log`, `heartbeat_tick_now`,
  `heartbeat_set_dormant`, `heartbeat_set_active`, `status` insofar as it
  needs `AutonomyStatus`) have no dispatcher entry on the TS side because
  the command dispatcher itself stops at `inject_system_message` and
  `usage`. See parity audit, blocker #3.

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

#### Phase 9a: side-by-side opt-in packaging (done, 2026-05-24)

- [x] `contrib/shore-daemon-ts/PKGBUILD` builds the Bun single-binary
  daemon from `backend/daemon-ts`, runs typecheck/tests/compiled
  smoketest in `check()`, and installs it as `/usr/bin/shore-daemon-ts`
  so it can coexist with the Rust `/usr/bin/shore-daemon`.
- [x] `contrib/shore-daemon-ts.service` provides an opt-in user service
  with the same Shore config/data/runtime paths as the Rust service but
  `ExecStart=shore-daemon-ts`.
- [x] `backend/daemon-ts/README.md` now reflects Phase 8d status and
  documents the preview service.

#### Phase 9b: default + Rust retirement (pending)

- [x] Startup CLI parity: `shore-daemon-ts` now accepts
  `--config <PATH>` like the Rust daemon. The explicit file is loaded
  directly, while `.env`, `conf.d/`, character discovery, and prompt
  files are resolved relative to its parent directory.
- [x] Preview release packaging: `.github/workflows/package.yml` now
  listens for `shore-daemon-ts-v*` tags and publishes
  `contrib/shore-daemon-ts`, so the TS daemon can enter the side-by-side
  release soak through the same repo-arch package path as the Rust
  daemon and CLI.
- [x] CI cutover gate: `.github/workflows/ci.yml` now runs the TS daemon's
  frozen Bun install, typecheck, full test suite, compiled build, and
  compiled smoketest on pushes and PRs alongside the Rust CI.
- [x] Cutover runbook: `docs/DAEMON_TS_CUTOVER.md` defines the preview tag,
  install/start, smoke, soak-evidence, failure-reset, default-switch, rollback,
  and Rust-retirement decision gates for Phase 9b.
- [x] Validation docs: `README.md`, `ARCHITECTURE.md`, and `AGENTS.md` now
  list the TS daemon preview install/typecheck/test/build/compiled-smoke gate
  alongside the existing Rust checks.
- [x] Mainline handoff: `daemon-ts-rewrite` is pushed to GitHub and draft PR
  #36 (`https://github.com/mythofmeat/shore-core/pull/36`) is open against
  `main`, giving the preview tag gate a path to an `origin/main` commit.
- [x] PR verification: #36 is green after the rustfmt fix
  (`ci / check`, `daemon-ts`, and `harness-check` passing).

#### Phase 9b parity gaps (must land before preview soak starts)

The 2026-05-24 audit found that the TS daemon's "ready for preview" state
overstated what is wired. The items below must land — or be explicitly
descoped with rationale — before the preview soak can begin. The previously
scheduled soak/cutover items are blocked on these.

- [x] **Wire autonomous compaction (done, 2026-05-24).** Mirror of
  `handler/task.rs:367` `spawn_inline_compaction` + `tick_character`
  compaction arm landed in TS:
  - `src/autonomy/inline_compaction.ts` — emits `Phase{phase:"compacting"}`,
    pulls `cachedLastRequest`, runs `runCompaction`, reloads engine,
    notifies completion/failure.
  - `AutonomyRegistry` gained `shouldCompactNow`, `notifyCompactionComplete`,
    `notifyCompactionFailed`, `notifyAssistantMessage`, plus per-character
    compaction state (`triggered`, `pending`, `activeTurnCount`,
    `lastActivityMs`). `notifyUserMessage` now takes a message count.
    `tickCharacter` checks the compaction trigger every 10s and either
    fires `onIdleCompaction` inline or sets the `pending` flag for the
    post-generation handler to pick up.
  - `ConversationEngine` gained `reload()` and `messageCount()` (audit
    item #13 — first two of the missing methods).
  - `swp/types.ts` gained `ServerPhase` (`{type:"phase"}`) to match Rust's
    `ServerMessage::Phase`.
  - `LoadedConfig.memory.compaction` is now parsed from `[memory.compaction]`
    (mirrors Rust defaults: 8 min, 16 max, 1800s idle, 200k tokens, 2 keep).
  - 16 new tests in `tests/compaction_wiring.test.ts` cover the
    `shouldCompactNow` decision matrix, the notify-complete/failed state
    mutations, and the `tickCharacter` trigger (with/without
    `onIdleCompaction` wired, idle threshold, min_turns floor).
  - Caveat: `RealCompactionLlm.summarize` still ignores `cachedRequest`
    (the wiring threads it through, but the LLM call falls back to fresh).
    Audit blocker #12 — needs a separate slice to land the cache-preserving
    path.
- [x] **Wire dreaming (done, 2026-05-24).** Mirror of
  `autonomy/manager.rs:1279` `execute_scheduled_dream` landed in TS:
  - `src/autonomy/inline_dreaming.ts` — resolves model + api key, calls
    `runLibrarianSweep`, translates outcome into `notifyDreamingSuccess`
    (clear backoff) / `notifyDreamingFailed` (increment + back off).
  - `AutonomyRegistry` gained per-character dreaming state
    (`nextAttemptAtMs`, `failureCount`, `running`) plus the
    `notifyDreaming*` methods and `dreamingState` accessor. Tick arm
    fires `runScheduledDream` when `autonomy.enabled &&
    dreaming.enabled && !running && now >= nextAttemptAtMs`.
    `backgroundRetryDelayMs` mirrors Rust's `background_retry_delay`
    (60s × 2^n, capped at 1h).
  - `src/memory/dreaming_schedule.ts` — `isDueNow(frequency, lastRunAt)`
    using `croner` (new dep). Mirror of Rust's `is_due`
    (`memory/dreaming.rs:1154`). Cron parsing as a battle-tested library
    is preferable to hand-rolling the 460-line `core/config/src/cron.rs`.
  - `runLibrarianSweep` now gates on `isDueNow` (matching Rust's check at
    `dreaming.rs:258`). `force` and `dryRun` still skip the gate for
    manual `shore memory dream` paths.
  - `AutonomyRegistry` accepts a `dreamingConfig` option (defaults from
    `config.memory.dreaming` already loaded by the loader).
  - 12 new tests in `tests/dreaming_wiring.test.ts` cover `isDueNow`
    (daily, weekly, never-ran, invalid cron, malformed timestamp), the
    backoff state machine, and the tick-arm gating (enabled flags,
    callback wired, backoff window, double-fire prevention).
  - Live `scripts/dreaming-smoketest.ts` — sets a 1-minute cron,
    handshakes the character, waits for the first 10s tick to fire
    dreaming, asserts `DREAMS.md` is appended and `dreams/state.json`
    has `runs >= 1`. Passes against haiku-4.5 via OpenRouter.
  - Manual `memory_dream` / `memory_dreams` command path still pending —
    requires the wider command dispatcher work (audit blocker #3). The
    autonomous path that the audit actually flagged is live.
- [x] **Port the missing command surface (done, 2026-05-25).** The TS
  daemon now has a Rust-shaped command dispatcher under `src/commands/`
  with category handlers for navigation, conversation, state, and
  providers, wired through `main.ts` for all 35 Rust command names. Fully
  backed commands: `status`, `list_characters`, `switch_character`,
  `character_info`, `list_models`, `model_info`, `switch_model`,
  `set_model_setting`, `model_settings`, `reset_model`, `config`,
  `config_check`, `config_reset`, `compact`, `memory`, `memory_dream`,
  `memory_dreams`, `memory_changelog`, `log`, `history_page`, `get`,
  `edit`, `delete`, `alt`, `list_alternatives`, `inject_system`,
  `heartbeat_log`, `heartbeat_tick_now`, `heartbeat_set_dormant`,
  `heartbeat_set_active`, `list_providers`, `list_provider_models`,
  and `usage`. `inject_system_message` remains as a one-release
  compatibility alias for the earlier TS-only command name. Explicit
  stubs: `diagnostics` returns the Rust section shape with
  `diagnostics ring buffer not ported in TS daemon` (audit #11), and
  `refresh_provider_models` / `refresh_all_provider_models` return
  clear `provider model refresh not implemented in TS daemon` payloads
  because the provider discovery refresh path / scheduler remains audit
  #8. Four command test files cover the category payloads and common Rust
  error paths; `bun test` and `bun run typecheck` are green.
- [x] **Port the preferences module (done, 2026-05-25).**
  `backend/daemon/src/preferences/mod.rs` → `src/preferences/` landed:
  - `src/preferences/{types,store,resolve,overlay,index}.ts` mirrors the
    Rust `models.toml` schema for global + per-character preferences,
    flattened per-model sampler entries, strict unknown-field rejection,
    sticky per-model setters, and selection/reset helpers for the command
    dispatcher slice.
  - Resolver parity covers selected model layering, sampler settings +
    sampler scope attribution, static-catalog defaults, discovered-model
    restoration through the provider cache / provider registry fallback,
    chat model resolution, and background-task model resolution.
  - `applySamplerOverlay` is wired into `src/llm/generate.ts` request
    building so saved sampler settings apply before SWP per-call
    `overrides`, preserving Rust's per-call > character > global >
    catalog precedence.
  - 3 new test files (`preferences_store`, `preferences_resolve`,
    `preferences_overlay`) mirror the Rust module tests and cover load/save,
    malformed TOML, resolver precedence, reasoning `"off"`, and request
    overlay edge cases. `bun test` and `bun run typecheck` green.
  - Command handlers that call these setters remain part of the wider
    dispatcher work (audit blocker #3).
- [x] **Extend the `ConversationEngine` API (done, 2026-05-25).**
  ConversationEngine ported — `engine.ts` + `messages.ts` extended,
  17-test suite green. Audit item #13's edit/delete/alt machinery,
  segment accessors, insert-by-timestamp, reload/reset,
  message_count/turn_count, and truncate-after-last-user-turn surface is
  now present for the command dispatcher slice.
- [x] **Multi-key credential fallback descoped (2026-05-25).** Single-key
  is the only configuration anyone actually runs; `handler/key_fallback.rs`
  + the `shore_llm::credentials` classifier are not being ported. TS
  adapters keep their single-env-var resolve-and-propagate behavior.
  Anyone who later wants multi-key redundancy gets it as a post-cutover
  add-on, not a cutover blocker. (Audit blocker #5.)
- **Major divergences — decisions (2026-05-25):**
  - **#6 smart image resize.** Descoped from cutover. Tracked in GH #40.
    Bun has no `image`/`fast_image_resize` equivalent without a native
    dep (`sharp`) that complicates `bun build --compile`. To remove the
    silent-drop correctness issue, oversized-image handling needs to
    error visibly rather than `console.warn`-skip — pending follow-up.
  - [x] **#7 push notifications (done, 2026-05-25).** Re-scoped: notify-send
    backend only; ntfy and custom-command backends dropped. The Rust CLI's
    `shore notify` listener stays (it's in `clients/cli`, which the rewrite
    doesn't touch).
    - `src/notifications/types.ts` ports `NotificationsConfig` with the
      reduced surface (`enabled`, `generation_threshold_ms`, `events.{autonomous_message,compaction_complete,error,message_complete,usage_warning}`).
      `cache_warning` is omitted because Rust defined the enum variant but
      no call site ever fired it.
    - `src/notifications/service.ts` ports `NotificationService` — fire-and-
      forget `Bun.spawn(["notify-send", "--app-name=shore", ...])`, no shell.
      Bodies truncated to 200 chars. Defaults match Rust
      (`message_complete=false`, everything else true).
    - Config loader exposes `app.notifications` from `[notifications]` +
      `[notifications.events]`; `generation_threshold` accepts the same
      duration strings as the rest of the loader.
    - Fan-out sites match Rust:
      - `message_complete` after every user/regen turn (`main.ts`
        `buildMessageHandler` + `buildRegenHandler`), threshold-gated.
      - `error` from `handleGenerationError` and inline-compaction failures.
      - `usage_warning` from `emitBudgetWarnings` for each newly crossed
        budget.
      - `compaction_complete` from `inline_compaction.ts` on success
        (entries-from-turns body matches Rust).
      - `autonomous_message` from `autonomy/dispatch.ts::persistAutonomousMessage`.
    - Tests: `tests/notifications.test.ts` (7 cases — master switch,
      per-event toggles, truncation, threshold gate, reload),
      plus 2 new cases in `tests/config_loader.test.ts` for the TOML
      surface (defaults + overrides).
    - `bun test`: 501 pass / 7 skip / 0 fail. `bun run typecheck` green.
  - [x] **#8 provider auto-discovery refresh.** Manual refresh ported
    (2026-05-25). `refresh_provider_models` and
    `refresh_all_provider_models` now hit the configured discovery
    endpoint via `src/llm/discovery.ts` (Anthropic native + OpenAI
    `/v1/models`) and write `ProviderModelsCache` atomically. The 24h
    auto-discovery scheduler (`auto_discovery.rs`) is intentionally not
    ported; users refresh on demand.
  - **#9 config hot reload.** Descoped. Restart-required is a UX
    downgrade, not a correctness regression. Post-cutover follow-up.
  - **#10 prompt template upgrade manifest.** Descoped (2026-05-25). The
    only active user doesn't override stock prompts — workspace
    customization happens through AGENTS/SOUL/USER/TOOL files in the
    character workspace, which the TS daemon already reads end-to-end.
    Bundled prompts stay as TS string literals (`parser.ts` etc.); no
    on-disk override path. If override demand surfaces later, port
    `templates.rs` (~450 lines) + write the bundled defaults to
    `$XDG_CONFIG_HOME/shore/prompts/` on first run.
  - **#11 diagnostics ring buffer.** Descoped. Same observability is
    available via the ledger (`shore usage`); the dispatcher already
    returns "diagnostics ring buffer not ported in TS daemon" with the
    correct section shape.
- [ ] **Catalog parity follow-up.** Walk
  `effective_catalog.rs` (1029 lines) against `llm/catalog.ts` (326 lines)
  function-by-function. Document what's actually missing and either port or
  descope. (Audit item #15.)
- [x] **Autonomy manager orchestration (done, 2026-05-25).**
  AutonomyRegistry orchestration surface added — `setResources`,
  `reloadRuntimeConfig`, `notifyAssistantMessage`, heartbeat debug
  methods, `setPaused`, `status`, `heartbeatLog`, `shutdown`.
  `notifyCompactionComplete` / `_Failed` and `shouldCompactNow` were
  spot-checked against Rust and remain the already-landed parity surface.

#### Phase 9b soak + cutover (blocked on parity gaps above)

- [ ] **Automated parity coverage lands.** The existing harness only
  covers handshake, one message-append, and offline prompt-assembly. The
  cutover gate ("one full release cycle with no live failures") is a
  user-observation parity test; this work makes it a defense-in-depth
  observation, not the only signal. Tracked in
  `docs/DAEMON_TS_PARITY.md` (T1 persistence flows, T2 command
  dispatcher round-trips, T3 content-level parity via LLM proxy stub).
- [ ] Preview soak starts: merge the rewrite branch to `origin/main`, publish
  a `shore-daemon-ts-v*` tag from that main commit, verify the repo-arch
  package, install/run `shore-daemon-ts.service`, and record the start
  evidence from `docs/DAEMON_TS_CUTOVER.md`.
- [ ] Preview soak completes: one full release cycle of opt-in TS daemon
  traffic finishes with no live failures attributable to the TS daemon. Any
  code fix for a live TS-daemon failure restarts the soak clock from the fixed
  preview release.
- [ ] Default switch lands: `shore-daemon-ts` becomes the default daemon
  package/service path, with migration and rollback notes captured in the
  cutover PR.
- [ ] Rust daemon retired: `backend/daemon` is moved to `attic/` or deleted by
  the cutover decision.
- **Exit criterion:** preview soak complete, TS daemon is the default, and the
  Rust daemon is retired.

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
