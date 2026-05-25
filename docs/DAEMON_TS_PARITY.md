# shore-daemon-ts Parity Coverage

Tracks automated parity coverage between the Rust daemon (`backend/daemon`)
and the TS daemon (`backend/daemon-ts`). The Phase 9b cutover gate in
REWRITE.md is "one full release cycle of opt-in TS daemon traffic with no
live failures attributable to the TS daemon." That's a user-observation
gate; this document tracks the automated coverage that closes the gap so
soak is for catching the *unexpected* divergence, not the expected one.

> **Status (2026-05-25).** Tier 1 persistence coverage is green for
> handshake, message append, multi-turn, edit, delete, and alt. Tier 2
> command-dispatcher coverage is green for the manifest-backed batch under
> `backend/daemon-ts/parity-traces/commands/`. The first Tier 3 slice is
> also green for Anthropic and OpenAI-compatible text generation, plus
> Anthropic regen persistence: these compare SWP output, canonical
> provider request bodies, and the post-restart history where relevant.

## Existing harness recap

- `backend/daemon-ts/scripts/parity-check.ts` — replays a captured client
  trace against the TS daemon, diffs s2c frames against the Rust baseline.
  Uses `EXPECTED_DIFFS` (per frame type, dotted-path allowlist) for known
  divergences like `hello.server_name`.
- `backend/daemon-ts/scripts/parity-check-message-append.ts` — extends the
  same pattern for the persistence assertion: capture writes, kill the
  daemon, restart, diff the History snapshot. The capture script
  (`capture-message-append.ts`) deliberately disables the LLM (no provider
  keys) so the trace is deterministic — we don't try to diff the assistant
  turn, we diff the post-restart on-disk state.
- `backend/daemon-ts/scripts/parity-check-prompt.ts` — offline:
  `AssembledPrompt` JSON deep-diff across `tests/fixtures/prompt/*.json`.
  Both daemons spawn a dump binary with `TZ=America/Los_Angeles` for
  reproducibility.

The `bun run parity` package script chains the three live checks; the
prompt-assembly check has its own `bun run parity:prompt` (requires
`cargo build -p shore-daemon --example dump_assemble_prompt`).
The first T3 content check is separate for now:
`bun run parity:generation` for Anthropic,
`bun run parity:generation:openai` for OpenAI-compatible, and
`bun run parity:regen` for Anthropic regen (all require
`/usr/bin/shore-daemon`).

## Coverage tiers

Tiers reflect infrastructure cost, not importance. T1 + T2 extend the
existing harness; T3 needs new infra (deterministic LLM stub); T4 is
explicitly out of scope for automation.

### Tier 1 — persistence-based flows (no new infra)

Same pattern as `parity-check-message-append.ts`: drive the daemon with a
scripted client, kill it, restart, diff the persisted state. Eligible
flows are the ones whose state mutation happens **before** any LLM call
— without a working provider key (the capture/check default), any
post-LLM state change never happens.

- [x] **multi-turn dialog (done 2026-05-25).** 3 user messages →
  restart → all persisted. `capture-multi-turn.ts` /
  `parity-check-multi-turn.ts`.
- [x] **edit command (done 2026-05-25).** Edit seeded msg →
  restart → edited content persists. `capture-edit.ts` /
  `parity-check-edit.ts`. Two-frame command response (history broadcast
  + command_output) is sorted by type to absorb racy emission order
  between event_tx and direct_tx; same pattern reused by delete + alt.
- [x] **delete command (done 2026-05-25).** Delete seeded msg →
  restart → message gone. `capture-delete.ts` / `parity-check-delete.ts`.
- [x] **alt command (done 2026-05-25).** Switch selected alternative on
  pre-seeded multi-alt message → restart → new alt is active +
  `alt_index` updated + alternatives preserved.
  `parity-traces/fixtures/alt-cycle/` carries the seeded m2 with two
  alternatives. `capture-alt.ts` / `parity-check-alt.ts`. Note: the
  Rust `alt` command is select-only; adding alternatives requires
  successful regen, which is covered under Tier 3.

Capture scripts live next to `capture-message-append.ts`; baselines under
`backend/daemon-ts/parity-traces/`; check scripts next to
`parity-check-message-append.ts`. One capture + check pair per flow.

**Moved out of Tier 1** (require working LLM, covered under Tier 3 below):
- **regen** — state mutation only happens on LLM success
  (`handler/persistence.rs:138` `replace_after_last_user_turn`). With a
  failed LLM, history is unchanged, so persistence diff is trivial.
- **inline compaction trigger** — compaction is itself an LLM call.
- **truncate-after-last-user-turn** — not SWP-triggerable; internal
  engine API. Covered by unit tests, not parity.

### Tier 2 — command dispatcher round-trips (no LLM involved)

Done 2026-05-25. The shared runner is manifest-backed:

- `backend/daemon-ts/parity-traces/commands/manifest.json` names each
  case, fixture, expected post-command frame count, baseline, fuzzy paths,
  and expected outcome.
- `backend/daemon-ts/scripts/capture-command.ts` captures Rust baselines
  from the manifest.
- `backend/daemon-ts/scripts/parity-check-commands.ts` replays the
  manifest against TS. Each case gets a fresh fixture copy and daemon.

Captured command cases:

- [x] Navigation: `list_characters`, `character_info`,
  `switch_character`, `switch_character` missing-target error.
- [x] Conversation: `log`, `history_page`, `get`, `get` missing-arg
  error, `list_alternatives`, `inject_system`.
- [x] State/read: `status`, `memory`, `memory_changelog`,
  `memory_dreams`, `config`, `config_check`, `config_reset`,
  `diagnostics`, `usage`.
- [x] Heartbeat debug: `heartbeat_log`, `heartbeat_tick_now`,
  `heartbeat_set_dormant`, `heartbeat_set_active`. The three mutators
  currently pin Rust's minimal-fixture error shape when no autonomy state
  exists.
- [x] Models/providers: `list_models`, `model_info`, `model_settings`,
  `set_model_setting`, `set_model_setting` invalid-key error,
  `switch_model`, `switch_model` missing-model error, `reset_model`,
  `list_providers`, `list_provider_models`,
  `list_provider_models` missing-provider error.

Already covered by Tier 1: `edit`, `delete`, `alt`.

Deferred to Tier 3: `compact`, `memory_dream`, `refresh_provider_models`,
`refresh_all_provider_models`. `inject_system_message` is a TS-only alias
for `inject_system`; Rust has no equivalent command name, so parity does
not cover it.

### Tier 3 — content-level parity (requires LLM stub)

Real generation, dreaming, autonomy-driven LLM calls. Needs a
deterministic LLM stub so two daemons see the same response for the same
request.

**Design: HTTP intercept proxy.** A tiny in-process HTTP server that:
1. Hashes the inbound request (method + URL + canonicalized body) into a
   fixture key.
2. If a fixture exists for the key, serves the canned response (streaming
   format preserved).
3. If no fixture exists, either (a) records mode: forwards to the real
   provider and saves the response keyed by hash, or (b) replay mode:
   fails the test with the request body dumped so a human can record it.

Both daemons get `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` /
`OPENROUTER_BASE_URL` (etc.) pointed at the proxy via env, so neither
daemon needs to know it's being mocked. The proxy lives in
`backend/daemon-ts/scripts/parity/llm-proxy.ts` and the fixtures in
`backend/daemon-ts/parity-traces/llm-fixtures/`.

Initial implementation lives in
`backend/daemon-ts/scripts/parity/llm-proxy.ts`. The first check,
`backend/daemon-ts/scripts/parity-check-generation.ts`, runs Rust and TS
against the same canned provider SSE stream, then diffs both the SWP
generation summary and the canonical provider request body. The regen
check, `backend/daemon-ts/scripts/parity-check-regen.ts`, uses the same
proxy with a queued response pair so the initial message receives response
A and regen receives response B before the restart-history diff. The
generation check currently has Anthropic and OpenAI-compatible fixtures.

Once the rest of that infra exists:

- [x] **generation content parity (Anthropic, done 2026-05-25)** — send
  msg → diff assistant text/tokens/finish reason and canonical Anthropic
  provider request body. `bun run parity:generation`.
- [x] **generation content parity (OpenAI-compatible, done 2026-05-25)**
  — same as above for `/chat/completions` SSE.
  `bun run parity:generation:openai`.
- [x] **regen (Anthropic, done 2026-05-25)** — send msg (deterministic
  response A) → regen (deterministic response B) → kill → restart → diff
  history, including `alt_index` / `alt_count` and alternatives.
  `bun run parity:regen`.
- [ ] **inline compaction trigger end-to-end** — append until trigger
  threshold → wait for compaction → kill → restart → diff `active.jsonl`
  truncation + memory files written + ledger rows. Requires the LLM stub
  for the compaction LLM call.
- [ ] **dreaming cron firing end-to-end** — trigger via debug command → wait
  → diff memory files written + ledger rows
- [ ] **autonomous-message dispatch** — fast-forward heartbeat (debug cmd)
  → wait for autonomous turn → diff history + notification spawn
- [ ] **tool loop multi-turn** — message that triggers ≥2 tool calls →
  diff tool-call frames + final assistant text
- [ ] **notification fan-out** — intercept `Bun.spawn(["notify-send", ...])`
  and the Rust equivalent; diff the (event, title, body) tuples emitted
  for the same scenario
- [ ] **inline compaction LLM body** — pin that the compaction LLM call
  reuses the cached prefix (audit #12 regression pin)

### Tier 4 — out of scope for automation

These are either timing-dependent (need fake clock injection), too
provider-specific (model output varies even with seed=0), or already
covered by deterministic unit tests on both sides:

- Cache forensics ring buffer ordering (timing-dependent)
- Heartbeat tick scheduling intervals (needs fake clock injection on both
  sides; not worth the infra for one flow)
- Embedding vector content (provider-determined; verify dimension/count
  only, not values)
- LLM-internal scheduling of streaming chunks (provider variance)

Spot-check during soak; daemon-internal unit tests cover the deterministic
parts.

## Infrastructure work

- [x] **T1 harness consolidation (done 2026-05-25).** Shared helpers
  live in `backend/daemon-ts/scripts/parity/_lib.ts`:
  `spawnDaemon`, `readListenAddr`, `openConnection`, `FrameQueue`,
  `readFrame`, `copyFixtureToTmp`, `buildDaemonEnv`, `compareFrames`,
  `pathToMatcher`, `fail`. All four existing scripts
  (`capture-rust-trace.ts`, `capture-message-append.ts`,
  `parity-check.ts`, `parity-check-message-append.ts`) refactored to
  import from it. `bun run parity` green; per-flow scripts now ~100
  lines each instead of ~280.
- [x] **T2 runner (done 2026-05-25).**
  `backend/daemon-ts/scripts/parity-check-commands.ts` iterates
  `parity-traces/commands/manifest.json`; adding a command case is a
  manifest entry plus one captured baseline, no new runner script.
- [x] **T3 LLM proxy, first slice (done 2026-05-25).**
  `backend/daemon-ts/scripts/parity/llm-proxy.ts` uses Bun's built-in
  HTTP server, preserves Anthropic and OpenAI-compatible SSE streaming,
  captures canonical request bodies, and can use a content-addressed
  fixture directory. Real-provider forward-record mode is still deferred
  until we need provider-captured fixtures.
- [ ] **T3 notify-send intercept.** Shim that both daemons can shell out
  to instead of the real `notify-send`, logs the (title, body) args, both
  daemons under test write to the same log file → diff. Cheaper than
  intercepting `Bun.spawn` directly.

## How to add a new parity case

1. Pick the tier. T1 if it's state-touching, T2 if it's a dispatcher
   command, T3 if it requires an LLM response.
2. For T2, add a case to
   `backend/daemon-ts/parity-traces/commands/manifest.json`. Use
   `expected_frames` for the exact number of post-command s2c frames;
   mutators that broadcast history generally need `2`.
3. Reuse an existing fixture under `parity-traces/fixtures/` or add a
   new named fixture when the command needs different config/data.
4. Capture against Rust:
   `bun scripts/capture-command.ts /usr/bin/shore-daemon --id <case-id>`
   from `backend/daemon-ts/`.
5. Run `bun run parity:commands`; add fuzzy paths only for known
   non-semantic differences such as temp paths, generated ids, and
   timestamps.
6. For T1, follow the kill+restart+diff pattern; for T3, ensure the LLM
   proxy has the response recorded.
7. Add fuzzy entries only for *structurally* expected
   divergences (server name, request ids, timestamps). Anything content-
   bearing that diverges is a parity bug, not an expected diff.
8. Add the script invocation to the `parity` package script so CI runs it.

## CI integration

The `bun run parity` script runs in CI via
`.github/workflows/ci.yml`'s daemon-ts job. Each new check goes in that
chain. The prompt-assembly check (`parity:prompt`) requires the Rust
example binary and so currently runs locally only; if we move that into
CI, the workflow needs `cargo build -p shore-daemon --example
dump_assemble_prompt` as a prerequisite step.
