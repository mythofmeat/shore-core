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
> also green for Anthropic and OpenAI-compatible text generation,
> Anthropic regen persistence, a one-tool Anthropic loop, inline
> compaction end-to-end (trigger → memory writes → segment archive →
> active.jsonl truncation → restart history), autonomous heartbeat
> message dispatch, the manual `memory_dream` command path, and
> scheduled dreaming through the autonomy tick. These
> compare SWP output,
> canonical provider request bodies, and the post-restart on-disk state
> where relevant.

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
The first T3 content checks are separate for now:
`bun run parity:generation` for Anthropic,
`bun run parity:generation:openai` for OpenAI-compatible,
`bun run parity:regen` for Anthropic regen,
`bun run parity:tool-loop` for the one-tool Anthropic loop,
`bun run parity:compaction` for inline compaction end-to-end,
`bun run parity:heartbeat-tick` for autonomous heartbeat dispatch,
`bun run parity:dreaming` for the manual memory-dream command path, and
`bun run parity:scheduled-dreaming` for the scheduled cron/tick path (all
require `/usr/bin/shore-daemon`).

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

Deferred to Tier 3: `compact`, `refresh_provider_models`,
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
tool-loop check, `backend/daemon-ts/scripts/parity-check-tool-loop.ts`,
queues a `tool_use` response followed by a final text response, then diffs
the intermediate tool frames, both provider request bodies, and persisted
history. The inline-compaction check,
`backend/daemon-ts/scripts/parity-check-inline-compaction.ts`, seeds two
user/assistant turns into `active.jsonl`, sends a third turn that crosses
`max_turns=3`, waits for the post-stream `phase{compacting}` and the
`segments/0001.jsonl` archive to land, then diffs the chat-call request
body, the compaction-truncated `active.jsonl`, the archived segment, the
written memory files (`memory/people/parity-user.md` + `MEMORY.md`), the
`compaction.json` manifest, and the post-restart history. The
compaction-call request body is captured to
`/tmp/parity-compaction-{rust,ts}-req2.json` but **not** asserted on
here — that's the [audit #12 cache-prefix regression pin](#tier-3--content-level-parity-requires-llm-stub),
tracked separately. The generation check currently has Anthropic and
OpenAI-compatible fixtures. The LLM proxy serves SSE for streaming
requests and a single JSON message for non-streaming requests (compaction
and heartbeat calls go through the latter path). The heartbeat check,
`backend/daemon-ts/scripts/parity-check-heartbeat-tick.ts`, sends one
deterministic setup turn to create autonomy state, forces
`heartbeat_tick_now`, waits for an autonomous `new_message`, then diffs
setup and heartbeat request bodies, tick SWP frames, `active.jsonl`,
restart history, and notify-send argv. The dreaming check,
`backend/daemon-ts/scripts/parity-check-dreaming.ts`, sends
`memory_dream force=true`, then diffs the command output, librarian
request body, `dreams/state.json`, `DREAMS.md`, and fallback
`MEMORY.md`. The scheduled dreaming check,
`backend/daemon-ts/scripts/parity-check-scheduled-dreaming.ts`, seeds a
future dream state to absorb the first autonomy tick, sends one setup
turn to cache the completed chat request, deletes the dream state to make
the cron due, then waits for the next tick and diffs the cached-prefix
librarian request, the same dream artifacts, and the `dreaming` ledger
row.

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
- [x] **inline compaction trigger end-to-end (done 2026-05-25)** —
  seeded `active.jsonl` + a third user turn crosses `max_turns` → diff
  post-`stream_end` `phase{compacting}`, chat request body, retained
  `active.jsonl` (fuzzy `msg_id`/`timestamp`), archived
  `segments/0001.jsonl`, written memory files, `compaction.json`
  (fuzzy `compacted_at`), and post-restart history. The
  compaction-call request body is captured to
  `/tmp/parity-compaction-{rust,ts}-req2.json` but not asserted on —
  the **inline compaction LLM body** item below is the assertion lift
  for that. `bun run parity:compaction`. Surfaced and fixed a TS-side
  bug where the prompt-assembly time marker was leaking into the
  persisted user message (`engine/prompt.ts` shallow `content_blocks.slice()`
  vs deep block copy).
- [x] **manual memory-dream command (done 2026-05-25)** —
  `memory_dream force=true` runs one non-streaming librarian call,
  exercises the no-tool fallback MEMORY.md path, and diffs the SWP
  output, request body, dreams state, dreams log, and fallback memory
  index. `bun run parity:dreaming`.
- [x] **scheduled dreaming cron firing end-to-end (done 2026-05-26)** —
  setup turn creates autonomy state + cached request, the fixture's
  future dream state suppresses the first tick, then the check makes the
  cron due and waits for the autonomy tick to run the scheduled
  librarian pass. Diffs the cached-prefix request body, dreams state,
  dreams log, fallback memory index, and `dreaming` ledger row.
  `bun run parity:scheduled-dreaming`.
- [x] **autonomous-message dispatch (done 2026-05-25)** — setup turn
  creates autonomy state → `heartbeat_tick_now` fast-forwards the clock
  → diff autonomous SWP frames, setup + heartbeat request bodies,
  `active.jsonl`, restart history, and notify-send spawn.
- [x] **tool loop multi-turn (Anthropic, done 2026-05-25)** — message
  that triggers a `read` tool call → diff tool-call/tool-result frames,
  both provider request bodies, final assistant text, and post-restart
  history. `bun run parity:tool-loop`.
- [x] **notification fan-out (done 2026-05-25)** — `notify-send`
  shim installed via PATH override in `buildDaemonEnv` (helper
  `installNotifySendShim` in `scripts/parity/_lib.ts`). Each daemon's
  `notify-send` invocation is captured as a JSON line in a shared log
  file; the inline-compaction check compares both daemons' captured
  argv arrays. Compaction emits exactly one notification per daemon
  with identical `--app-name=shore <title> <body>` content. Piggybacks
  on the inline-compaction fixture (notifications enabled via
  `[notifications]` block).
- [x] **inline compaction LLM body / cached prefix (audit #12, done
  2026-05-25)** — TS `RealCompactionLlm` now reuses the cached chat
  request prefix (system + tools + messages) and appends compaction
  tail via the non-streaming generate() path. Bodies match Rust
  structurally; one known wire-form divergence (trailing-user
  content string-vs-array) is parked in "Known divergences" along
  with the breakpoint-placement gate.

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

## Known divergences (must-resolve gates)

These are real wire-level divergences uncovered by the parity harness
that are *not* parity-test bugs. Each blocks preview soak.

- **Cache-breakpoint placement (chat call, `cache_ttl="1h"`).**
  Surfaced by `parity-check-inline-compaction.ts` after flipping the
  fixture from `cache_ttl=""` to `"1h"` on 2026-05-25.
  - Rust marks **system block 1** (`tools_guidance`) and the
    **second-to-last stable user message** as cache breakpoints.
  - TS marks **system block 2** (`character`) and the
    **second-to-last stable assistant message** as cache breakpoints.
  - Same conversation → different cache-key hashes → no cross-daemon
    cache reuse. Worse, the *correct* placement strategy isn't
    obvious from offline reasoning alone — Anthropic charges
    differently for cache_creation vs cache_read at each position,
    and the optimal breakpoint depends on actual cache_read accounting
    on real traffic. **Resolution requires live API runs** comparing
    `cache_creation_input_tokens` / `cache_read_input_tokens` deltas
    across the two strategies on a multi-turn conversation; pick the
    winner, bring both daemons into agreement, then drop this entry.
  - Until then: existing TS users do not share cache with existing
    Rust users (no regression vs status quo); both daemons cache
    *within themselves*; the cost penalty is one cold-cache pass per
    daemon-switch (= the cutover event).
  - **Blocks preview soak.**

- **Compaction trailer content form.** After implementing the
  cached-prefix path (audit #12, 2026-05-25), TS compaction request
  bodies match Rust's structurally — same system/tools/messages
  prefix, same trailing-user content text, same wrapped
  `<system_instruction>` payload. The only remaining diff is the
  **content form of the trailing user message**: Rust emits
  `content: "..."` (string), TS emits
  `content: [{"type":"text","text":"..."}]` (single-element array).
  Both are valid Anthropic Messages API forms, but different bytes →
  different cache-key hashes → no cross-daemon cache reuse for the
  *compaction-written* prefix. Within each daemon: chat-call prefix
  matches compaction-call prefix (both daemons use array-content for
  the historical messages), so cache reads from the chat write
  succeed.
  - Root cause: Rust's `compaction_impls.rs` builds the trailing
    user as `json!({"role":"user", "content": <string>})` directly,
    bypassing the structured-block path. TS goes through
    `TurnMessage` which mandates `content: ContentBlock[]`. A
    string-content path would also change *chat-call* wire shape
    (which currently does byte-match Rust at array form), so this
    can't be unified without an Anthropic-adapter special case for
    single-text-block content.
  - Same category as the breakpoint-placement divergence: blocks
    cross-daemon cache reuse, not within-daemon caching. Resolution
    bundled with the breakpoint live-API gate.

- **`_label` wire leak (fixed 2026-05-25).** TS was copying
  `SystemPromptBlock._label` onto Anthropic request bodies. Anthropic
  silently ignores unknown fields but they pollute the cache-key hash.
  Rust strips at `backend/llm/src/providers/anthropic.rs:301-306`; TS
  port at `backend/daemon-ts/src/llm/providers/anthropic.ts:308` was
  doing the opposite. Fixed and pinned by
  `_label_never_reaches_wire` in `tests/cache_placement.test.ts`.
  Surfaced only because the inline-compaction fixture was switched to
  `cache_ttl="1h"` — every other T3 fixture had caching disabled and
  never exercised the wire-build path.

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
  until we need provider-captured fixtures. The proxy serves SSE when
  the request body has `stream: true` and a single JSON message
  otherwise — added 2026-05-25 for inline-compaction parity, since
  `LedgerClient::generate` (compaction's call path) is non-streaming.
- [x] **T3 notify-send intercept (done 2026-05-25).** Shim that both
  daemons can shell out to instead of the real `notify-send`, logs the
  argv to JSONL, and lets T3 checks diff notification title/body content.
  Used by inline compaction and autonomous heartbeat dispatch.
- [ ] **Live-API validation pass (pre-soak gate).** Every T3 check
  today runs against `parity/llm-proxy.ts` serving canned responses.
  That proves *daemon-vs-daemon* parity against the mock; it does not
  prove the mock is faithful to the real provider. Since the rewrite's
  motivating bug (cache prefix regression) was a mock-vs-real
  divergence, mock-only confidence is insufficient before preview
  soak. Procedure once all T3 checks land:

  1. Point each T3 fixture's `base_url` at the real provider
     (`api.anthropic.com`, `api.openai.com`, OpenRouter when ported).
  2. Run the check in forward-record mode (`startParityLlmProxy({
     ..., recordMissing: true, fixtureDir })`) — the proxy still
     captures canonical request bodies, lets the real response stream
     through to the daemon, and writes the response to a content-
     addressed fixture under `parity-traces/llm-fixtures/recorded/`.
  3. The pass succeeds if both daemons agree on the *outgoing*
     canonical request body for every LLM call. The recorded provider
     responses are non-deterministic in text, but parity is a property
     of what each daemon *sends*, not what it gets back.
  4. The recorded fixture is then committed and used by the mock-mode
     CI run going forward, replacing the hand-authored canned response.
     If a future provider-side change drifts the mock from reality, a
     re-record pass catches it.

  Each check pays one round-trip per fixture in real API cost (~cents);
  manual gate, not CI.

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
