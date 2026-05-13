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

- [ ] **Five parallel "build a request" paths.** No canonical
      `build_request_for(character, task)`:
      - `handler/task.rs` (chat) — applies overlay, builds prompt, builds tools
      - `compaction_impls.rs::build_compaction_request`
      - `dreaming.rs::build_librarian_request`
      - `autonomy/manager.rs::rebuild_request_from_disk`
      - `autonomy/manager.rs::apply_heartbeat_model_override`

      Each one independently decides: which model? apply overlay? what tools?
      what system? cache_ttl? This is the load-bearing reason new "did X
      forget Y?" bugs keep appearing.

- [~] **`s.last_request = None` after compaction is now arguably wrong.** On
      re-investigation: nulling is the correct behavior. After compaction the
      old `last_request.messages` no longer matches `active.jsonl` (most
      entries have been moved to segments), so a heartbeat reusing it would
      send a stale pre-compaction prefix as if it were the current
      conversation. The next call rebuilds from disk and produces a correct
      shorter prefix. The buglist suggestion to "preserve last_request" would
      only save cache rebuilds if compaction itself warmed the new prefix in
      the provider cache, which it doesn't. Leaving open as a "wontfix —
      analysis filed in commit message" so it doesn't keep resurfacing.

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

- [~] **Cache-breakpoint math after compaction is fragile.** Partially
      revisited after the `system_suffix` migration: compaction now appends
      only **one** message to `request.messages` (the "compact now" user),
      with the actual compaction system prompt living in `system_suffix`
      and getting expanded into a trailing system message at provider
      dispatch. The existing-markers-skip path still relies on Anthropic's
      `convert_inline_system_messages` merging that trailing system into
      the prior user turn — same risk shape, slightly different numbers.
      Genuine fix would centralize the "what shape is the compaction tail"
      knowledge in one place + add a dedicated breakpoint-position test.
      Lower priority than the prefix-equivalence test that already pins
      the high-risk regression. Leaving open.

- [x] **No regression test pins tools-in-prefix.** Added
      `cached_compaction_request_matches_chat_prefix_byte_for_byte` —
      asserts every prefix-relevant field (`system`, `tools`, leading
      messages) is byte-identical between the chat request and the
      compaction request it spawns. Fails immediately on any future
      refactor that drops one.

- [~] **API payload logs only retain ~3 days.** No internal rotation logic
      lives in shore-llm — `debug_log.rs` writes one file per call and the
      module comment is explicit that "operators manage disk usage by
      deleting the folder." The 3-day window must come from the user's local
      cron / systemd timer setup. Real fix needs design input: a separate
      compressed long-retention tier for compaction/dreaming/heartbeat, or
      flagging those payloads with a `retain_long` envelope field. Leaving
      open pending direction.

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

- [ ] **Per-character paths are joined ad-hoc in ~46 places.**
      `data_dir.join(character)`, `character_dir.join("active.jsonl")`,
      `character_dir.join("segments")`, `character_dir.join("compaction.json")`
      — all open-coded. Existing helpers (`character_workspace_dir`,
      `character_memory_dir`, `engine::character_dir`) don't form a coherent
      surface. A `CharacterPaths` struct with `.active_jsonl()
      .segments_dir() .compaction_manifest() .preferences() .workspace()
      .memory_dir()` cuts tens of `.join` sites and removes typo-bug surface.

### Medium blast radius

- [ ] **Provider `translate_messages` / `translate_tools` × 4.**
      `anthropic.rs`, `openai.rs`, `zai.rs`, `gemini.rs` each have their own.
      Z.ai is ~90% OpenAI with a thinking-clear hack — could fold into
      `openai.rs` behind a `ProviderContext` flag (which already exists for
      routing). Anthropic/Gemini are genuinely different shapes, but the
      structural-content extraction (text / tool_use / tool_result / thinking)
      is the same logic three ways. A shared `iter_content_blocks(msg) -> impl
      Iterator<ContentBlock>` removes ~hundreds of lines.

- [~] **`<system_instruction>` wrapping is reinvented in 4 providers.**
      Partly addressed: trailing system instructions now flow through
      `system_suffix`, but each provider still owns its inline conversion
      because `inject_system` (mid-history) and persisted heartbeat-recap
      `Role::System` messages still need wrapping at the provider layer.
      Fully unifying this would require either threading mid-history system
      messages through a separate first-class shape or accepting the
      per-provider conversion as the canonical form. Leaving open as
      lower-priority — the trailing case (the high-traffic path) is fixed.

- [x] **JSONL parse loop is open-coded where `MessageStore` already exists.**
      `compaction/background.rs::run_compaction` now loads via
      `MessageStore::load`. Added `Message::is_tool_result_only()` to
      shore-protocol, and migrated four inline copies of the same check
      (`commands/state/memory`, `engine/messages::turn_count` /
      `is_real_user_turn`, `compaction/background`).

- [ ] **Segment + manifest writing exists in two implementations.**
      `compaction_impls.rs::archive_and_retain` is the real one;
      `commands/conversation.rs:563+679` builds segments and manifests
      directly. If those are test helpers, fine — but if they're real command
      paths they bypass the rollback logic and any future invariants need
      enforcing twice.

### Lower priority

- [ ] **`character_data_dir(&self) -> &str` declared on three traits**
      (`handler/mod.rs:83`, `tools/context.rs:61`, `tools/mod.rs:93`). One
      shared trait would do.

- [x] **`s.last_request = Some(req.clone())` written from 4 sites.** Added
      `cache_last_request(state, character, req)` private helper. The public
      `notify_last_request` and the two internal heartbeat/dormant-ping
      paths all route through it now, so the log line stays consistent and
      future callers can't silently forget to emit it.

- [ ] **Provider 4xx/error handling.** Each provider does its own status check
      + JSON error parse with slight variations. One shared
      `parse_provider_error(provider_key, status, body)` cleaner than four
      near-copies.

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
