# Shore V2 — Intentionally Removed / Replaced

V1 features that were consciously not ported to V2, either because they were
replaced by better alternatives or because they don't fit the V2 architecture.

Add items here as decisions are made.

## Replaced by Better V2 Alternatives

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

## Not Needed

- **Reset subcommand** (5.11) — Not needed. Users can delete or archive
  the conversation file directly for a fresh start.

- **Connection error hints** (7.16) — Not worth the complexity. The error
  message from the OS is sufficient.

## Failed Concepts (not porting)

- **Interiority — journal writing** (2.4) — Failed concept in V1. Not porting.
- **Interiority — story writing** (2.5) — Failed concept in V1. Not porting.
- **Interiority scheduling** (2.6) — Depended on interiority. Not porting.
