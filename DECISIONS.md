# Shore V2 — Intentionally Removed / Replaced

V1 features that were consciously not ported to V2, either because they were
replaced by better alternatives or because they don't fit the V2 architecture.

Add items here as decisions are made.

## Replaced by Better V2 Alternatives

- **defaults.cli_target_character** (10.1) — Removed. V2 uses a state file +
  `SHORE_CHARACTER` envvar for character targeting, and defaults to the only
  character for single-character setups. The V1 config default caused more
  problems than it solved.

- **Flat models.toml with [[models]] array** — Replaced by nested
  [chat.provider.model] config structure with include/conf.d support.
  More expressive, matches V1's original design intent.

- **Separate models.toml file** — Merged into config.toml. Can still be
  split out via `include = ["models.toml"]` or `conf.d/models.toml` if desired.

- **provider_defaults section** — Replaced by hardcoded provider defaults
  (ported from V1's PROVIDER_DEFAULTS) plus inline provider-level scalars
  under [chat.provider]. More ergonomic — zero config for known providers.

- **Swipe CLI command** — Removed from CLI; still available daemon-side.
  Will be TUI-only (swipe gestures / keybindings make more sense in TUI context).

- **`shore info` command** (5.14) — Removed. Entirely redundant with `shore status`,
  which already shows character, model, message count, and more.

- **`shore autonomy pause/resume`** (2.8) — Removed. Subsumed by
  `shore config autonomy.enabled true/false` (5.41).

- **`shore cache suppress/unsuppress`** (5.48/5.49) — Removed. Cache refresh
  is now handled by the unified interiority system (no separate keepalive).

- **CLI image commands** (5.50 list, 5.51 import, 5.52 describe) — Removed.
  Superseded by in-context image tools (`send_image`, `list_images`,
  `recall_image`) which the character uses during conversation.

- **research_web** (4.8) — Removed in favor of the LLM orchestrating
  multi-step research via `web_search` + `fetch_url` through the existing tool loop.

## Architecture Decisions

- **Multi-conversation per character** — V1 had list/switch/new conversation
  commands. V2 uses single-conversation-per-character via CharacterRegistry.
  Reset clears the conversation; no need for multiple named conversations.

- **Toggle private mode** — Removed. V2 has no private/public distinction
  for conversations.

- **RAG injection in prompt assembly** (9.2) — Removed. In V1 this was
  completely superseded by the agentic memory tool-use loop; passive RAG
  context injection in the system prompt is redundant when the character
  has tool-use access to memory search. The memory tool (9.3) is the
  correct path for memory retrieval.

## Deferred Indefinitely

- **Telegram bot** (1.1) — Never used. Message routing, typing indicators,
  image attachments, texting delay simulation. Can re-implement later if needed.

- **Discord bot** (1.2) — Never used. Slash commands, selective character
  filtering. Can re-implement later if needed.

## Superseded by Existing Features

- **Failed message list/retry/clear** (5.45–5.47) — Removed. Auto-retry on
  transient errors + `shore regen` covers this use case.

## Not Needed

- **Search conversations** (5.13) — Not needed. The memory agent already
  covers the use case of finding whether something was discussed, in a more
  thorough and contextual way than raw full-text search.


- **Insert message at position** (5.19) — Never used. No practical use case.

- **Detach attachment** (5.20) — Never used. No practical use case.

- **Memory import command** (5.33) — A standalone script is more appropriate
  than a built-in command for one-time bulk imports.

- **Reset subcommand** (5.11) — Not needed. Users can delete or archive
  the conversation file directly for a fresh start.

- **Connection error hints** (7.16) — Not worth the complexity. The error
  message from the OS is sufficient.

## Failed Concepts (not porting)

- **Interiority — journal writing** (2.4) — Failed concept in V1. Replaced by the new interiority system (autonomous turns with full tool access).
- **Interiority — story writing** (2.5) — Failed concept in V1. Replaced by the new interiority system.
- **Interiority scheduling** (2.6) — Replaced by InteriorityClock (simple timer + dormancy).

## Compaction: Turn-Based Semantics (2026-03-31)

**Decision**: Compaction config fields renamed from message-based to turn-based
(`min_messages` → `min_turns`, `max_messages` → `max_turns`, `keep_recent` →
`keep_recent_turns`). Defaults lowered from 20/60/4 to 8/16/2.

**What changed**:
- All config fields and internal structs renamed to reflect that compaction
  tracks user turns (excluding tool-result-only messages), not raw messages.
- `has_enough_messages()` → `has_enough_turns()` — gate is now simply
  `turn_count >= min_turns` with no invisible addition of `keep_recent_turns`.
- Retention split (`find_turn_split`) counts backward by real user turns
  instead of slicing by raw message count, so `keep_recent_turns` preserves
  complete turn pairs (user + assistant).
- Startup validation in `AutonomyManager::new`: if `min_turns <= keep_recent_turns`
  or `max_turns <= keep_recent_turns`, compaction is disabled with an error log.

**Why**: The old naming caused compaction to appear broken — a user with
`max_messages = 26` at 66 raw messages wouldn't trigger compaction because the
engine tracked 23 real user turns. The names implied total messages but the
logic counted turns. Renaming eliminates the mismatch.

**Trade-off**: Breaking config change — old field names (`min_messages`, etc.)
will fail to parse due to `deny_unknown_fields`. Accepted because Shore V2 is
pre-release and the old names were actively misleading.

## Async Message Generation (2026-03-31)

**Decision**: Message generation (Message/Regen) now runs in spawned `tokio` tasks
rather than blocking the handler loop. Commands (status, log, diagnostics, etc.)
are processed inline and always return immediately.

**What changed**:
- `CharacterRegistry.engines` now stores `Arc<tokio::sync::Mutex<ConversationEngine>>`
  instead of bare `ConversationEngine`. The registry lock only needs to be held
  briefly to retrieve an engine Arc; the engine lock is independent.
- A `GenContext` struct (Clone-able, all Arc-backed) holds everything a generation
  task needs: registry, llm_client, push_tx, autonomy, atomics, session_tokens, diagnostics.
- `MessageHandler::run()` spawns `tokio::spawn` for every Engine message and processes
  Commands inline. The handler loop never blocks on LLM streaming.
- `session_tokens` is now `Arc<std::sync::Mutex<SessionTokens>>` shared between
  `CommandContext` and generation tasks so the status command always sees live counts.
- `is_first_after_restart` and `has_seen_cache_read` are now `Arc<AtomicBool>` so
  generation tasks can update them without coordinating with the handler.

**Why**: A long LLM generation was blocking `shore status`, `shore log`, and any other
command that arrived while streaming. This was user-visible friction.

**Trade-off**: A mutating command (edit, delete, compact) acquires the engine lock and
holds it for the command's duration. If a generation task is also waiting to append
to the same engine, it waits. This is intentional serialization — coherent state
is more important than latency for mutating operations.

## Collation Pipeline Rewrite (2026-03-31)

Rewrote the memory collation pipeline to fix multiple design flaws in the original implementation.

**Changes made:**
- Replaced one-shot `collation_skip` table with `collated_at` timestamp watermark on entries
- Reordered phases: merge-then-split (collate → tidy) instead of split-then-merge
- Protected image entries (`image_path` non-empty) and canonical entries from collation
- Fixed merge timestamps to use `min(sources)/max(sources)` instead of first-source copy
- Fixed split supersession to store all replacement IDs, not just the first
- Added embedding-driven clustering: reads existing vectors from the vector store, clusters by cosine similarity in-memory, sends focused 5-15 entry batches to the LLM instead of one giant prompt
- Added incremental timestamp backfill phase (20 entries per run, walks ancestry chain)
- Added `shore memory collate --full` convergence mode (loops until stable, max 10 passes)
- Added `shore memory purge --older-than 30d` to delete verified superseded entries
- Added `collated_at` column via idempotent migration, `delete_entry()` and `vacuum()` DB methods
- Added optional `AgentIndexer` and `VectorStore` params to collation pipeline

**Why**: The `collation_skip` table made collation permanently one-shot per entry — confidence decay ran once and never again, entries left alone could never be reconsidered. Batch LLM calls didn't scale (all candidates in one prompt). 74% of entries had empty timestamps from V1 import propagating through collation. Image and canonical entries had no protection from merge/split.

**Trade-off**: The `collation_skip` table and its DB methods still exist (no destructive removal) but are no longer called by collation. The vector store parameter is optional — clustering falls back to sequential chunking without it. The `collated_at` watermark uses string comparison of RFC3339 timestamps, which is correct for lexicographic ordering but fragile if non-RFC3339 values are stored.

### Collation Candidate Selection and Model Config (2026-04-01)

**Decision:** Collation candidate selection uses TTL-based reconsideration instead of one-shot watermark. A dedicated `defaults.collation` model config controls which LLM is used.

**Changes made:**
- Replaced simple `updated_at > collated_at` watermark with two-tier selection: new entries (`collated_at` empty) are always candidates; previously-collated entries become candidates when their TTL expires (default 7 days)
- Added `defaults.collation` config field with fallback chain: `collation` → `memory_agent` → `model` → first chat model. Removed `active_model` (runtime session state) from the resolution chain.
- Added `memory.collation.batch_limit` (default 10) to cap entries processed per run, controlling LLM cost
- Added `--limit` CLI override for manual `shore collate` runs
- Wired `AgentSearchContext` + `RealAgentIndexer` at all 3 collation call sites (manual, post-compact inline, auto-collation) — enables embedding-driven clustering and indexes collation outputs into vector store + BM25
- Changed post-pipeline stamping to only stamp entries that were actually examined as candidates, preserving TTL clocks on unexamined entries
- Unified model resolution across all 3 call sites via shared `resolve_collation_model()` helper

**Why:** The original watermark (`updated_at > collated_at`) was permanently one-shot — once stamped, entries were never reconsidered unless externally modified. TTL-based reconsideration allows incremental refinement: `shore collate` can be run repeatedly to work through the backlog, and entries naturally come up for re-evaluation as their TTL expires. The separate model config exists because collation is synthesis/judgment work (merge decisions, split decisions, entity normalization) that benefits from a more capable model than memory retrieval.

**Trade-off:** With `batch_limit = 10`, convergence mode (`--full`) may take many passes. The existing 10-pass cap prevents runaway, but a single `--full` invocation could process up to 100 entries. This is acceptable since the user explicitly opts into convergence mode.
### OpenRouter proxy removed from Anthropic SDK (2026-04-01)

**Decision:** The Anthropic SDK (`sdk = "anthropic"`) no longer supports custom `base_url`. Setting one is a runtime error with a message pointing to the `openrouter` SDK. Localhost is exempted for unit tests.

**Changes made:**
- Removed `base_url()`, `is_native_anthropic()`, and Bearer auth fallback from `anthropic.rs`
- Removed OpenRouter `provider` routing block from `build_body()`
- Removed `strip_thinking_from_prior_assistants()` — the Anthropic API handles thinking block stripping internally (confirmed via live testing with adaptive thinking on direct Anthropic)
- Added race condition guard in `execute_keepalive_ping()` — re-checks keepalive state under the lock before sending to prevent stale pings when a concurrent handler transitions state

**Why:** A/B testing with identical request bodies showed OpenRouter intermittently drops prompt cache hits even with static, never-changing system prompt breakpoints and 1h TTL. Direct Anthropic API gets 100% cache hits with the exact same code. Client-side thinking stripping was also unnecessary — the API strips prior-turn thinking internally and the cache key accounts for it. Supporting a proxy path that silently degrades caching is worse than not supporting it.

**Trade-off:** Users who were routing Anthropic models through OpenRouter must switch to using the `openrouter` SDK (which uses the OpenAI-compatible path). This is the correct approach anyway — OpenRouter's API is OpenAI-compatible, not Anthropic-compatible.

### Unified Refine Phase — Collation Redesign (2026-04-01)

**Decision:** Replace the 3 separate LLM phases (collate/merge, tidy/split, normalize entities) with a single unified "refine" phase. Drop entity normalization entirely.

**Changes made:**
- Replaced `phase_collate`, `phase_tidy`, `phase_normalize_entities` with single `phase_refine`
- Replaced 3 prompt templates (`DEFAULT_COLLATE_PROMPT`, `DEFAULT_TIDY_PROMPT`, `DEFAULT_NORMALIZE_PROMPT`) with `DEFAULT_REFINE_PROMPT`
- Replaced 3-method `CollationLlm` trait with single `refine()` method returning `Vec<RefineAction>`
- `RefineAction` is a `#[serde(tag = "action")]` enum with `Merge`, `Split`, `Update` variants — the LLM returns a JSON array of actions it wants to take
- Added context entries: vector store centroid search fetches up to 10 non-candidate entries near each cluster for reference (labeled `[CONTEXT]` in the prompt, read-only)
- Added `Update` action type — in-place rewrite of summary/tags/confidence without creating new entries
- Every action requires a `reason` field that goes directly into changelog entries
- Validation guards: merge requires ≥2 sources, split requires ≥2 results, only candidate entries can be acted on, confidence clamped to [0.0, 1.0]
- `run()` signature takes 1 template instead of 3; `CollationOutcome` fields renamed to `refine_merges`, `refine_splits`, `refine_updates`, `refine_kept`

**Why:** Live testing on real character data revealed three fundamental flaws in the multi-phase approach:
1. **Merge-then-split churn**: Phase 1 merged entries → Phase 2 split them → next run merged them back. The phases fought each other in a loop.
2. **Dangerous entity normalization**: Phase 3 merged "christina" (ex-girlfriend) into "christine" (mother) because it only saw name/type pairs with no semantic context. This is a data-corruption-level bug.
3. **Narrow isolated decisions**: Each LLM phase lacked context about what the other phases did. The collate phase might merge entries that the tidy phase would then immediately split.

A single holistic call lets the LLM see all candidates + nearby context and make coherent merge/split/update decisions in one pass.

**Trade-off:** The single prompt is larger (candidates + context entries), increasing token cost per call. Accepted because: (a) the multi-phase approach made 3 separate LLM calls anyway, (b) context entries provide critical disambiguation that prevents data corruption, (c) batch_limit caps total candidates per run. Entity normalization is permanently dropped — the risk of incorrect merges (christina→christine) outweighs the benefit of consistent naming.

### Interiority System — Replace Heartbeat (2026-04-01)

**Decision:** Replace the 5-state heartbeat probability machine with a simple interiority system that gives characters periodic autonomous turns with full tool access.

**Changes made:**
- Deleted `heartbeat.rs`, `timing.rs`, `time_parse.rs` from `shore-daemon/src/autonomy/`
- New `interiority.rs`: `InteriorityClock` with two states (Active, Dormant), timer with jitter, dormancy counter
- New `scratchpad.rs` in `shore-daemon/src/tools/`: 4 tools (`scratchpad_list`, `scratchpad_read`, `scratchpad_write`, `scratchpad_delete`) with path traversal protection
- Rewrote `autonomy/manager.rs`: `execute_interiority_tick()` reads conversation from `active.jsonl`, builds full prompt with identical tool set, sends to LLM, extracts optional `<sendMessage>` tags for user-visible output
- Replaced `AutonomyConfig` fields: removed `personality`, `max_unanswered`, `max_deferral_hours`, `heartbeat`; added `interiority: InteriorityConfig`
- Replaced `CapabilitiesConfig`: `heartbeat_enabled` → `interiority_enabled`, added `scratchpad_enabled`
- Updated CLI output, persisted state (version 1→2), all downstream consumers

**Why:** The 5-state heartbeat was overengineered — complex scheduling heuristics (τ computation, engagement scores, heatmaps, social need bars, time parsing from natural language) for a simple goal: "the character should sometimes do things on its own." The interiority system achieves the same goal with a timer + dormancy counter. Additionally, the heartbeat used separate, simpler prompts that couldn't leverage the character's full tool set. Interiority ticks use the identical system prompt and tool definitions as normal conversation, preserving Anthropic prompt cache.

**Trade-off:** The interiority system has no adaptive timing — it doesn't speed up during active conversation or slow down during quiet periods. The timer is fixed (with jitter). This is intentionally simpler. If adaptive timing proves necessary, it can be layered on top of the InteriorityClock without changing the fundamental architecture.

### Unified Interiority — Replace Keepalive (2026-04-03)

**Decision:** Delete `CacheKeepaliveScheduler` entirely. Merge cache refresh into InteriorityClock via dual deadlines and a rolling JSONL journal for tick-to-tick continuity. One LLM call per tick.

**Changes made:**
- Deleted `cache_keepalive.rs` (832 LOC — 4-state machine, coordination logic, `snap_to_deadline`)
- New `interiority_journal.rs`: `JournalEntry` types (Thought, ToolCall, ToolResult, MessageSent), JSONL file I/O, rendering, budget truncation, atomic compaction
- `InteriorityClock` now tracks two deadlines: `next_tick_at` (full interiority) and `next_cache_ping_at` (bare cache refresh). Full tick resets both. Returns `RunTick`, `RunDormantPing`, or `None`
- New `execute_unified_tick()`: reads journal → renders into prompt → ONE Opus call → parses response into journal entries → appends → compacts if oversized
- New `execute_dormant_ping()`: bare `max_tokens=1` call, no journal, no prompt changes
- Removed `coordinate_interiority_keepalive()`, `notify_api_response()`, keepalive config from handler
- `ensure_state` takes `cache_ttl_secs: Option<u64>` instead of `CacheKeepaliveConfig`
- Persisted state version 2→3 (drops `cache_ping_count`)
- Removed `max_tool_rounds` from `InteriorityConfig` (tool calls now spread across ticks via journal)

**Why:** The old system had two separate timers with fragile coordination (`snap_to_deadline` was effectively a no-op during Pinging state). The tool-use loop cost 3-4 Opus calls per tick (~$2.40/day). The keepalive state machine (Monitoring→Active→Pinging→Stopped) was complex for what it did. The unified system achieves the same goals — autonomous thinking + cache refresh — with one call per tick and zero coordination code. ~3.5x cost reduction.

**Trade-off:** No explicit cache miss detection. The old keepalive stopped after a cache miss; the new system just keeps pinging. In practice, cache misses only happen when the conversation context changes (which resets the prefix anyway), so stopping was unnecessary complexity.

### Separate client.toml for Client Configuration (2026-04-02)

**Decision:** Add `$XDG_CONFIG_HOME/shore/client.toml` as a client-side config file, loaded by `shore-client` independently of the daemon's `config.toml`.

**Changes made:**
- New `shore-client/src/client_config.rs`: `ClientConfig` struct with `default_address` field, `load_client_config()` loader
- Updated `discover_or_default()` to check `client.toml` between the `--socket` flag and instance discovery
- Added `toml` dependency to shore-client

**Why:** Remote clients (running on a different machine from the daemon) had to pass `--socket host:port` on every invocation. A persistent config eliminates the repetition. The file is intentionally separate from `config.toml` because: (a) the daemon config uses `deny_unknown_fields` and would reject a `[client]` section, and (b) the packages will eventually be split — client config must not depend on daemon config infrastructure.

**Resolution order:** `--socket` CLI flag → `client.toml` `default_address` → instance discovery → default Unix socket.

**Trade-off:** `load_client_config()` reads and parses the file on every invocation of `discover_or_default()`. This is acceptable because it is a single small file read, and caching would add complexity with no measurable benefit for a CLI tool.

---

### Image ingestion pipeline and `remember_image` tool

**Changes made:**
- Fixed bug where user-sent images never reached the LLM: the `content_blocks` branch in `handler.rs` ignored `m.images` entirely, making `build_content()` dead code for user messages
- Incoming images are now copied to `<data_dir>/<char>/images/attachments/` with timestamped filenames (matching `generate_image` naming convention)
- Each copied image adds `[Attached image saved as: <rel_path>]` to content_blocks so the character learns the storage path
- New `remember_image` tool lets the character save user-shared images to memory with rich contextual descriptions
- Memory agent's `create_entry` now accepts `image_path` parameter and `"image"` memory_type
- Prompt guidance instructs the character to use `remember_image` when images are shared

**Why:** User-sent images were completely invisible to the LLM — the most basic image feature was broken. The ingestion pipeline ensures images survive beyond the conversation (durable copy) and become searchable memories (via `remember_image` → memory DB → FTS5/RAG).

**Trade-off:** No image compression on ingestion — large images inflate LLM context. No vector indexing at `remember_image` time (matches `generate_image` pattern; backfill via `shore memory reindex`). `send_image` still doesn't emit a `ServerMessage::SendImage` event to the client — display-side concern, separate fix.

---

### Codebase consolidation audit (2026-04-02)

**Changes made (8 phases, all compile-clean, zero test regressions):**

1. **Truncation functions** — 3 duplicate `truncate()`/`truncate_log()` functions removed from `shore-daemon/src/notifications.rs` and `shore-daemon/src/autonomy/manager.rs`. Both now call `shore_diagnostics::truncate_summary`. `shore-llm-client/src/retry.rs` rewritten in-place to use `floor_char_boundary` + "…" (matching the canonical implementation). Tests for the deleted duplicates removed.

2. **ToolToggles refactor** — `shore-config/src/app.rs` `ToolToggles` struct replaced with `BTreeMap<String, bool>` newtype (`#[serde(transparent)]`). Adding a new tool now requires a single change (add a method) instead of three. Named accessor methods (`memory()`, `scratchpad_read()`, etc.) added for callers in `handler.rs`. Unknown tool names in config are silently accepted (no `deny_unknown_fields`).

3. **Provider defaults** — `shore-config/src/models.rs` `hardcoded_defaults()` extracted `base_provider_defaults()` helper for the three fields shared across all 6 providers (temperature=1.0, max_tokens=8192, max_context_tokens=200_000).

4. **Connection handshake** — `shore-client/src/connection.rs` `connect()` and `connect_raw()` deduplicated by extracting `do_handshake()` method (~38 identical lines removed).

5. **Visibility cleanup** — `shore-llm-client` providers module narrowed to `pub(crate)`. `parse_compaction_response` in `compaction.rs` narrowed to `pub(crate)`.

6–8. **LLM streaming helpers** — New `shore-llm-client/src/providers/stream_helpers.rs` with `build_done_event`, `build_start_event`, `build_tool_use_event`, and `StreamTiming`. All three provider streaming functions (`openai.rs`, `anthropic.rs`, `gemini.rs`) migrated to use these helpers. Gemini's first-chunk/subsequent-chunk duplication (~190 LOC) collapsed into a single unified code path.

**Why:** Audit identified 4 duplicate truncation functions, 5 duplicate HTTP client builders, ~500 LOC of LLM streaming boilerplate, and the ToolToggles 3-way synchronization trap. All addressed.

**Trade-off:** ToolToggles loses compile-time enforcement of valid tool names (field access → method call). This is acceptable — tool names are also embedded as string literals throughout the tool dispatch layer anyway.

### Mid-Conversation System Message Injection (2026-04-02)

**Decision:** Add `:sys` TUI command and `shore sys` CLI command to inject `Role::System` messages into the conversation history for mid-conversation behavioral correction.

**Changes made:**
- New `inject_system` daemon command creates a `Role::System` message and appends it to the conversation engine
- TUI: `:sys <instruction>` command (also accepts `:system`)
- CLI: `shore send --system <text>` flag on existing send command
- Anthropic provider: `convert_inline_system_messages()` transforms system-role messages in the array to user/assistant pairs wrapped in `<system_instruction>` XML tags (Anthropic API rejects `role: "system"` in the messages array)
- Gemini provider: same user/model wrapping approach (previously system messages were silently skipped)
- OpenAI provider: no changes needed — already passes `role: "system"` through natively

**Why:** Users need to correct model behavior mid-conversation (e.g. "stop using roleplay actions", "respond in English only") without modifying the system prompt (which invalidates the prompt cache) or sending user-role messages (which pollute conversation context and are treated as dialogue).

**Trade-off:** For Anthropic/Gemini, the system instruction becomes a synthetic user/assistant turn rather than a true system message, which uses slightly more tokens and may be less authoritative than a real system message. Accepted because: (a) these providers don't support mid-conversation system messages at all, (b) XML-tagged instructions are well-understood by the models, (c) the alternative (no injection) is worse.

---

### Base64 image data in wire protocol (2026-04-04)

**Changes made:**
- Added `ImageUpload { filename, data }` struct and `image_data: Vec<ImageUpload>` field to `ClientMessageBody` (client → server base64-encoded uploads)
- Added `data: Option<String>` to `ImageRef` (server → client embedded image bytes)
- Added `data: Option<String>` to `SendImage` (server → client image push)
- Daemon's `ingest_images()` now handles both `image_data` (decode+write) and legacy `images` (fs::copy)
- Daemon's `embed_image_data()` populates `data` on ImageRefs before sending over wire (NewMessage, log command)
- `serialize_for_storage()` strips `data` from images to keep JSONL lean
- TUI and CLI clients encode images as base64 before sending; TUI's `ensure_transmitted_from_b64()` displays from wire data
- All changes backwards-compatible: old `images` (paths) field retained, `image_data` and `data` use `#[serde(default)]`

**Why:** The protocol was path-based — both upload (`client → server`) and display (`server → client`) required shared filesystem access. Remote clients couldn't upload images (server couldn't read client's paths) and couldn't display them (TUI couldn't read server's attachment paths), falling back to `[image: filename.jpg]` text.

**Trade-off:** Log responses now include base64-encoded image data, increasing bandwidth per `log` command. Accepted because: (a) SWP is a local socket protocol, not internet traffic, (b) conversations with dozens of images are uncommon, (c) the alternative (separate image transfer protocol, client-side caching) adds significant complexity for marginal gain.

### Z.AI Provider — Dedicated Module (2026-04-04)

**Decision:** Z.AI (formerly Zhipu AI, international brand) gets its own provider module (`zai.rs`) rather than routing through the existing OpenAI handler. The existing `zhipuai` provider (China endpoint) is kept as-is.

**Changes made:**
- New `Sdk::Zai` variant, new `zai` dispatch arms in `providers/mod.rs`
- New `providers/zai.rs` (~450 LOC) with `stream()` and `generate()` functions
- Two new config fields: `zai_clear_thinking` (default `false`) and `zai_subscription` (default `false`)
- `zai_clear_thinking: false` means reasoning is preserved across turns — the client sends `reasoning_content` back in assistant messages
- `zai_subscription: true` switches from `api.z.ai/api/paas/v4` to `api.z.ai/api/coding/paas/v4` (subscription billing endpoint)
- Handler's `build_llm_messages` gains `include_unsigned_thinking` flag — Z.AI thinking blocks have no signature, so unsigned blocks must pass through to the provider module

**Why:** Z.AI's thinking parameter format (`{"type": "enabled"}` vs Anthropic's `{"budget_tokens": N}`) and reasoning field (`reasoning_content` as a separate field, not embedded in content) are different enough from standard OpenAI that routing through the OpenAI handler would require too many special cases. A dedicated module keeps provider-specific logic isolated.

**Trade-off:** Some code duplication with `openai.rs` (message translation, tool translation, SSE streaming). Accepted because: (a) the duplicated parts are simple and stable, (b) a shared abstraction would need to accommodate three different thinking parameter formats (Anthropic, OpenAI, Z.AI), making it more complex than the duplication it eliminates.
