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
