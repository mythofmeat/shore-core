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

- **`shore cache suppress/unsuppress`** (5.48/5.49) — Removed. Subsumed by
  `shore config cache_keepalive.enabled true/false` (5.41).

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

- **Interiority — journal writing** (2.4) — Failed concept in V1. Not porting.
- **Interiority — story writing** (2.5) — Failed concept in V1. Not porting.
- **Interiority scheduling** (2.6) — Depended on interiority. Not porting.

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

### OpenRouter proxy removed from Anthropic SDK (2026-04-01)

**Decision:** The Anthropic SDK (`sdk = "anthropic"`) no longer supports custom `base_url`. Setting one is a runtime error with a message pointing to the `openrouter` SDK. Localhost is exempted for unit tests.

**Changes made:**
- Removed `base_url()`, `is_native_anthropic()`, and Bearer auth fallback from `anthropic.rs`
- Removed OpenRouter `provider` routing block from `build_body()`
- Removed `strip_thinking_from_prior_assistants()` — the Anthropic API handles thinking block stripping internally (confirmed via live testing with adaptive thinking on direct Anthropic)
- Added race condition guard in `execute_keepalive_ping()` — re-checks keepalive state under the lock before sending to prevent stale pings when a concurrent handler transitions state

**Why:** A/B testing with identical request bodies showed OpenRouter intermittently drops prompt cache hits even with static, never-changing system prompt breakpoints and 1h TTL. Direct Anthropic API gets 100% cache hits with the exact same code. Client-side thinking stripping was also unnecessary — the API strips prior-turn thinking internally and the cache key accounts for it. Supporting a proxy path that silently degrades caching is worse than not supporting it.

**Trade-off:** Users who were routing Anthropic models through OpenRouter must switch to using the `openrouter` SDK (which uses the OpenAI-compatible path). This is the correct approach anyway — OpenRouter's API is OpenAI-compatible, not Anthropic-compatible.
