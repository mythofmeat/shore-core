# Documentation Overhaul — Design

**Date:** 2026-04-15
**Status:** Draft (awaiting user review)
**Scope:** User-facing documentation only (README + two new user-facing docs). Internal docs (`ARCHITECTURE.md`, `DECISIONS.md`, `QUIRKS.md`, `docs/specs/`) are out of scope.

## Motivation

From `TODO/TODO.md`:

> update *ALL* documentation — the readme in particular is very out of date and not explanatory enough. explain what the features are, why they exist, and how to use them and configure them

The current `README.md` (288 lines) names features without explaining them. Terms like *autonomy*, *interiority*, *heartbeat*, *compaction*, *collation*, *recap*, *dormant phase* appear without definition. The CLI reference, provider table, remote-access prose, and character-file deep-dive all live inline, making the README a kitchen sink rather than an entry point. The workspace has grown to 14 crates and ~5 binaries; the README lists 4.

## Audience

**Primary:** end users installing and operating Shore.
**Secondary:** future-you (maintainer aide-memoire) — brief asides permitted where a non-obvious tradeoff is load-bearing.

Not in scope: contributors reading internals, AI agents, evaluators deciding whether to adopt.

## Doc Set

```
/
├── README.md               # REWRITTEN: quick-start + "what Shore is" (~200 lines)
├── docs/
│   ├── FEATURES.md         # NEW: per-feature what/why/how (user-facing)
│   ├── CONFIGURATION.md    # NEW: config prose — purpose & tradeoffs per section
│   ├── ARCHITECTURE.md     # UNTOUCHED (internal)
│   ├── DECISIONS.md        # UNTOUCHED
│   ├── QUIRKS.md           # UNTOUCHED
│   └── specs/              # UNTOUCHED
├── examples/
│   ├── config.toml         # REMAINS canonical annotated option reference
│   ├── client.toml         # unchanged
│   └── models.toml         # unchanged
└── shore-mcp/README.md     # UNTOUCHED (already current, debug-only)
```

**One-way links.** Markdown docs link *into* `examples/config.toml` for the full option list; `config.toml` links nowhere. `FEATURES.md` links *into* `CONFIGURATION.md` for tunables; `CONFIGURATION.md` doesn't link back. No duplicated option tables across files.

## README — Outline

Target: ~200 lines. Rebuilt from scratch, not patched.

1. **What Shore is** (3–4 paragraphs)
   - Persistent AI character engine, not a chat wrapper
   - Runs as a daemon; CLI / TUI / Matrix clients connect to it
   - Memory persists across sessions; character can speak on its own (autonomy/interiority) when configured
   - Multi-provider (Anthropic, OpenAI-compat, Gemini, DeepSeek, ZhipuAI, xAI)

2. **A day of use** — one short narrative paragraph making the abstract features feel concrete.

3. **Prerequisites** — Rust 1.75+, SQLite headers, Linux.

4. **Install** — `cargo build --workspace --release`; which binaries are produced.

5. **Quick start** — set API key, minimal `config.toml`, create a character (with `shore character --new` shortcut), start daemon, send message.

6. **What's next** — navigation block linking `docs/FEATURES.md`, `docs/CONFIGURATION.md`, `docs/ARCHITECTURE.md`.

7. **Tests & linting** — one line each.

8. **License.**

**Removed from current README** (moves to other docs):
- Full CLI reference table → `FEATURES.md#clients`
- Provider table → `CONFIGURATION.md#chat-provider-alias`
- `client.toml` / remote-access prose → `CONFIGURATION.md#client-toml`, `CONFIGURATION.md#daemon`
- Template-variable table + character-files deep dive → `FEATURES.md#characters`
- Daemon startup precedence block → `CONFIGURATION.md#orientation`
- Platform notes → relevant sections of `FEATURES.md` / `CONFIGURATION.md`

## FEATURES.md — Outline

Target: ~500–700 lines. Every section follows: **What it does** (1–2 paragraphs) → **Why it exists** (1–2 sentences on the problem) → **How to use it** (runnable commands + pointer to relevant `CONFIGURATION.md` anchor).

Section order (deliberate — core concepts before derived ones):

1. **Characters** — discovery, `character.md`, `user.md`, `prompts/system.md`, template variables, switching.
2. **Models and providers** — multi-provider support, per-operation model selection (`tool_model`, `compaction`, `interiority`, `memory_agent`, `collation`, `embedding`, `image_generation`), runtime overrides.
3. **Conversations** — `send`, `regen` / `regen --guidance`, `log` family (tail, follow, edit, delete, single-ref), images (`-i`), extended thinking (`--thinking`).
4. **Memory** — vector + FTS RAG (in plain language), compaction, collation, changelog, reindex, purge, memory-agent shell.
5. **Autonomy** — heartbeat probes, session gaps, dormant/active phases, `personality` dial, `max_unanswered` backoff.
6. **Interiority** — private ticks, recaps, tool rounds during a tick, wrap-up behavior, self-scheduled cadence.
7. **Tool use** — tool surface explained one by one: `memory`, `web_search` (Tavily), `fetch_url`, `check_time`, `roll_dice`, `activity_heatmap`, image tools (`send_image`, `list_images`, `recall_image`, `generate_image`). Per-tool toggles.
8. **Clients**
   - 8.1 **CLI** — full command reference (replaces README's current table).
   - 8.2 **TUI** — what it adds over CLI.
   - 8.3 **Matrix bridge** — setup, embedded homeserver, user registration.
9. **Prompt caching** — what it does for cost, provider pinning, `cache_ttl`, cache forensics opt-in.
10. **Diagnostics** — `shore status --diagnostics`, API call log, token/cost accounting.
11. **Remote access** — localhost-only default, opt-in TCP, allowlist, Tailscale caveat, no-TLS warning.
12. **Shell completions** — `shore completions <shell>`.

Every term defined on first use. No architectural jargon. No crate names.

## CONFIGURATION.md — Outline

Mirrors `examples/config.toml` layout. Every section follows: **Purpose** (1–2 sentences) → **When to change it** (tradeoffs) → **Small targeted example** → **Pointer to `examples/config.toml` for every option**.

Section order:

1. **Orientation** — where config lives (`$XDG_CONFIG_HOME/shore/`), `include = [...]` + `conf.d/*.toml` loading, precedence file/env/CLI, daemon startup precedence (`--config` → `SHORE_ADDR` → `[daemon].addr`).
2. **Environment variables** — API keys per provider, `SHORE_CHARACTER`, `SHORE_ADDR`, Tavily, etc. One grep-friendly table.
3. **`[daemon]`** — bind address, remote-access opt-in, allowlist.
4. **`[defaults]`** — default model, per-operation model slots, `display_name`, `stream`.
5. **`[behavior.autonomy]` + `.heartbeat` + `.interiority`** — grouped; explains which dials affect which behaviors.
6. **`[behavior.tool_use]`** — per-tool toggles, max iterations, `[behavior.tool_use.search]` (Tavily).
7. **`[memory.compaction]` + `[memory.collation]`** — idle triggers, thresholds.
8. **`[chat.<provider>.<alias>]`** — model configuration: provider keys, SDK, API key env var, per-model options (`temperature`, `max_tokens`, `max_context_tokens`, `reasoning_effort`, `budget_tokens`, `cache_ttl`).
9. **`[advanced]`** — cache forensics, other opt-ins.
10. **`client.toml`** — remote daemon address; precedence; daemon-side `[daemon]` requirements for non-loopback binds.

Does not duplicate `examples/config.toml`'s full annotated option list. Prose explains *why* and *when*; the example file covers *what every option is*.

## Content Style & Conventions

These apply to all three user-facing docs so they feel like one coherent set.

1. **WHAT → WHY → HOW, always in that order.** Users who already know the WHAT can skip ahead; users who don't get oriented first.
2. **Write to someone who has never used Shore.** No assumed familiarity with *interiority*, *recap*, *compaction*, *collation*, *dormant phase*. Define on first use in `FEATURES.md`.
3. **No internal architecture leakage.** User-facing docs don't mention SWP, crate names, module boundaries, or daemon state ownership. Implementation details surface only when they affect operation.
4. **Name every config key explicitly.** If a feature is governed by `behavior.autonomy.interiority.fallback_interiority_interval`, that exact string appears. Docs must be grep-friendly.
5. **Code blocks are runnable.** `shore send "hello"`, not `shore send <message>`.
6. **Aide-memoire asides allowed but rare.** A brief italicized note where future-you would otherwise be surprised. Not more than a handful across the doc set.
7. **One-way links** between docs (see Doc Set).
8. **Cross-doc links use stable anchors**, not line numbers.
9. **No emojis.**
10. **Linux-only in practice.** Docs state Linux as the supported platform without hedging. Speculative non-Unix support isn't carried forward.

## Maintenance

1. **User-facing behavior changes update user-facing docs.** Parallel to `CLAUDE.md`'s rule for `ARCHITECTURE.md`: any change to commands, config keys, feature toggles, or defaults updates `README.md` / `FEATURES.md` / `CONFIGURATION.md` in the same PR. Aide-memoire for future-you, not a policy enforced externally.
2. **`examples/config.toml` is canon for options.** New config keys land there first, with a comment. Markdown docs explain only where tradeoffs warrant discussion.
3. **`CHANGELOG.md` stays separate.** Per-release history lives there; user-facing docs describe current behavior.
4. **`docs/DECISIONS.md` stays separate.** Architectural "why" for contributors; FEATURES.md's "why" is the user's why — what problem the feature solves for them.
5. **No doc-generation tooling.** Hand-written. A CLI-reference generator or config-schema exporter is overkill for this scale and introduces a new thing to maintain.
6. **When in doubt, delete.** Stale paragraphs are worse than missing ones.

## Out of Scope

- Per-crate READMEs (library crates aren't user-facing; `shore-mcp/README.md` stays as-is because it's debug-only documentation for a specific tool)
- `CLAUDE.md` (agent directives, separate purpose)
- `CHANGELOG.md` (separate concern)
- Internal docs under `docs/specs/`, `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/QUIRKS.md`

## Success Criteria

1. A user landing on the repo page can, without reading anything else, understand what Shore is and whether they want to install it.
2. A user who follows the README's Quick Start reaches the point of successfully sending a message to a character.
3. Every feature named in `config.toml` or any CLI command is explained somewhere in `FEATURES.md` or `CONFIGURATION.md`.
4. Every term used in `FEATURES.md` is either common knowledge or defined on first use in the same doc.
5. Any config key a user might want to change is reachable via grep against the three markdown files *or* `examples/config.toml`.
