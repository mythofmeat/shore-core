# Shore buglist

Working notes from the 2026-05-14 compaction-loss investigation. Items are
checkboxes — strike through as you go.

Context: chat moved to the daemon-owned preferences overlay on 2026-04-27
(commit `1af2a82`). Background paths weren't migrated. Three were fixed in
commit `1b4fc03` (2026-05-14); the rest are below.

---

## Already fixed (commit `1b4fc03`)

- [x] `memory/compaction/background.rs` — apply `apply_sampler_overlay` after
      `resolve_compaction_model`. Restores `max_tokens` from preferences (was
      defaulting to 4096, truncating XML responses mid-`<write>`).
- [x] `memory/dreaming.rs` — same overlay fix on the fresh-request path.
- [x] `memory/compaction_impls.rs` — carry `cached.tools` through on the
      cache-preserving path. Anthropic includes `tools` in the cache-prefix
      hash; `tools: None` was forcing a full cache rebuild on every compaction
      (suspected ~40% budget bump since 2026-05-02).

---

## Tier 1 — same overlay bug

Closed by introducing `preferences::resolve_background_model` and
`preferences::resolve_chat_model_for_character` (Tier 3 #2 helper) and routing
every background site through them.

- [x] **Manual `/compact` command** — `commands/state/memory.rs`. Now resolves
      via `preferences::resolve_background_model(BackgroundTask::Compaction)`.
- [x] **Heartbeat cold rebuild** — `autonomy/manager.rs::rebuild_request_from_disk`.
      Now resolves via `preferences::resolve_chat_model_for_character`, which
      also fixes a latent bug: it previously used `defaults.model` and ignored
      per-character `[selected]` preferences, so the rebuilt chat-cache prefix
      could differ from what chat would actually send.
- [x] **Heartbeat background-model override** —
      `autonomy/manager.rs::apply_heartbeat_model_override`. Now resolves via
      `preferences::resolve_background_model(BackgroundTask::Heartbeat)`.

---

## Tier 2 — architectural smells

- [x] **Five parallel "build a request" paths.** Closed by analysis. The
      two that genuinely duplicated each other (chat handler + heartbeat
      cold rebuild) now share `handler::prepare_chat_context`. The
      remaining three sites do meaningfully different things and aren't
      worth force-merging:
      - `apply_heartbeat_model_override` swaps the model on a request that
        already exists.
      - `compaction_impls::build_compaction_request` either rebuilds with
        the compaction model while inheriting the cached prefix (cache hit)
        or builds fresh.
      - `dreaming::build_librarian_request` clones the cached request to
        preserve the chat model + cache prefix, or builds fresh.

      "Which model? Which tools? Which system?" is now consistently resolved
      via `preferences::resolve_background_model` and the
      `system_suffix`/`tools` field rules. Each remaining site has a
      genuinely different shape; an umbrella helper would just be the
      same `match task` switch with more indirection. Will revisit if a
      fourth task forces a new variant.

- [x] **`s.last_request = None` after compaction is now arguably wrong.**
      Closed — wontfix. On re-investigation: nulling is the correct
      behavior. After compaction the old `last_request.messages` no longer
      matches `active.jsonl` (most entries have been moved to segments), so
      a heartbeat reusing it would send a stale pre-compaction prefix as if
      it were the current conversation. The next call rebuilds from disk
      and produces a correct shorter prefix. The buglist suggestion to
      "preserve last_request" would only save cache rebuilds if compaction
      itself warmed the new prefix in the provider cache, which it doesn't.

- [x] **Trailing `role: "system"` message hack.** Closed by adding
      `LlmRequest::system_suffix: Option<String>` (transient, `#[serde(skip)]`).
      shore-llm's `preprocess_request` expands the suffix into a trailing
      `role: "system"` message just before provider dispatch, so per-provider
      `<system_instruction>` wrapping continues to apply uniformly. Heartbeat,
      background compaction (cached path), and dreaming (cached path) now use
      the field instead of mutating `request.messages` directly. The
      per-provider inline conversion is still needed for genuinely mid-history
      `inject_system` messages, so it stays — but the trailing-system case is
      now declarative.

- [x] **Cache-breakpoint math after compaction is fragile.** Closed.
      The tail shape (1 user message + `system_suffix`) is now
      centralized as `COMPACTION_TAIL_USER_PROMPT_COUNT` in
      `compaction_impls`, with `append_compaction_tail` as the single
      site that applies it and a `debug_assert_eq!` that fails loudly
      if a caller drifts. Two regression tests pin the wire-level
      behavior:
      - `compaction_tail_merges_into_compact_now_not_prefix` asserts
        Anthropic's `convert_inline_system_messages` merges the
        trailing system into the compact-now user, not into a cached
        prefix turn.
      - `compaction_tail_preserves_cache_breakpoint_positions` drives
        `build_body` end-to-end with a compaction-shaped request that
        has existing cache_control markers, asserting they stay on
        their original positions and no fresh marker lands on the tail.

- [x] **No regression test pins tools-in-prefix.** Added
      `cached_compaction_request_matches_chat_prefix_byte_for_byte` —
      asserts every prefix-relevant field (`system`, `tools`, leading
      messages) is byte-identical between the chat request and the
      compaction request it spawns. Fails immediately on any future
      refactor that drops one.

- [x] **API payload logs only retain ~3 days.** Closed. Added
      `LlmRequest::retain_long: bool` (transient, `#[serde(skip)]`).
      `debug_log::log_request` routes flagged calls to
      `debug/api_logs_long/` instead of `debug/api_logs/`, so operators
      can run separate cron timers per tier (chat ~3 days, background
      ~30 days). Wired from the three background sites: compaction
      (`RealCompactionLlm::build_compaction_request`), dreaming
      (`build_librarian_request` both paths), and heartbeat
      (`apply_heartbeat_model_override`). ARCHITECTURE.md +
      CONFIGURATION.md document the split for operators.

- [x] **`data_dir` threading is ad hoc.** Compaction's `run_compaction` no
      longer takes a separate `data_dir` arg — it pulls from
      `config.dirs.data` like dreaming does. One source of truth, three
      callers simplified (`handler/task`, `autonomy/manager`,
      `shore_test_harness`).

- [x] **Heartbeat cold rebuild rebuilds tools fresh.** Now handled by the
      same `prepare_chat_context` helper chat uses, so both paths go through
      identical `tool_use.tools` toggles + `render_tool_defs` invocation —
      drift between them is no longer possible without changing the shared
      helper.

---

## Tier 3 — duplication audit ("a thousand different ways to build the message")

Sorted by lines-of-code-removed-per-refactor.

### High blast radius

- [x] **The "load 4 prompt files, assemble, build_llm_messages" block is
      copy-pasted.** Closed by `handler::prepare_chat_context` (new module
      `backend/daemon/src/handler/context.rs`). Both `handler::task::handle_generation`
      and `autonomy::manager::rebuild_request_from_disk` now construct a
      `PrepareChatContextParams` and consume the `PreparedChatContext`.
      Compaction and dreaming don't use this block (they assemble a much
      simpler request: a single user prompt + system instruction), so routing
      them through this helper would be a different shape of refactor.

- [x] **Per-task model-resolution chain is rebuilt three times.** Closed by
      `preferences::resolve_background_model` + `resolve_chat_model_for_character`.
      All four background sites (manual `/compact`, background compaction,
      dreaming, heartbeat override) and the heartbeat chat-cache rebuild now
      route through these helpers. The old `state::memory::resolve_compaction_model`
      and `dreaming::resolve_dreaming_model` shims were removed.

- [x] **Per-character paths are joined ad-hoc in ~46 places.** Closed
      for production sites. The earlier round added
      `shore_config::{ACTIVE_JSONL_FILE, SEGMENTS_DIR,
      COMPACTION_MANIFEST_FILE}` constants and
      `character_data_dir/character_active_jsonl/character_segments_dir/
      character_compaction_manifest` free functions. The follow-up
      sweep migrated the remaining production sites:
      `autonomy::manager` (state_path, heartbeat_log_path,
      build_tool_context, ensure-active-prompt paths),
      `memory::dreaming` (sweep + librarian-request + state I/O),
      `memory::dreams_log::dreams_log_path`,
      `memory::compaction::mod::run_compaction`, `handler::{mod,
      task, generation, images, command_dispatch}`,
      `commands::navigation::character_info`, and `characters` (all
      three discover/refresh/reload sites). Test fixtures keep their
      literal `data_dir.join("alice")` form — they exist to *seed* a
      character dir, not to *follow* the production path convention,
      and migrating them is churn without typo-surface reduction.

### Medium blast radius

- [x] **Provider `translate_messages` / `translate_tools` × 4.** Closed
      with the productive consolidation done and the rest declared
      not-worth-doing.

      Productive half: Z.AI's `translate_messages` (~180 lines) and
      `translate_tools` are now thin shims over
      `openai::translate_messages` / `translate_tools`. The two
      provider-specific differences became flags on `ProviderContext`:
      `wrap_inline_system` (Z.AI accepts raw `role:"system"`
      mid-history, everyone else needs the `<system_instruction>`
      wrapper) and `drop_prior_thinking` (Z.AI's `zai_clear_thinking`
      option). The Z.AI `reasoning_content` field name flows through
      `reasoning_field_for`.

      Unproductive half (Gemini + Anthropic): Anthropic uses the
      Shore-internal Anthropic-shaped format, so its "translate"
      is mostly a no-op (only `convert_inline_system_messages` runs).
      Gemini's wire shape is fundamentally different — `parts` arrays,
      `functionCall`/`functionResponse` blocks, role rename
      (`assistant` → `model`), `systemInstruction` at top level, a
      `tool_use_id → name` pre-pass unique to Gemini. Any shared
      abstraction would be either too leaky (provider flags
      multiplying like `ProviderContext` did) or too thin (just hiding
      already-extracted helpers like `extract_system_text`,
      `wrap_inline_system_instruction`, `translate_tool_declarations`,
      all of which are already shared). Not worth pursuing.

- [x] **`<system_instruction>` wrapping is reinvented in 4 providers.**
      Closed. Trailing system instructions flow through `system_suffix`
      (Tier 2 #3). The tag-spelling itself
      (`<system_instruction>...</system_instruction>`) is a single
      helper, `stream_helpers::wrap_inline_system_instruction`, called
      from all five sites (Anthropic, OpenAI ×2, Gemini, Claude Code
      session + driver). Future rename or sentinel swap edits one
      place. The remaining per-provider "wrap and emit" logic
      genuinely needs to know each provider's message shape (Anthropic
      content arrays, Gemini parts, OpenAI string-or-array, Claude
      Code stdin) — that's not duplication, it's the cost of supporting
      four wire formats.

- [x] **JSONL parse loop is open-coded where `MessageStore` already exists.**
      `compaction/background.rs::run_compaction` now loads via
      `MessageStore::load`. Added `Message::is_tool_result_only()` to
      shore-protocol, and migrated four inline copies of the same check
      (`commands/state/memory`, `engine/messages::turn_count` /
      `is_real_user_turn`, `compaction/background`).

- [x] **Segment + manifest writing exists in two implementations.** The two
      sites in `commands/conversation.rs` were test fixtures, not real
      command paths. Consolidated into `test_support::write_segmented_fixture`
      with a comment that points future tests to
      `compaction_impls::archive_and_retain` for tests exercising the real
      pipeline.

### Lower priority

- [x] **`character_data_dir(&self) -> &str` declared on three traits.** Not
      actually three traits: `character_data_dir` is declared exactly once
      on the `ToolContext` trait at `tools/mod.rs:93`. The other two
      occurrences are concrete implementations (`HandlerToolContext` and
      `SharedToolContext`) implementing that same trait method. Misread of
      grep results during the audit — no consolidation needed.

- [x] **`s.last_request = Some(req.clone())` written from 4 sites.** Added
      `cache_last_request(state, character, req)` private helper. The public
      `notify_last_request` and the two internal heartbeat/dormant-ping
      paths all route through it now, so the log line stays consistent and
      future callers can't silently forget to emit it.

- [x] **Provider 4xx/error handling.** Already centralized: every provider
      (`anthropic`, `openai`, `zai`, `gemini`) routes non-success responses
      through the shared `providers::check_response`, which builds a single
      `LlmError::HttpStatus { status, body }` with consistent logging. The
      only provider-specific error translation is `claude_code::quota` (CLI
      quota errors → synthetic 429), which is a different responsibility
      from generic 4xx parsing. Audit found no actual "four near-copies."

- [x] **`write_jsonl` test helper defined twice.** Moved to
      `test_support::write_jsonl`. `engine/mod.rs` and
      `commands/conversation.rs` import the shared helper.

---

## Recommended order

If picking three from this list by impact-per-line-of-refactor:

1. **Tier 3 #2** — background-model resolver with overlay built in. Would
   have prevented the entire 2026-04-27 → 2026-05-14 incident. Closes
   Tier 1 #1, #2, #3 as a side effect.
2. **Tier 3 #1** — `prepare_chat_context` helper. Directly attacks the
   "five build-request paths" smell.
3. **Tier 2 #3 + Tier 3 #5** — `system_suffix` on `LlmRequest` plus single
   provider transform. Closes the trailing-system hack and a
   future-provider footgun.

---

## Progress as of 2026-05-14

Started from the 2026-05-14 compaction-loss investigation. Tier 1 closed in
the first three commits via the recommended order above. Remaining sweeps
through Tier 2/3 picked up the small/cheap wins along the way.

Final tally: **all 25 items closed** — 19 with concrete code changes,
6 closed-by-analysis with the rationale filed inline (the consolidations
that would have produced churn-without-value were declined, not
deferred). No `[~]` items remain.

Commit log on `refactor/2026-05-14`:

1.  `Centralize background-task model resolution` — Tier 1 #1/#2/#3 + Tier 3 #2.
2.  `Extract prepare_chat_context helper` — Tier 3 #1.
3.  `Promote trailing-system instruction to LlmRequest::system_suffix` — Tier 2 #3.
4.  `Quality follow-ups: prefix test, MessageStore, helpers` — Tier 2 #5,
    Tier 3 #6/#7/#9 + the cache-prefix regression pin.
5.  `Drop redundant data_dir arg from run_compaction` — Tier 2 #7.
6.  `Consolidate segmented-history test fixture` — Tier 3 #8.
7.  `Add canonical filename constants + character-data path helpers` —
    Tier 3 #3 (foundations).
8.  `Update BUGLIST with progress + analysis on remaining items` — docs.
9.  `Collapse zai translate_messages into openai via ProviderContext flags` —
    Tier 3 #4 (Z.AI half), -180 LoC.
10. `Single source of truth for <system_instruction> tag spelling` —
    Tier 3 #5 (tag-spelling part).
11. `Route ad-hoc data_dir.join(character) through character_data_dir
    helper` — Tier 3 #3 (production sweep across autonomy, dreaming,
    handler, commands, characters).
12. `Centralize compaction-tail shape + pin cache-breakpoint preservation` —
    Tier 2 #4. Adds `COMPACTION_TAIL_USER_PROMPT_COUNT`,
    `append_compaction_tail`, and two regression tests in shore-llm.
13. `Split API payload debug logs into chat / long-retention tiers` —
    Tier 2 #6. Adds `LlmRequest::retain_long` + `debug/api_logs_long/`
    subdir, wired from compaction/dreaming/heartbeat, with operator
    docs in ARCHITECTURE.md and CONFIGURATION.md.

Items closed by analysis (rationale filed inline above):
- Tier 2 #1 — five build paths. Three remaining sites diverge by task,
  not by oversight; an umbrella helper would just be a `match task`
  switch with more indirection.
- Tier 2 #2 — `last_request = None` after compaction. Wontfix; the null
  is correct because the old prefix no longer matches `active.jsonl`.
- Tier 3 #4 — Gemini + Anthropic `translate_messages`. Wire shapes are
  fundamentally different (Anthropic-shaped pass-through vs Gemini's
  `parts`/`functionCall` restructure). Already-shared helpers
  (`extract_system_text`, `wrap_inline_system_instruction`,
  `translate_tool_declarations`) cover what generalizes; the rest is
  the genuine cost of multi-format support.
- Tier 3 #5 — mid-history `inject_system` wrap logic. The tag spelling
  is shared via `wrap_inline_system_instruction`; the surrounding
  message-shape construction is per-provider for the same reason
  Tier 3 #4 is.
