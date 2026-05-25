# shore-daemon-ts Parity Coverage

Tracks automated parity coverage between the Rust daemon (`backend/daemon`)
and the TS daemon (`backend/daemon-ts`). The Phase 9b cutover gate in
REWRITE.md is "one full release cycle of opt-in TS daemon traffic with no
live failures attributable to the TS daemon." That's a user-observation
gate; this document tracks the automated coverage that closes the gap so
soak is for catching the *unexpected* divergence, not the expected one.

> **Status (2026-05-25).** Coverage is currently three narrow slices:
> handshake (empty + character), one message-append round-trip, and an
> offline `AssembledPrompt` JSON diff across 10 prompt fixtures. Everything
> else in this document is target state.

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

## Coverage tiers

Tiers reflect infrastructure cost, not importance. T1 + T2 extend the
existing harness; T3 needs new infra (deterministic LLM stub); T4 is
explicitly out of scope for automation.

### Tier 1 — persistence-based flows (no new infra)

Same pattern as `parity-check-message-append.ts`: drive the daemon with a
scripted client, kill it, restart, diff the persisted state.

- [ ] **regen** — append user msg → regen → kill → restart → diff history
- [ ] **edit** — append → edit-message → kill → restart → diff
- [ ] **delete** — append → delete-message → kill → restart → diff
- [ ] **alt-cycle** — append → add_alt → select_alt → kill → restart → diff
- [ ] **multi-turn dialog** — N user msgs → kill → restart → diff full history
- [ ] **inline compaction trigger** — append until trigger → wait for
  compaction → kill → restart → diff `active.jsonl` truncation + memory
  files written
- [ ] **truncate-after-last-user-turn** — pin the engine API gap closed by
  audit #13

Capture scripts live next to `capture-message-append.ts`; baselines under
`backend/daemon-ts/parity-traces/`; check scripts next to
`parity-check-message-append.ts`. One capture + check pair per flow.

### Tier 2 — command dispatcher round-trips (no LLM involved)

Pure command/response. One fixture per dispatcher command: handshake →
send command frame → read response frame → diff. No daemon kill required.

The complete command surface lives in
`backend/daemon-ts/src/commands/dispatch.ts`. Coverage tracks against the
Rust dispatcher in `backend/daemon/src/commands/`.

- [ ] State commands: `get_state`, `set_state`, `model_settings`,
  `set_model_setting`, `reset_model`, `set_model`, `list_models`,
  `find_model`
- [ ] Provider commands: `list_provider_models`, `provider_cache_summary`,
  `refresh_provider_models`, `refresh_all_provider_models`
- [ ] Memory commands: the full memory-state surface in
  `commands/state/memory.rs`
- [ ] Status commands: `status`, `heartbeat_log`, `activity_heatmap`
- [ ] Preferences commands: get / set / reset
- [ ] Autonomy debug commands: `heartbeat_tick_now`,
  `heartbeat_set_dormant`, `heartbeat_set_active`, `set_paused`
- [ ] Usage commands: `shore usage` backing surface
- [ ] Misc: `diagnostics` (TS returns the explicit not-ported shape; pin
  that), `set_next_wake`, plus anything else listed in
  `dispatch.ts`

Each command goes in `backend/daemon-ts/parity-traces/commands/<name>.jsonl`
with a shared `parity-check-command.ts` runner that iterates the directory.
Use `EXPECTED_DIFFS` for known-divergent fields (timestamps, request ids).

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

Once that infra exists:

- [ ] **generation content parity** — send msg → diff the assistant text
  frame body (not just persistence)
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
- [ ] **T2 runner.** `parity-check-command.ts` that iterates
  `parity-traces/commands/*.jsonl` so adding a new command is one fixture
  file, no new script.
- [ ] **T3 LLM proxy.** Design above. Bun's built-in HTTP server + a
  content-addressable fixture store under `parity-traces/llm-fixtures/`.
  Record/replay flag. Streaming response preservation.
- [ ] **T3 notify-send intercept.** Shim that both daemons can shell out
  to instead of the real `notify-send`, logs the (title, body) args, both
  daemons under test write to the same log file → diff. Cheaper than
  intercepting `Bun.spawn` directly.

## How to add a new parity case

1. Pick the tier. T1 if it's state-touching, T2 if it's a dispatcher
   command, T3 if it requires an LLM response.
2. Capture against the Rust daemon: write or extend a capture script,
   produce a `.jsonl` baseline under `parity-traces/`. For T2, just hand-
   author the request/response fixture if simpler than scripted capture.
3. Wire the check: for T1, follow the kill+restart+diff pattern; for T2,
   the shared runner picks up new fixtures automatically; for T3, ensure
   the LLM proxy has the response recorded.
4. Add `EXPECTED_DIFFS` entries only for *structurally* expected
   divergences (server name, request ids, timestamps). Anything content-
   bearing that diverges is a parity bug, not an expected diff.
5. Add the script invocation to the `parity` package script so CI runs it.

## CI integration

The `bun run parity` script runs in CI via
`.github/workflows/ci.yml`'s daemon-ts job. Each new check goes in that
chain. The prompt-assembly check (`parity:prompt`) requires the Rust
example binary and so currently runs locally only; if we move that into
CI, the workflow needs `cargo build -p shore-daemon --example
dump_assemble_prompt` as a prerequisite step.
