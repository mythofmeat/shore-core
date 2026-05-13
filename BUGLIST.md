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

- [ ] **`s.last_request = None` after compaction is now arguably wrong.**
      `autonomy/manager.rs:587`. Comment says nulling is fine because the next
      call rebuilds from disk. With tools-passthrough fixed, the cached request
      would have been a clean handoff into the next compaction or heartbeat.
      Nulling forces a cache rebuild we're now positioned to avoid.

- [ ] **Trailing `role: "system"` message hack.** Compaction and dreaming both
      append a system-role message and rely on each provider's
      `convert_inline_system_messages` to wrap it in `<system_instruction>` and
      merge into the prior user turn. Implemented in Anthropic, OpenAI, Gemini,
      Claude Code — convention enforced by repetition, not types. A new
      provider could ship without the transform and silently break compaction.
      Promote to a first-class `system_suffix: Option<String>` on `LlmRequest`.

- [ ] **Cache-breakpoint math after compaction is fragile.**
      `apply_cache_control` places markers at `depth_turns=[0,1]` (last two
      message breakpoints). Compaction appends 2 messages, so the existing
      markers (preserved via `has_existing_markers` skip) end up at positions
      N-3/N-4. Works today, but depends on "compaction final user message +
      trailing system → merged into one user turn" always being true. If that
      shape ever changes, cache hits silently degrade.

- [ ] **No regression test pins tools-in-prefix.** Unit test asserts the field
      is preserved, but no integration test compares chat's prefix hash to
      compaction's prefix hash and fails on divergence. This bug **will** come
      back the next time someone re-refactors compaction without understanding
      why `tools` is inherited.

- [ ] **API payload logs only retain ~3 days.** Investigating a 17-day
      regression was un-enumerable past the 3-day window. For compaction /
      dreaming specifically, a slow-rotation / compressed tier would help
      future forensic work.

- [ ] **`data_dir` threading is ad hoc.** Compaction reads `data_dir` as a
      `&Path` arg, dreaming reaches into `loaded_config.dirs.data`. Same value,
      two access paths.

- [ ] **Heartbeat cold rebuild rebuilds tools fresh.**
      `autonomy/manager.rs:1469` calls `tool_system::render_tool_defs`. May or
      may not byte-match what chat produced earlier; if chat and heartbeat ever
      drift in toggle defaults, that's another silent cache miss.

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

- [ ] **`<system_instruction>` wrapping is reinvented in 4 providers.**
      `anthropic.rs:460::convert_inline_system_messages`, `openai.rs` (inline),
      `gemini.rs:81`, `claude_code/driver.rs:570`, `claude_code/session.rs:148`.
      Same XML tag, same "merge into prior user" fallback. Pair with the
      Tier-2 `system_suffix` field on `LlmRequest`: one shared transform,
      every provider drops its hand-roll.

- [ ] **JSONL parse loop is open-coded where `MessageStore` already exists.**
      `compaction/background.rs:32–52` writes a hand-rolled
      `for line in content.lines() { serde_json::from_str::<Message> }` loop.
      `MessageStore::load` already does this; `tools/history.rs:322` uses it
      correctly. The hand-roll also computes `is_tool_result_only` inline —
      should be a method on the protocol `Message` type.

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

- [ ] **`s.last_request = Some(req.clone())` written from 4 sites**
      (1× `handler/persistence.rs`, 3× `autonomy/manager.rs`). A
      `notify_last_request(req)` already exists at `manager.rs:566` but only
      `persistence.rs` calls it. The three autonomy sites reach into the
      mutex directly.

- [ ] **Provider 4xx/error handling.** Each provider does its own status check
      + JSON error parse with slight variations. One shared
      `parse_provider_error(provider_key, status, body)` cleaner than four
      near-copies.

- [ ] **`write_jsonl` test helper defined twice** in
      `commands/conversation.rs:563` and `engine/mod.rs:390`. Move to
      `test_support.rs`.

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
