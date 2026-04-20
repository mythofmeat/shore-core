# Tool search + `defer_loading` — later problem

Anthropic's [tool search tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool) lets you mark individual tools with `defer_loading: true`, which keeps them out of the cached system-prompt prefix until Claude discovers them via a search. It's designed for large tool catalogs (30–50+ tools, or >10k tokens of tool definitions), and its stated benefits are (a) context bloat reduction and (b) tool-selection accuracy on large sets.

Shore has 16 tools right now — below the threshold where Anthropic recommends this, and below the threshold where selection accuracy is the bottleneck. Current decision: **skip for now**. This doc captures what adoption would look like and the open questions that block it.

## Why it's worth doing eventually

- Tool count will grow. Music library, filesystem, Jellyfin, Trakt, caco, MCP plugins, etc. (see `plugins_general.md`) — all plausible tool surface. At 30+ we're in the territory where tool search earns its keep.
- The hot-set argument independent of scaling: memory and web search are always useful; `roll_dice`, `activity_heatmap`, `check_time` are not. Less-frequently-used tools in the prefix dilute stance weight on the core tools. User's hypothesis is that shrinking the visible set increases the probability Claude reaches for the tools we actually want. Worth measuring.
- `defer_loading` preserves the cache prefix (deferred tools are appended inline as `tool_reference` blocks during the conversation; the cached tools/system prefix never changes). So the cost of deferring a tool that turns out to be relevant is one extra search round-trip, not a prompt-cache miss. Cheap downside.

## What adoption looks like on the Anthropic side

1. Per-tool `defer_loading: bool` field on `ToolDef` in `shore-daemon/src/tools/mod.rs`.
2. User-configurable override in `shore-config::app::ToolToggles` (or a sibling struct) so the defaults can be tuned per character without recompiling.
3. Sensible built-in default by `ToolCategory`: `Web`, `MemoryWrite`, `MemoryRead`, `Scratchpad` → visible; `Image` (non-memory), `Basic` novelty (`roll_dice`, `check_time`), `Activity`, `Interiority` (`set_next_wake`) → deferred.
4. Emit a `tool_search_tool_bm25_20251119` server-tool entry alongside user tools when any are deferred. BM25 over regex — natural-language queries fit a character-driven model better than `re.search()` patterns.
5. `render_tool_defs` in `shore-daemon/src/tools/mod.rs` passes `defer_loading` through to the outbound JSON.
6. `shore-llm-client/src/providers/anthropic.rs` passes it through unchanged — it's just a field on the tool dict.

None of that is hard. It's one config field, one provider-feature gate, and a category-based default table.

## The hard part: non-Anthropic providers

Shore speaks Anthropic's tool-definition shape internally (`shore-llm-client/src/providers/stream_helpers.rs:242` `translate_tool_declarations`) and each provider translates on the way out:

- **Anthropic** (`providers/anthropic.rs`): native — passes tools through as-is, and `defer_loading` works.
- **OpenAI-compat** (`providers/openai.rs`): wraps each tool as `{type: "function", function: <decl>}`. No `defer_loading` equivalent in the OpenAI spec. No tool-search server tool.
- **Gemini** (`providers/gemini.rs`): wraps as `[{functionDeclarations: <decls>}]`. Same situation — no deferral primitive.
- **Z.ai** (`providers/zai.rs`): presumably OpenAI-compat.

Anthropic's docs describe [custom client-side tool search](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool#custom-tool-search-implementation) — implementing a tool that returns `tool_reference` blocks. But that's still an Anthropic API-idiom; it doesn't help other providers.

### Open questions

1. **What's the fallback for non-Anthropic providers?** Three candidates:
   - **Send all tools.** Simplest. Lose the hot-set benefit on those providers. Acceptable if we consider Anthropic the primary target and others the fallback.
   - **Client-side pre-filter based on conversation content.** Shore inspects the last user message, picks a subset of tools to include, never sends the rest. Loses cache stability on those providers (every request has potentially-different tools). OpenAI-compat providers generally don't cache tool prefixes anyway, so this may be cheap.
   - **Two-call dance.** First call: describe the conversation and ask the model which tool categories are relevant. Second call: full request with just those tools. Expensive, not obviously better than (1) or (2).

2. **Per-provider capability flags.** Do we introduce something like `ProviderCapabilities { supports_tool_search: bool, supports_defer_loading: bool, supports_prompt_cache: bool }` in `shore-llm-client`? The client already differentiates behavior per-provider in ad-hoc ways; a unified capability surface would make feature-gating explicit rather than spread across `if provider == "anthropic"` checks in various call sites. Probably wants to happen regardless of tool search, as we add more provider-specific features.

3. **Where does the fallback logic live?** Three candidate homes:
   - `shore-llm-client`: each provider handles its own tool-list translation, including optional filtering. Keeps per-provider quirks in one place.
   - `shore-daemon` (call site): the daemon asks the client for capabilities, decides what subset to send. Cleaner separation but adds coupling.
   - A new `shore-tools` crate that owns tool definitions, category-based defaults, and the per-provider degradation strategy. Might be warranted if tool registration grows much further — right now tools live in `shore-daemon` but the registration pattern is ready to extract.

4. **Does `render_tool_defs` need to know the target provider?** Currently it renders Anthropic-shape tools and the provider translates. If deferral semantics differ per-provider, `render_tool_defs` either stays provider-agnostic (and providers strip `defer_loading` when unsupported) or becomes provider-aware (cleaner dispatch but leakier abstraction). Leaning toward the former — strip in the provider, keep the daemon's tool-building pure.

5. **What does the interiority tick mode do?** Interiority already uses a different tool set (or could); `set_next_wake` is canonically interiority-only. Deferral might let us leave `set_next_wake` visible always (matching today's validation-not-filtering design — see `project_shore_tool_scoping_validation_not_filtering` memory) without paying its token cost during normal chat. That's a nicer end state than the current "always visible, errors on misuse" approach.

6. **Observability.** `server_tool_use.tool_search_requests` is in the usage object per the docs. Do we surface this in `shore-ledger` as a separate call-type, or roll it into the owning request? Probably its own line item for diagnostics parity.

## Don't-forget list for when we actually pick this up

- Tool-search is not compatible with `input_examples` (per docs). We'd need to choose one or the other if we ever adopt input examples.
- All-tools-deferred returns a 400. Need at least one non-deferred tool. The hot-set design enforces this naturally but it's a footgun for config.
- `tool_choice` changes invalidate cached *message* blocks but not tools/system prefix. Non-issue for our use since we use `auto`.
- Model support per docs: Sonnet 4.0+, Opus 4.0+, Haiku 4.5+. All current Shore targets support it.
