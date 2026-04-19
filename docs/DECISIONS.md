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

## TUI clipboard image paste via wl-paste (2026-04-16)

`shore-tui` binds ctrl+v to read an image from the system clipboard and
attach it to the next outgoing message, matching the existing `:image`
file-picker flow. Implementation shells out to `wl-paste --type image/png`
rather than using the `arboard` crate.

arboard was tried first; it works on X11 and some Wayland compositors but
fails on KDE/KWin because the compositor advertises Qt-flavored MIME types
(`application/x-qt-image`) ahead of `image/png`, and arboard's Wayland
backend doesn't walk the offered types to find a usable image format.
`wl-paste` handles this case cleanly.

Tradeoffs: Wayland-only (runtime check on `$WAYLAND_DISPLAY`). Requires
`wl-clipboard` installed — declared as optdepends on the shore-tui Arch
package. X11 and macOS paste are deferred; they can be added behind
platform branches when there's real demand.

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
- Updated `discover_or_default()` to check `client.toml` between the `--addr` flag and instance discovery
- Added `toml` dependency to shore-client

**Why:** Remote clients (running on a different machine from the daemon) had to pass `--addr host:port` on every invocation. A persistent config eliminates the repetition. The file is intentionally separate from `config.toml` because: (a) the daemon config uses `deny_unknown_fields` and would reject a `[client]` section, and (b) the packages will eventually be split — client config must not depend on daemon config infrastructure.

**Resolution order:** `--addr` CLI flag → `client.toml` `default_address` → instance discovery → default `127.0.0.1:7320`.

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

**Trade-off:** No image compression on ingestion — large images inflate LLM context. No vector indexing at `remember_image` time (matches `generate_image` pattern; backfill via `shore memory reindex`).

**Follow-up (2026-04-07):** `send_image` now surfaces the image to clients. After a successful `send_image` dispatch in the tool loop (`shore-daemon/src/engine/tools.rs`), the resolved path is attached to the issuing assistant message's `images` vec (so log replay renders it inline in the TUI) and a `ServerMessage::SendImage` is broadcast for live consumers (TUI image cache, matrix bridge collector).

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

## Timestamps: UTC → Local-Offset RFC 3339

**Decision:** All timestamps are generated with `Local::now().to_rfc3339()` (e.g. `2026-04-04T20:00:00-07:00`) instead of `Utc::now().to_rfc3339()` (e.g. `2026-04-05T03:00:00+00:00`). The `check_time` tool returns a human-friendly format (`"Friday, April 4th, 2026 at 4:34 PM"`).

**Why:** UTC timestamps were displayed as-is in time-gap markers (e.g. `[6 hours later · 3:00 AM]` when the local time was 8 PM), the memory agent prompt labeled its time "UTC" while the system prompt used local, and `check_time` returned local RFC 3339 while everything else stored UTC. Three inconsistent conventions fed into the LLM's context simultaneously.

**Backward compatibility:** Old `+00:00` data in SQLite coexists safely with new local-offset data. chrono's `DateTime<FixedOffset>` arithmetic is offset-aware, so age calculations (`now - stored_timestamp`) produce correct durations regardless of offset. Lexicographic `ORDER BY` may mis-sort entries from the transition day; this affects only display ordering, not correctness. No data migration needed.

**Trade-off:** Timestamps in the database are no longer uniformly UTC. Any future tool that needs absolute ordering across timezones would need to parse rather than string-sort. Accepted because: all current consumers already parse timestamps for arithmetic, and the string-sort sites only affect best-effort display ordering.

## Token Usage Ledger (shore-ledger)

**Decision:** Use SQLite (not TSV/CSV) for the token usage ledger from day one.

**Why:** The primary use case is aggregation queries (cost per model per day, cache anomaly filtering, warm streak counting). These are fundamentally GROUP BY / SUM operations that SQLite handles natively. A TSV would require building a query engine in Rust. rusqlite was already a workspace dependency (used by shore-daemon for memory databases), so this adds zero new weight.

**Decision:** Compiler-enforced recording via LedgerClient wrapper that consumes LlmClient.

**Why:** Convention-based logging ("remember to call ledger.record() after every API call") is fragile and guaranteed to be missed somewhere. By consuming the LlmClient into a LedgerClient and making the daemon hold only the wrapper, it is structurally impossible to make an unlogged LLM call. The type system enforces the invariant.

**Decision:** Use OpenRouter's public /api/v1/models endpoint for per-model pricing.

**Why:** OpenRouter indexes pricing for nearly every model across all providers, with prices matching the official endpoints exactly (confirmed empirically). This gives us a single API call to get accurate pricing for any model, avoiding hardcoded pricing tables that go stale. Prices are cached lazily per-model in the SQLite DB with manual refresh via `shore usage --refresh-pricing`.

**Decision:** Hardcode a 4x multiplier for Anthropic's 1-hour cache TTL write pricing.

**Why:** OpenRouter reports 5-minute cache TTL prices. Shore uses 1-hour cache TTL for Anthropic. The 1h write price is 4x the 5m price. This is a stable relationship defined by Anthropic's pricing structure.

## Interiority: Real Tool Loop Replaces Journal System (2026-04-05)

**Decision:** Replaced the 1-call-per-tick interiority architecture (with JSONL journal for cross-tick continuity) with a real multi-turn tool loop within each tick.

**Problem:** The journal-based approach caused the model to fixate on scratchpad journaling. The full conversation context plus rendered journal steered the model toward processing/introspection rather than using diverse tools (web_search, generate_image, memory, etc.). The model never saw tool results within the same tick, so it had no feedback loop.

**What changed:**
- Deleted `interiority_journal.rs` module (JSONL read/write/render/compact)
- Removed `journal_path` field from `AutonomyState`
- Rewrote `execute_unified_tick` to run a real `generate()` → dispatch → feed back loop, up to `min(max_iterations, 6)` iterations per tick
- First call uses `CallType::Interiority`, subsequent calls use `CallType::ToolLoop`
- Tool loop messages are ephemeral — only `<sendMessage>` content persists to `active.jsonl`
- All tool activity logged to the interiority ring buffer for `shore log --heartbeat`

**Trade-offs:**
- Cost increase: ~$23/month extra (multiple generate() calls per tick instead of one)

## `shore usage` Pricing & Anomaly Fixes

**Date:** 2026-04-06

**Problem:** `shore usage` showed no pricing data (all costs `—`) and `--anomalies` showed "No cache anomalies found" despite the summary reporting 7 anomalies.

**Root causes found and fixed:**

1. **OpenRouter single-model API endpoint dead:** `/api/v1/models/{id}` returns 404 for all models. Rewrote `PricingEngine::fetch_pricing` to fetch the full `/api/v1/models` catalog and scan for the target model. Also bulk-caches all discovered pricing in one pass.

2. **Anthropic model ID mismatch:** Shore stores `claude-opus-4-6` but OpenRouter expects `claude-opus-4.6`. Added `normalize_anthropic_model()` to convert the last digit-hyphen-digit to a dot.

3. **SQL NULL propagation:** `SUM(total_cost)` returns NULL if any row has NULL cost. Changed to `TOTAL()` which returns 0.0 instead.

4. **Anomaly time window mismatch:** Summary counted anomalies over 7d unfiltered; `--anomalies` defaulted to today. Fixed `--anomalies` to default to 7d when `--last` is today (the default).

5. **`--recalculate` silent failures:** Added failure reporting with model ID and reason when pricing can't be fetched.

**Trade-offs:**
- Catalog fetch is larger (full model list ~1MB JSON) but happens once and caches everything
- Anomaly `--anomalies` defaults to 7d only when `--last` is "today"; explicit `--last today` still uses today
- Lost: Cross-tick continuity (the model no longer "remembers" what it did on previous ticks via journal)
- Gained: The model can actually use tools and see results, enabling genuine exploration and discovery

## SDK/Provider Split (2026-04-07)

Decoupled wire protocol (SDK) from endpoint identity (provider) in `shore-llm-client`.

**What changed:**
- `Sdk` enum shrunk from 6 to 4 variants: `Anthropic`, `Openai`, `Gemini`, `Zai`. Deepseek and Zhipuai were just OpenAI dialects, not distinct wire protocols.
- Provider-specific logic (OpenRouter headers, Deepseek reasoning field, etc.) extracted from SDK modules into a centralized `ProviderContext` struct (`providers/context.rs`).
- `LlmRequest.provider: String` renamed to `LlmRequest.sdk: Sdk` (the enum, not a string).
- Anthropic SDK now accepts any `base_url`, re-enabling Anthropic wire protocol through OpenRouter and other gateways.
- Legacy `sdk = "deepseek"` / `sdk = "zhipuai"` in TOML configs maps to `Sdk::Openai` with a deprecation warning.

**Why:**
- The old design tangled "how to format the request" (SDK) with "where to send it" (provider), making it impossible to use the Anthropic protocol through OpenRouter without hacks.
- OpenRouter support for Claude models was hastily removed due to cache debugging issues. The real problem was the tight coupling, not the feature itself.

**Config impact:**
- Users can now override SDK per model: `[chat.openrouter."anthropic/claude-opus"] sdk = "anthropic"`
- Both approaches work: override sdk on an openrouter model, or override base_url/api_key on an anthropic model

## Interiority: Deadline Holder with Self-Scheduling (2026-04-08)

**Decision:** Replaced the fixed-interval Active/Dormant state machine with a deadline-based `InteriorityClock` that lets characters self-schedule via `set_next_wake` tool, decoupled cache keepalive into its own subsystem, and added a recap system for inner-life continuity.

**What changed:**
- `InteriorityClock` rewritten: pure deadline holder + dual abandonment guard (`ticks_without_user >= 3` OR wall-clock silence >= 48h). No more `InteriorityState` enum.
- `CacheKeepalive` (new module): independent 59min ping cycle with 18h break-even gate. No longer entangled with interiority tick scheduling.
- `RecapStore` (new module): JSONL sidecar (`recaps.jsonl`) for character first-person recap entries via `<recap>` tag.
- `set_next_wake` tool: injected into interiority tick tool list, intercepted before `dispatch_tool`. Characters schedule their own cadence (clamped to 1h–48h).
- Dynamic `INTERIORITY_PROMPT`: replaces static constant, includes recent thread context from recaps or ring buffer.
- Recap injection in `trim_messages`: recap entries appear alongside time-gap markers in conversation history.
- `PersistedState` v4: RFC3339 timestamps for `next_wake_at` and `last_user_at`, enabling restart recovery.
- `on_user_message` uses `max()` semantics: `next_wake_at = max(existing, now + min_wake_secs)`. Character-scheduled deadlines are preserved.
- Removed: `InteriorityState` enum, `jitter_factor`, `cache_refresh_interval_secs`, `RunDormantPing` variant from `InteriorityAction`.
- Added config: `dormant_after_idle_time` (48h default), `minimum_interiority_latency` (1h default, configurable for testing).

**Why:**
- The old system treated characters as passive tick recipients. The redesign gives characters agency over their own inner life cadence.
- Cache keepalive was entangled with interiority state transitions, causing unnecessary complexity and coupling.
- The journal system (removed in prior decision) left a gap in cross-tick continuity. Recaps fill this gap with first-person notes that survive compaction.

**Trade-offs:**
- Breaking config change: existing configs with `jitter_factor` will fail to parse (`deny_unknown_fields`).
- `set_next_wake` adds a tool the character can misuse (requesting very frequent ticks). Clamping to [1h, 48h] bounds this.
- RecapStore is append-only with no automatic pruning — acceptable for expected volume (~3 entries/day).

## ConfigDuration type for human-readable durations (2026-04-08)

All duration-type config fields now accept systemd-style strings: `500ms`, `30s`, `2m`, `1h`, `2d`.
Bare integers in TOML are interpreted as seconds for backwards compatibility with programmatic
config generation. Internally stored as milliseconds to support sub-second precision
(used by `retry_backoff`).

Renamed interiority fields for clarity:
- `interval_secs` → `fallback_interiority_interval`
- `max_idle_ticks` → `dormant_after_interiority_turns`
- `max_silent_secs` → `dormant_after_idle_time`
- `min_wake_secs` → `minimum_interiority_latency`

Also renamed across other config sections:
- `idle_trigger_minutes` → `idle_trigger`
- `generation_threshold_secs` → `generation_threshold`
- `retry_backoff_seconds` → `retry_backoff`
- `keepalive_ttl_minutes` → `keepalive_ttl`

**Why:**
- Raw numeric fields with `_secs` / `_minutes` suffixes were confusing — users had to mentally convert units and the naming was inconsistent.
- systemd-style strings (`"1h"`, `"30m"`) are self-documenting in config files.
- Interiority field names were opaque (`interval_secs`, `max_silent_secs`) — new names describe what happens from the user's perspective.

**Trade-offs:**
- Breaking config change: old field names will fail to parse (`deny_unknown_fields`). Users must update config files.
- `cache_ttl` was left as `Option<String>` since it passes through to the Anthropic API directly (only two valid values: `"5m"`, `"1h"`).

## Integration Test Harness (2026-04-09)

**Decision:** Created `shore-test-harness` crate with `TestHarness` that boots a
real daemon in-process and mocks only the HTTP boundary via `wiremock`. All daemon
plumbing (SWP, handler, persistence, autonomy, tools) runs for real.

**Alternatives considered:**
- Full mock of LlmClient via trait abstraction — rejected because it requires
  significant refactoring and wouldn't test the real reqwest/SSE parsing path.
- Record/replay from real API calls — rejected because recordings rot and are only
  marginally better than canned SSE for the bugs that actually occur.
- Real API calls in CI — rejected because it costs money and is non-deterministic.

**Trade-offs:**
- We don't test actual LLM response quality or real provider quirks (socket behavior,
  undocumented error formats). The existing `#[ignore]`-gated e2e tests cover that.
- Autonomy tests use `tokio::time::pause()` which requires all autonomy code to use
  `tokio::time::Instant` instead of `std::time::Instant`.

## Smart Image Resize Pipeline (2026-04-10)

**Decision:** Replaced the MVP single-pass image resizer (`maybe_resize`) with a
format-aware, cached, async pipeline in a new `resize.rs` module.

**Key design choices:**
- **Format preservation:** Transparent PNGs stay PNG; opaque images convert to JPEG.
  Alpha detected by scanning decoded pixels — cheap since already decoded.
- **Quality-first for small images:** Images ≤2048px try quality reduction (90→75)
  before dimension reduction. Preserves resolution for screenshots/diagrams.
- **`fast_image_resize` v6:** SIMD-optimized resize (~14x faster than `image` crate).
  Pure Rust, no system library. `image` crate retained for decode/encode.
- **XDG disk cache:** Resized images cached at `$XDG_CACHE_HOME/shore/resized/`.
  Key = sha256(path + mtime + max_bytes). No eviction — images are ~1-2MB each.
- **Pre-warm pattern:** Async `warm_image_cache()` via `spawn_blocking` before
  sync `build_llm_messages()`. Cache is the communication channel.

**Alternatives rejected:**
- Making `build_llm_messages` async — large refactor for same result.
- In-memory LRU cache — lost on restart, no persistence.
- Iterative quality binary search — too many encodes for pathological images.

**Compromises:**
- Autonomy path is sync — `warm_image_cache` skipped, uses sync fallback.
- Retry may return images slightly over the limit (warn log emitted).
- bpp estimation is content-dependent; 0.85 safety margin handles most cases.

## Unix Sockets Removed — TCP-Only Transport (2026-04-10)

**Decision:** Remove Unix socket support entirely. TCP is the sole transport.

**Rationale:** Unix sockets added complexity (socket path management, stale file cleanup, dual-listener code) with no real benefit over TCP on localhost. For remote clients, identifying an instance by Unix socket path on another machine is meaningless. TCP was already a core feature, so making it the only transport simplifies the codebase and makes instance identity uniform (`host:port`).

**Default:** `127.0.0.1:7320` (localhost-only). Non-loopback binds require explicit
`[daemon].unsafe_allow_remote_access = true`. `allowed_hosts` remains a peer IP
allowlist, not authentication or TLS.

**Trade-offs:** Marginally higher per-message overhead vs Unix sockets on localhost (negligible for JSON-Lines messages). Lost the ability to enforce filesystem-level permissions on the socket file. Shore mitigates this with a localhost-only default and an explicit unsafe remote-access opt-in, but remote TCP is still only appropriate on trusted private or overlay networks.


## Extract shore-daemon-server crate (2026-04-10)

## Abandonment Guard Fix & Debug Commands (2026-04-11)

**Bug:** When the abandonment guard tripped, it cleared `next_wake_at`. On the next tick, step 1 unconditionally bootstrapped a new deadline from the stale `last_anchor`, causing it to immediately re-trip — infinite log-spam loop every tick.

**Fix:** Added `is_abandoned(now)` check in step 1 of `InteriorityClock::tick()`. Once abandoned, the clock stays dormant until `reset_on_user_message()` clears the counter. Introduced `is_dormant()` as the public accessor, and made user-facing status/log output report the unified state label `Dormant` for tick-count dormancy, silent-duration dormancy, and forced dormancy.

**Debug commands:** Replaced the single hidden `shore debug force-tick` with three explicit debug commands using snake_case naming (`#[command(rename_all = "snake_case")]`), and unhid the `debug` subcommand:

- `shore debug interiority_tick_now` — schedules immediate tick, warns if dormant
- `shore debug interiority_status_dormant` — forces dormant, reverts on user message
- `shore debug interiority_status_active` — forces active, reverts naturally via guard

Snake_case was chosen over kebab-case for debug commands to visually distinguish them from normal CLI commands and signal "direct internal access, use with care". This also simplifies the SWP mapping since CLI and wire names are identical under `rename_all = "snake_case"`.

`InteriorityClock` methods split: `force_wake()` (deadline only, no counter reset), `force_dormant()` (sets counter to max, clears deadline), `force_active()` (resets counter + last_user_at + deadline). The old monolithic `force_wake` that did everything was replaced because each debug command maps to a single primitive.

**Compromise:** `interiority_tick_now` on a dormant clock sets the deadline but the tick will be suppressed by the guard. This is intentional — it's a debug tool and the user gets a warning. Silently auto-activating would mask the state.

Extracted `shore-daemon/src/server/` (~1.3K LOC) into a standalone `shore-daemon-server`
workspace crate. The server module had zero internal dependencies on other daemon modules,
making it the cleanest extraction candidate. `RoutedMessage` enum stays in the server crate
because it's a server routing concern (not a wire protocol type) and handler already depends
on the server crate. Registry stays as a submodule (221 LOC, not worth its own crate).

## Refactor Hardening Closeout (2026-04-12)

**Decision:** Close the targeted refactor hardening pass without reopening larger
concurrency redesign work. Shore keeps the current single-process architecture
and only revisits deeper executor or async changes if new measurements justify it.

**What landed:**
- Added maintenance-path timing around compaction, vector-store open/reindex,
  ledger operations, and pricing cache lookups so blocking work is observable.
- Hardened `shore-ledger` shared-state locking with `lock_or_recover()` and
  poison-recovery tests instead of panic-on-poison behavior in production paths.
- Centralized vector-store entry-ID predicate construction and validation in one
  helper, with consistent invalid-ID tests across index/delete/get paths.
- Moved compaction archive/retain file mutation behind an explicit
  `spawn_blocking` boundary and added a regression test proving sibling tasks
  stay responsive while that work runs.
- Promoted the panic classification note to `docs/specs/panic-policy.md` and
  codified the remaining production panic sites as startup-fatal or
  invariant-protecting.

**What we are not doing now:**
- No dedicated maintenance executor or job queue.
- No blanket `tokio::fs` or `tokio::sync::Mutex` rewrite.
- No `parking_lot` migration.
- No further async/concurrency churn unless new timings show an actual hotspot.

### 2026-04-14 — shore-mcp crate added as a debug-only MCP server

Added a new `shore-mcp` crate exposing Shore's CLI surface as MCP tools for AI clients (primarily Claude Code). The crate is gated behind a `feature = "enabled"` + `required-features` on the `[[bin]]`, plus a `cfg(debug_assertions)` stub in `main.rs`, so it is never built by `cargo build --workspace --release` in the default configuration. A custom release profile that deliberately enables both the feature and debug_assertions will produce the real binary — supported but not "default."

**Why:** We wanted Claude Code (and other MCP clients) to drive Shore programmatically for debugging and automated workflows, without (a) bloating the shipped release binary set or (b) giving an AI client default access to the user's real Shore profile.

**Hybrid daemon model:** By default, `shore-mcp` targets an isolated test profile (`$XDG_DATA_HOME/shore-mcp-test/...`) and spawns a dedicated `shore-daemon` child process with `--instance-id=shore-mcp-test` if one is not already running. `--attach-main` opts into the user's real profile via normal discovery. Mutation tools (send/regen/config-set/character-switch/etc.) refuse to execute on the main profile unless `--allow-main-writes` is also passed.

**Sacrificed:** Zero-touch single-binary distribution. You can't `cargo install shore-mcp` from a release checkout without custom profile flags — that's intentional.

### 2026-04-14 — Live-testing policy revised to a four-rule structure

The original blanket rule "never mock `shore-llm`" was causing tests to be skipped entirely rather than rewritten to use real API calls. Revised policy (see `CLAUDE.md` for the authoritative version):

1. Inside `shore-llm-client`: real API calls or recorded fixtures only, never hand-written HTTP responses.
2. Upstream of `shore-llm-client`: trait-level doubles and HTTP-level wiremock (via existing `MockLlmServer` in `shore-test-harness`) are allowed.
3. Live tests (`cargo test --test e2e -- --ignored`) remain mandatory for release verification.
4. When standing in for an LLM response outside `shore-llm-client`, prefer recorded fixtures over hand-written stand-ins.

**Why:** The policy was written to prevent fantasy-output tests — mocks that described the LLM as the author wished it behaved rather than how it actually did. That failure mode is specifically in the parsing/wire-protocol layer, which is confined to `shore-llm-client`. Code upstream of it doesn't benefit from real API calls at all; it benefits from deterministic inputs. The revision preserves the original intent for the layer where it matters and unblocks fast tests for everything else.

### 2026-04-16 — TTS integration as a daemon-side relay to ttsd

Added text-to-speech as a first-class SWP feature. The daemon proxies a
`Speak` request to an external OpenAI-compatible TTS server (ttsd) at
`[tts].host:port/v1/audio/speech`, parses the returned WAV, and streams
`AudioStart` / `AudioChunk` (base64 int16 LE PCM) / `AudioEnd` messages to
the requesting client. A daemon-global `SetLiveSpeak` flag causes any
completed assistant response to trigger the same relay automatically.

Clients (`shore` CLI and `shore-tui`) play audio via `rodio` with the
`wav`-only feature set, feeding int16-decoded samples into an in-memory
`Sink`. Clients without audio hardware silently drop chunks without
erroring — the daemon does not know or care whether playback succeeded.

**Why:** We want to hear characters speak without building a TTS stack into
Shore itself. ttsd already exists, runs on `vegetable`, and exposes the
OpenAI shape. Putting the relay in the daemon (not the client) means every
client — CLI one-shots, TUI live-speak, future bridges — gets audio for
free, and ttsd credentials stay on the daemon host.

**Voice resolution — amendment to the original plan:** The plan assumed
voice names would match character names by convention. They don't: the
user's configured voice is `Nanachan` while the character is `cachetest`.
Resolved by making `[tts].voice` a first-class config field (global with
per-character override via `deep_merge`), falling back to the character
name only when no voice is configured.

**Sacrificed:** No local TTS (espeak/piper). No voice selection UI beyond
config edits. No per-message speaker hinting — the character's configured
voice always applies.

**Live verified 2026-04-16** via shore-mcp test profile: daemon streamed
24kHz mono WAV (166400 PCM bytes) from ttsd at `vegetable:8778`, framed
as AudioStart/AudioChunk/AudioEnd to `shore-cli`. `speak on` / `speak
off` toggled the live-speak flag cleanly (logs: `Live TTS toggled
enabled=true prev=false` then back).

**Sacrificed:** Nothing, in theory. In practice, it raises the discipline bar: upstream tests are now allowed to use doubles, but reviewers have to check that those doubles don't creep into `shore-llm-client` itself.

### 2026-04-16 — shore-mcp Cargo feature gate removed

The `enabled` Cargo feature that gated `shore-mcp`'s bin target (and the optional `rmcp`/`schemars` deps) was removed. `cargo build --workspace` now compiles `shore-mcp` unconditionally. The `cfg(debug_assertions)` stub in `main.rs` stays — release builds still produce a binary that refuses to run.

**Why:** The feature flag added friction without adding a real ship-gate. The actual ship-gate is `contrib/PKGBUILD`, which names each binary by hand (`install -Dm755 target/release/<name>`) and does not list `shore-mcp`. So building it in dev never had any chance of shipping it. Meanwhile, a fresh clone or a `cargo clean` would leave the MCP server registered in `.claude/` pointing at a non-existent binary, silently failing to connect until the user remembered the non-default build incantation. Since shore-mcp is the canonical live-verification path for daemon-surface changes (per project CLAUDE.md), that friction was hitting the workflow it's supposed to accelerate.

**Mechanically:** Dropped `[features]` and `required-features` from `shore-mcp/Cargo.toml`, removed `feature = "enabled"` from `src/lib.rs` cfg gates, simplified `.cargo/config.toml` aliases, and stripped `--features enabled` from test-file docs and panic messages. Pre-existing: when the previously-gated modules started compiling under `cargo check --workspace`, a missing match arm in `handler.rs` for the new `ServerMessage::Audio*` variants surfaced and was filled in (drain silently alongside the other async-push frames).

**Sacrificed:** A tiny amount of default workspace compile time (one extra crate + `rmcp` + `schemars`). The belt-and-suspenders of "double-gated release builds" is now single-gated at runtime via `debug_assertions`, which is sufficient because `contrib/PKGBUILD` is the real ship-gate.

### 2026-04-16 — shore-matrix re-enabled via patched matrix-sdk fork

`shore-matrix` was uncommented from the workspace after being disabled in early April. `matrix-sdk 0.16.0` on rustc 1.94.1 overflows the query depth limit computing the layout of `Client::sync()`'s async fn body. The fix is a one-liner: `#![recursion_limit = "512"]` on `matrix-sdk`'s crate root. Neither the 0.16.0 release nor upstream `main` has this attribute — it's a rustc 1.94+ issue that upstream hasn't shipped a fix for (tracking issue: matrix-org/matrix-rust-sdk#6254; the draft PR #6449 takes a more invasive "gate instrumentation behind a feature" path rather than bumping the limit).

**How:** Mirrored the upstream repo to `http://localhost:3000/eshen/matrix-rust-sdk.git` on the local Gitea, branched off the `matrix-sdk-0.16.0` tag, added the one-line attribute, and pinned via `[patch.crates-io]` in the workspace `Cargo.toml` to commit `8285d1ca5da1f18227ba4eddaeef9bf579a55de6`. Cargo transitively resolves the sibling crates (`matrix-sdk-base`, `matrix-sdk-common`, `matrix-sdk-crypto`, `matrix-sdk-store-encryption`) from the same git source — no additional patch entries needed.

**Drift fixed:** shore-matrix compiled clean against current `shore-client`/`shore-protocol` on the first try. The only code fix was `shore-matrix/src/connection.rs` — `spawn_connection` now accepts a `character: Option<String>` and forwards it to `shore_client::spawn_connection`, along with a new `--character` / `SHORE_CHARACTER` CLI flag wired through external mode. Tests (`bridge.rs`, `tests/bridge_integration.rs`) needed mechanical `rid: None` / `revision: 0` fields added to constructor sites for `StreamStart`/`StreamChunk`/`StreamEnd`/`SendImage`/`CommandOutput`/`Error`/`NewMessage` — protocol drift that doesn't affect the lib code (which only consumes those types). All 23 shore-matrix tests pass; full workspace test still green.

**Deferred (not in this change):** `MessageOverrides` are still always `None`, `ClientMessageBody.image_data` is still always empty, and embedded mode still uses a single daemon connection for all characters (routing all messages to the daemon default). These are missing features, not regressions. Follow-up tickets.

**Revisit trigger:** Drop the `[patch.crates-io]` entry once upstream ships a release that builds on rustc 1.94+. Until then, `cargo update -p matrix-sdk` will silently break — the patch must remain pinned. Documented in `docs/QUIRKS.md`.

**Live verification deferred:** No Matrix homeserver is available on this machine (`conduwuit` is not installed and no external homeserver is configured), so no end-to-end message round-trip was performed. Phase is compile + unit + integration-test verified only. Before declaring this bridge production-ready, install `conduwuit`/`continuwuity`/`tuwunel` (or point at an existing homeserver in external mode) and drive a real message through `matrix client → shore-matrix → shore-daemon → LLM → matrix room`.

### 2026-04-17 — shore-matrix PKGBUILD re-enable + config_dir in instance registry

Two correlated fixes so `shore matrix setup` works out of the box for users whose daemon runs under systemd with a non-default `SHORE_CONFIG_DIR`.

**PKGBUILD:** The Apr 5 disable commit (`a67db40`) stripped `shore-matrix` from the `pkgname` array in `contrib/PKGBUILD` and commented out `package_shore-matrix()`. The Apr 16 re-enable (`5f85fb6`) restored the Cargo workspace but not the PKGBUILD, so pacman still shipped the stale pre-disable `shore-matrix` binary (version `.r300.f65739c-1`) alongside fresh `0.15.0-1` packages of shore-daemon/shore-cli/shore-tui. Restored the package entry (with the homeserver binaries added as `optdepends`).

**Config discovery:** The daemon's systemd unit can set `SHORE_CONFIG_DIR` to a non-XDG path, but that env var doesn't propagate to the user's interactive shell. `shore matrix setup` from the shell therefore couldn't find the same `config.toml` the daemon was reading and failed with `"homeserver required"` as it fell through to external mode.

Added an optional `config_dir` field to `InstanceInfo` / `InstanceEntry` and populate it at daemon registration from `loaded.dirs.config`. Exposed `shore_client::discover_config_dir()` as a parallel to the existing `discover_data_dir()`. `shore-cli`'s `handle_matrix_command` now falls back to the registry when `--config` isn't passed, so `shore matrix setup` transparently reads the running daemon's config regardless of shell env.

**Backwards compat:** `config_dir` is `#[serde(default, skip_serializing_if = "Option::is_none")]` — older `instances.json` files parse fine, and older clients ignore the new field. All 1,208 workspace unit tests pass.

Also extended `discover()` to match a selector against **either** `entry.id` (how `shore-mcp` identifies its test daemon) **or** `entry.config_dir` (how `shore-matrix` identifies the daemon bound to its config). Previously the param was named `config_path` but only matched on `id`, so shore-matrix's daemon connection failed with "no daemon found for id: /path/to/config" even when a daemon with that exact config was registered. Backwards-compatible for callers passing instance IDs.

Separately, fixed a hardcoded-admin bug in `shore-matrix/src/provision.rs:400` exposed by non-default `admin_user` configs: `create_character_room` was granting room power 100 to a literal `@shore-admin:{server_name}` user in its `power_level_content_override`, which only worked when `[connections.matrix.embedded].admin_user` matched the `"shore-admin"` default. Any override (e.g. `admin_user = "eshen"`) left the actual room creator with power 0, failing the subsequent `m.room.join_rules` event with `M_FORBIDDEN`. Now takes `admin_user_id` as an explicit parameter threaded from `EmbeddedState::admin_user_id`.

**Not done yet:** `shore-tui`, `shore-mcp`, and other binaries still rely on `SHORE_CONFIG_DIR` / `--config` for config lookup. They could use the same registry fallback, but none of them have hit a reported issue and dragging them in now would creep this change. Follow-up if/when a user trips on the same friction.

### 2026-04-17 — shore-daemon supervises shore-matrix

The daemon now auto-spawns and supervises `shore-matrix` as a child process when `[connections.matrix]` is enabled in the config. Users no longer have to run `shore matrix setup` or `shore matrix` manually — the bridge comes up with the daemon. Setup-on-first-run still happens inside shore-matrix itself (unchanged behavior via `load_or_init_state()`), so the supervisor has no state of its own; it just ensures a live bridge process.

**Design:**
- Binary lookup: `which::which("shore-matrix")` → fallback to `current_exe().parent()/shore-matrix` → else one `warn!` and the supervisor task exits cleanly. No retry loop on binary-not-found; if the binary isn't installed, spamming the logs won't help.
- Restart policy: exponential backoff (1, 2, 4, 8, 16, 32 seconds cap) with a 5-consecutive-failure give-up threshold. Counter resets after 5 minutes of stable runtime. This catches homeserver-binary-missing, port-bind-failures, and Matrix auth errors without burning CPU forever on a permanently broken setup.
- Shutdown: listens on the same `tokio::sync::watch` shutdown signal the server uses. On signal, sends SIGTERM directly via `libc::kill` (tokio's `Child::start_kill` is SIGKILL on Unix, which would skip shore-matrix's own teardown of tuwunel), waits up to 5s, and escalates to SIGKILL if the child is still alive. `kill_on_drop` is the ultimate fallback.
- Non-fatal: all failure paths log and return from the supervisor task. The daemon never exits because of a Matrix problem.

**Why subprocess, not library:** `shore-matrix` depends on `matrix-sdk` which pulls in `sqlite` and a lot of crypto surface area. Linking it into the daemon would expand the daemon's build time and attack surface for no benefit — the bridge already talks to the daemon via SWP, same as any other client. Keeping it a separate binary also means the `shore matrix setup` / `shore matrix register` CLI commands keep working standalone for debugging; users running the daemon-supervised mode can still invoke them ad-hoc (though the daemon-managed bridge will hold port 6167, which surfaces as a clean tuwunel bind error if they try to run a second bridge).

**Register is NOT automated:** `shore matrix register --username X` remains a manual command. Running it on daemon startup would spam credentials into logs; the credentials live in `config.toml` anyway.

**Hook point:** `shore-daemon/src/main.rs` around line 361 (after the message handler spawn, before the server's run loop), with a clone of the shared `shutdown_rx`. Shutdown is joined before the handler so the Matrix child has a chance to flush before the daemon's SWP server tears down.

**Verification:** compile + 1,208 workspace unit tests + 2 new supervisor unit tests + a 5-scenario isolated integration harness covering (a) no-matrix-config short-circuit, (b) shore-matrix binary missing, (c) tuwunel missing with supervision loop → give-up, (d) full happy path end-to-end with tuwunel spawn + admin + character + room creation + bridge loop, and (e) warm restart on populated state. All tests ran in tempdir profiles with `SHORE_CONFIG_DIR` / `SHORE_DATA_DIR` / `SHORE_RUNTIME_DIR` overrides and non-default ports (7399 daemon, 6168 homeserver), leaving the user's real daemon untouched. Clean shutdown verified: no zombie `shore-matrix` or `tuwunel` processes after `SIGTERM`.

**Deferred:** no lockfile coordinating standalone-CLI vs daemon-supervised shore-matrix. Port collision on 6167 is the natural guard and surfaces cleanly; adding file-lock machinery is overkill for a single-machine user-level daemon.

### 2026-04-18 — `ClientError::Discovery` carries a typed `DiscoveryKind`

`ClientError::Discovery` is now a struct variant `{ kind: DiscoveryKind, message: String }` instead of a bare string. Callers that need to branch on the failure reason (shore-mcp's spawn-on-miss path, `discover_or_default`'s fallback gate) match on `kind` rather than string-prefix the human message.

**Why:** shore-mcp's auto-spawn regressed silently *twice* because the message produced in `shore-client/src/discovery.rs` drifted out of sync with the prefix list in `is_spawnable_discovery_miss` in `shore-mcp/src/profile.rs` — and the same pattern in `should_fallback_to_default` had its own stale prefix (`"instances registry is empty"` vs. the actual `"instances file is empty"`). The message strings had no compile-time link to the match arms, so the only way to notice breakage was to actually run shore-mcp against an empty registry and watch it refuse to spawn. A typed discriminant moves those checks into the type system.

**Kinds:** `RegistryMissing`, `RegistryEmpty` (file `[]`, empty string, or all PIDs pruned as dead), `NoMatch` (selector didn't hit any live entry), `RegistryCorrupt`, `Io`.

**shore-mcp spawn policy:** `RegistryMissing | RegistryEmpty | NoMatch` all trigger auto-spawn. `NoMatch` is now spawnable where it previously wasn't — that was the actual user-visible bug: an empty registry with a selector (`MCP_INSTANCE_ID`) produces `NoMatch`, not `RegistryEmpty`, and the old string match only covered the `None`-selector empty case. Corruption/IO bubble up as before — those mean the user's environment is broken, not merely that no daemon is registered.

**Fallout:** one breaking change to `shore-client`'s public API (the `Discovery` variant shape). `DiscoveryKind` is re-exported from `shore-client` for downstream matchers.

**Verification:** 39 shore-client unit tests + 18 shore-mcp unit/integration tests pass; live `cargo mcp-itest` against a real daemon passes; manual run of `target/debug/shore-mcp` with an empty `instances.json` now spawns the dev-repo daemon and registers it (previously exited with `discovery error: no daemon found matching id or config_dir: shore-mcp-test`).

### 2026-04-18 — shore-matrix auto-heals orphaned provision state via whoami liveness check

`shore-matrix` now verifies every saved access token against the live homeserver before trusting it, and wipes + re-provisions on `401 M_UNKNOWN_TOKEN`. Previously, `provision_character` reused any `provision.json` with a matching `homeserver_url` string, and `load_or_init_state` reused any `embedded_state.json` that was on disk — URL equality and file existence were treated as proof that the homeserver DB hadn't changed. They aren't.

**Symptom (reported by user):** shore-matrix started cleanly, provisioned a fresh admin account, discovered one character, and immediately emitted `failed to set display name: M_UNKNOWN_TOKEN` → crypto recovery errors → fatal sync error. No rooms visible in the user's Matrix client; DMs to the character bot got no response. Root cause: the `matrix-server/` dir had been deleted between runs, so the RocksDB was freshly created — but the per-character `provision.json` at `<data_dir>/<character>/matrix/provision.json` survived, carrying the access token and device_id of a user the new DB had never heard of. Everything 401'd cascadingly; the sync loop died, so `on_room_message` never fired, so DMs silently dropped.

**Fix:** three runtime liveness probes, in `shore_matrix::provision`:
1. `check_token(url, token)` → `TokenStatus::{Valid, Invalid, Unknown}` via `GET /_matrix/client/v3/account/whoami`.
2. `check_room_exists(url, room_id, token)` → `RoomStatus::{Exists, Gone, Unknown}` via `GET /rooms/{id}/state/m.room.create`.
3. `wipe_character_state` / `wipe_embedded_state_and_characters` for recovery.

Wired in at three points in `run_embedded`:
- After `wait_for_healthy`: whoami the admin token. On `Invalid`, wipe `embedded_state.json` + every character's matrix/ subdir (preserving the `registration_token` so re-registration hits the already-running homeserver), force `first_run = true`.
- Inside `provision_character`: whoami the character token. On `Invalid`, wipe that character's `provision.json` + `crypto_store` and fall through to fresh register.
- Before the create-room loop: probe each surviving `state.room_id`. On `Gone` (404), clear it so the next branch recreates the room.

On `Unknown` (network blip, 5xx), **no state is destroyed** — the error bubbles up. We never wipe on ambiguous signals; a transient blip must not trigger a reprovision cascade.

**Why whoami and not an on-disk DB fingerprint:** Fingerprinting (writing a DB UUID into every `provision.json`) was the alternative. Rejected because (a) it would require every shore-matrix version to embed the fingerprint in `provision.json` before it was useful, meaning a DB wipe before the field existed is undetectable; (b) conduwuit/continuwuity/tuwunel don't expose a stable DB identifier via the client API; (c) the whoami round-trip is one cheap HTTP call per startup, amortized across session lifetime. Runtime probing is self-healing from arbitrary on-disk drift — not just the DB-wipe case we've seen, but also token revocation, server-side account deletion, etc.

**Sacrificed:** First-run-after-fix is not instant: for each existing character the bridge pays ~3 round trips (whoami, re-register, join-room) before the first message flows. Acceptable; it's bounded and happens once.

**Related fix:** Dropped `database_backend = "rocksdb"` from the generated homeserver config — tuwunel rejects it as an unknown parameter (conduwuit/continuwuity defaulted to rocksdb anyway). Removes a noisy startup warning.

**Verification:** 23 shore-matrix unit + integration tests pass (updated two TOML-contents assertions). Live auto-heal verification against the user's current broken profile is pending — the running `/usr/bin/shore-matrix` is a pre-fix system install; verification requires either a release cut via `/deploy` or stopping the systemd unit and running the dev binary directly against the same data dir.

### 2026-04-18 — `memory compact` accepts an optional `keep_turns` override

`shore memory compact`, the TUI `:compact` slash command, and the
`memory_compact` MCP tool all gained an optional positional argument that
overrides `CompactionConfig.keep_recent_turns` for a single call. The override
flows through the SWP `compact` command as `keep_turns: u32`.

- `shore memory compact`     → use the configured default (unchanged behavior)
- `shore memory compact 8`   → retain the last 8 user turns
- `shore memory compact 0`   → run the full pipeline (LLM recap, DB entry
  persistence, vector indexing, archive of old segment) but leave
  `active.jsonl` empty afterwards. The recap becomes the only carry-forward
  context for the next turn.

**Why a positional, not a `--reset` flag:** It's the same code path with a
different value — `keep_turns_override: Option<usize>` flowing into
`CompactionManager::compact`. A `--reset` flag would have been a synonym for
`compact 0`, adding surface area for no functional gain.

**Why `keep_turns = 0` doesn't skip the compaction LLM call:** The motivating
use case is editing the system prompt or character config without paying for
a long post-compaction prompt write. Persisting the recap and memory entries
is precisely what makes that cheap — skipping them would lose the
conversational continuity that the user is paying the LLM call for.

**Sacrificed:** The CLI now needs an additional `clap` enum variant field, and
existing in-tree call sites of `CompactionManager::compact` had to add `None`
for the new parameter. ~13 callsite touches in the test suite, no functional
change to the auto-compaction (background-trigger) path.

### 2026-04-18 — Token-based compaction trigger (`max_context_tokens`)

`CompactionConfig` gains a `max_context_tokens: usize` field (default `0` =
disabled) that forces compaction when the just-completed turn's prompt context
— `input_tokens + cache_read_tokens + cache_creation_tokens` — reaches the
threshold. The `min_turns` floor still applies. The check runs alongside the
existing `max_turns` trigger in `AutonomyManager::should_compact_now`, which
now takes `context_tokens: usize` as a third parameter.

**Why**: Turn count is a poor proxy for context cost because per-turn content
varies by an order of magnitude (heavy-thinking/tool-use turns vs. light chat).
A 30-day ledger simulation showed the per-call cost curve has an elbow around
30K context — median cost roughly doubles above that. A token-driven trigger
at 30K saved ~3.3% on typical days and ~8% on heavy-thinking spike days in
simulation. `max_turns` alone can't capture this because a user can stay under
their turn limit while each turn grows large.

**Default is 200000** — the typical context-window ceiling for Claude 4.x.
This makes the trigger act as a blow-out-the-window safety net by default
rather than a cost optimiser: conversations that stay below the model's
context limit never see a behaviour change, while runaway contexts get
compacted before they hit the wall. Lowering to ~30000 is the
cost-optimisation setting for Opus 4.7 (the per-call cost elbow), but that
is empirical/model-specific, so we don't impose it as the default.
Users tune with `shore usage --export-csv`. Setting `0` disables the
trigger entirely.

**Why use actual usage instead of estimating pre-send**: The measurement
available at the trigger point (`result.usage` from the just-completed
`StreamResult`) is the same quantity the ledger records and the simulation
used. Running the prompt-length heuristic from `engine/prompt.rs` would
introduce a second (less accurate) definition of context size and drift from
what the ledger shows.

**Sacrificed**: `should_compact_now`'s signature is now three-arg — every test
caller had to be updated to pass `0` when it's testing the turn-based or idle
path. Tolerable: 7 test callsites, all in one file.

### 2026-04-18 — Strip prior-turn thinking blocks from outgoing history

Signed `thinking` / `redacted_thinking` content blocks from completed prior
turns are now removed when serialising message history for an outgoing API
request. The in-progress tool-use loop (messages appended by
`engine/tools.rs` or `autonomy/manager.rs` mid-turn) is unaffected — those
go through a different serialisation path, and Anthropic's API requires
thinking blocks to persist across iterations of the same tool loop.

**Behavior is governed by a new `[memory.thinking] preserve_prior_turns`
config (default `false` = strip)**. Users who want the pre-2026-04-18
behavior can set `preserve_prior_turns = true`.

**Why**: Anthropic's Claude 4.x models do not attend to prior-turn
thinking (confirmed by live testing on 2026-04-17). Re-sending those
blocks on every subsequent request just consumed input / cache tokens.
Measured on 2026-04-17: ~$1.12 of pure waste from re-sending ~10.7K
accumulated thinking tokens across 94 calls. The fix applies at two
serialisation points: `handler/task.rs` when building the initial request,
and `handler/persistence.rs` when building the `last_request` snapshot
the autonomy/interiority subsystem uses. Both strip the same way so the
cache prefix stays consistent across user-turn and autonomy-tick paths.

**Cache invalidation trade-off**: switching stripping on causes one-time
cache misses on prefixes that were previously cached with thinking
blocks. After that single boundary, the new stripped prefix is stable
and caches hit normally. Live-verified on 2026-04-18 (sonnet-test,
reasoning_effort=high): after a thinking response, the next turn's
outgoing request contained zero thinking blocks in history while
`cache_read` matched the new prefix size cleanly. The stored engine
messages still carry the thinking blocks — display and storage are
unchanged; only outbound serialisation strips.

**Why not strip at the `content_block_to_api_json` call level instead**:
that function is also called by the tool-loop continuation path
(`engine/tools.rs:115`), where thinking blocks must be preserved for the
in-progress loop. Filtering inside the conversion would require a
context parameter threaded through every call site. Operating at the
JSON-array level (`strip_thinking_from_assistant_history`) cleanly
separates the history case from the mid-loop case — the mid-loop path
appends new content directly to `request.messages` and never touches
the strip helper.

**Sacrificed**: one extra boolean parameter on `persist_and_notify`. No
ledger/engine-store format changes; no wire protocol changes.

## Runtime `reasoning_effort` override via new SWP command (2026-04-19)

**Added**: `set_reasoning_effort` SWP command + `shore reasoning` CLI +
`:reasoning` TUI slash command. A per-session `Option<Option<String>>`
lives on `SessionState` / `CommandContext`: `None` = inherit from
config, `Some(None)` = force reasoning off, `Some(Some(v))` = force the
given value. `handle_generation` clones the resolved `ResolvedModel`
and patches `.reasoning_effort` before `build_request` reads it.

**Why a new command rather than extending `config_set`**: `config_set`
mutates global fields (`defaults.model`, `autonomy.enabled`) and has a
focused allowlist. `reasoning_effort` is per-model, not global — its
effect depends on which model is active, and a clean "override this
field for this session, regardless of which model is selected next"
contract is simpler to reason about than layering per-model writes onto
the global key/value setter.

**Why session-level override, not config-file write-through**: the knob
is an exploratory quality/latency trade-off users flip several times per
session. Persisting it to TOML on every toggle would rewrite the config
file repeatedly and conflict with the user's hand-edited baseline. The
override is ephemeral on the daemon side; CLI adds a thin `active_reasoning_effort`
state file that mirrors `active_model` so one-shot invocations don't lose
the setting between `shore send` calls.

**Why the "off" sentinel**: the override has three states (inherit /
force-on / force-off) but the daemon protocol is JSON. `{"value": null}`
means force-off; string value means force-on. The state file encodes
force-off as the literal string `"off"` — same file format, same
parsing, no wire ambiguity. Synonyms (`none`, `disable`, `disabled`,
`unset`) are accepted on input for ergonomics.

**Sacrificed**: `reasoning_effort_override` field now threaded through
five constructor sites (`CommandContext`, `SessionState`,
`GenerationParams`, + 2 dispatch copy points). Considered wrapping in a
struct to reduce the diff but rejected — the field count on these
structs is already large and the `None` default is uniform.

## Interiority Recap Persistence in active.jsonl (2026-04-19)

**Decision:** Interiority tick recaps (the character's `<recap>` block) are
now persisted as `Role::System` messages in `active.jsonl` at tick completion,
not re-injected ephemerally from a `recaps.jsonl` sidecar each turn.

**Why:** Two linked bugs in the prior ephemeral-injection design:

1. **Autonomous-ping gating.** `trim_messages` advanced `prev_ts` on every
   message regardless of role. When an interiority tick produced an autonomous
   ping (assistant message), the tick's recap — timestamped before the ping —
   fell outside the gap measured between the ping and the next user message,
   and was silently dropped.
2. **Non-persistence across compaction.** Even when recaps surfaced on a given
   turn, once surrounding messages were compacted into a segment file the
   tick timestamps no longer fell between any *retained* message pair, and
   the recap vanished from future payloads forever. `recaps.jsonl` still had
   the entry, but it was orphaned.

Both collapse into one fix: write the recap as a real message at tick
completion, use `MessageStore::insert_by_timestamp` to splice it into the
correct chronological position (the tick races with the handler, so the user
message may already be present), and delete the ephemeral injection from
`trim_messages` entirely. Recaps are now first-class history — archived into
segments by compaction like any other message, and they persist naturally.

**Trade-offs:**
- `recaps.jsonl` is retained as a sidecar log for `tick_id` correlation
  in debugging, but is no longer read by the payload path.
- The interiority prompt's `{recent_thread_block}` now reads the three most
  recent `Role::System` messages from `active.jsonl` (previously read
  `recaps.jsonl` directly). Single source of truth.
- On the wire, recaps reach the model via `convert_inline_system_messages`
  (Anthropic path) or the new mid-history `<system_instruction>` wrap
  (OpenAI/OpenRouter path). The wrap is defensive for backends that reject
  raw `role:"system"` mid-conversation.

## Prompt Overhaul: Stance/Mechanics Split (2026-04-19)

**Decision:** Refactored the capabilities system block and tool descriptions
to follow Anthropic's documented split between "what the tool does" (tool
description) and "when/why to reach for it" (system-prompt stance).

**Why:** Capability bullets were restating tool mechanics already present in
the `tools` array schema, and tool descriptions carried behavioral coaching
("Don't wait to be asked", "A failed lookup costs nothing") that tried to
shape posture from inside the decision-time reference. Per Anthropic's
courses repo, tool descriptions should stay 1–2 sentences of pure contract
(with tool-intrinsic preconditions like "Only orders that are 'processing'
can be cancelled"), while the system prompt carries persona, global policy,
and stance. Overlapping instructions dilute the model's attention and waste
tokens on every request.

**Result:**
- **Capabilities block** (`engine/prompt.rs:build_capabilities_block`)
  restructured into markdown-style sections (`### Memory database`,
  `### Interiority`, `### Image creation`, `### Web access`) with
  sub-bullets flowing under their parent section (saved images under Memory,
  scratchpad under Interiority). Bullets are pure stance — no tool names or
  parameters — with the exception of tools that have no other way to be
  surfaced (e.g. `generate_image`).
- **Tool descriptions** trimmed across the board. `set_next_wake` in
  particular was rewritten from a stance-heavy "express your own sense of
  pacing…continue any unfinished work" to a single mechanical line; the
  stance moved into the interiority system prompt (`build_interiority_prompt`),
  which only loads during ticks. `set_next_wake` is in the base tool set on
  every request for cache stability (see CHANGELOG:132), so its description
  is part of the cached prefix and should not carry tick-specific posture.
- **Template rendering extended** to the capabilities block and tool
  descriptions via `render_template` + a new `render_tool_defs` helper in
  `shore-daemon/src/tools/mod.rs`. `{{user}}` and `{{char}}` now resolve in
  capability bullets and tool descriptions, letting the character's copy
  feel grounded in the actual user name. `{{date}}` / `{{time}}` must NOT
  appear in capability bullets — they'd bust the cache prefix.
- **Dropped capability bullets:** `activity_heatmap`, `roll_dice`,
  `check_time` (tool description self-sufficient); `remember_image`
  (moved to per-image annotation, see below).

**Trade-off — `remember_image` annotation:** The "when the user shares an
image, save it with context via remember_image" nudge was removed from the
capabilities block and bundled into the `[Attached image saved as: <path>]`
annotation emitted by `handler/images.rs:format_image_annotation`. The
instruction now appears exactly when an image has been attached, rather
than permanently in the cached system prefix when no image is in play.
Scopes token cost to events that matter, at the cost of slightly more
tokens per image-bearing message.

**Interiority prompt rewrite:** Ephemerality statement ("thoughts and tool
use are logged") was misleading — intermediate tick thoughts are never
persisted anywhere the character sees next tick, and tool-use ring-buffer
events are only for operator diagnostics. Rewrote to accurately describe
which surfaces persist (`<recap>`, `<sendMessage>`, scratchpad, memory)
vs. which are ephemeral (everything else), and to tell the character to
leave pointers in `<recap>` when scratchpad files or memory entries need
to be retrieved next session. Recap was also promoted from optional to
mandatory in the prompt text, matching the daemon's wrap-up enforcement.
