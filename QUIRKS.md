# Shore V2 — Quirks & Gotchas

Unexpected behavior, kludges, and idiosyncrasies that aren't obvious from reading the code. If you assumed something would work one way and it didn't, document it here.

## Provider Integrations

- **OpenRouter defaults to `Sdk::Openai`** but can be overridden to `Sdk::Anthropic` per model (e.g. `sdk = "anthropic"` for Claude models). The `base_url` in hardcoded defaults routes requests to OpenRouter's API. If the base_url is missing or wrong, requests go to OpenAI instead — silently.

- **OpenRouter inconsistently forwards thinking signatures.** When proxying Claude's extended thinking, OpenRouter sometimes strips or fails to relay `signature_delta` SSE events. This means thinking blocks stored via OpenRouter may have `signature: null` even when the upstream model produced one. Shore handles this gracefully (signatures are `Option<String>`), but if Anthropic ever strictly requires signatures on cached thinking blocks in subsequent turns, this could break multi-turn thinking continuity through OpenRouter.

- **LLMs emit whitespace-only Text blocks before thinking/tool_use.** Claude (via OpenRouter) sometimes produces a `{"type": "text", "text": "\n\n"}` block before the thinking and tool_use blocks in a tool-loop response. The tool-loop merge must treat whitespace-only Text blocks as non-substantive — otherwise the merge predicate fails and tool results get orphaned.

- **OpenRouter intermittently drops prompt cache hits.** With identical, static, never-changing system prompt breakpoints and 1h TTL, OpenRouter returns `cache_read_tokens: 0` on ~30% of requests. The same requests against the direct Anthropic API get 100% cache hits. This is not deterministic — the misses appear random. Confirmed 2026-04-01 with controlled A/B testing. The Anthropic SDK now accepts custom `base_url` again (after the SDK/provider split), but be aware of this cache reliability issue when using OpenRouter with `sdk = "anthropic"`.

## Anthropic API

- **Thinking blocks don't need client-side stripping.** The Anthropic API strips `thinking` and `redacted_thinking` blocks from prior assistant turns internally. Sending them intact does not affect the cache key — confirmed via live testing with adaptive thinking across multi-turn conversations including tool use. Pre-stripping on the client side is unnecessary and was removed.

- **Interiority replaces heartbeat — config migration is breaking.** The `[behavior.autonomy]` section no longer accepts `personality`, `max_unanswered`, `max_deferral_hours`, or `[behavior.autonomy.heartbeat]`. Due to `deny_unknown_fields`, old config files will fail to parse. The persisted state file (`autonomy.json`) version bumped from 1→2; old heartbeat fields are silently ignored on load. The wire protocol command `heartbeat_log` is kept as-is to avoid breaking existing CLI versions — it returns `InteriorityEvent` data under the old name.

- **Anthropic prompt cache has minimum token thresholds.** Caching silently doesn't activate if the cached prefix is shorter than the model's minimum. Opus 4.6: 4096 tokens, Sonnet 4.6: 2048 tokens, Haiku 4.5: 4096 tokens. The API returns `cache_creation_tokens=0` and `cache_read_tokens=0` with no error. This is easy to miss in test configurations with short prompts — cache verification is meaningless if input tokens don't exceed the threshold.

- **Anthropic prompt cache has ~5s propagation delay.** After a cache write, identical requests within ~2s may miss. Requests after ~5s reliably hit. This is relevant for tool loops where the continuation call fires within ~1s of the initial call — the cache from the initial call won't be available yet, but this is expected and not a bug.

- **Cache breakpoints at depth 2 are sufficient.** Messages 1-3 of a conversation won't have cache breakpoints (not enough turns for depth-2 to exist), but once the breakpoint activates at message 4+, it works reliably across all subsequent turns including tool use exchanges. Multiple breakpoints at depths 4 and 8 were tested but provided no additional benefit on direct Anthropic.

## Image Handling

- **Anthropic 1h cache TTL pricing differs from OpenRouter's reported 5m prices.** OpenRouter's `/api/v1/models` endpoint reports cache_write prices for the 5-minute TTL. Shore uses the 1-hour TTL (configured via `cache_ttl = "1h"`), where cache_write costs are 2x input price (5-minute price is 1.25x input). The PricingEngine hardcodes this multiplier (`ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER = 1.6`). If Anthropic changes the relationship between TTL tiers, this multiplier needs updating.

## OpenRouter Pricing

- **OpenRouter `/api/v1/models/{id}` returns 404 for everything.** The per-model endpoint is dead (confirmed 2026-04-06). The only working endpoint is `/api/v1/models` which returns the full catalog. `PricingEngine::fetch_pricing` was rewritten to fetch the full catalog, scan for the target model, and bulk-cache all pricing data in one pass.

- **Anthropic model IDs use dots for minor versions on OpenRouter.** Shore stores model names as `claude-opus-4-6` (from Anthropic's API) but OpenRouter's catalog uses `claude-opus-4.6`. The `normalize_anthropic_model()` function converts the last `digit-digit` hyphen to a dot. This is fragile — if Anthropic releases a model with a hyphenated suffix that isn't a version number, it could be incorrectly normalized.

- **Anthropic 1h cache TTL pricing differs from OpenRouter's reported 5m prices.** OpenRouter reports cache_write prices for the 5-minute TTL. Shore uses 1-hour TTL, where cache_write costs are 2x input price (5-minute price is 1.25x input). Hardcoded as `ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER = 1.6`.

## Image Handling

- **User messages always have `content_blocks` populated.** This means the `build_content(text, images)` fallback in the LLM message builder is dead code for user messages — it only fires when `content_blocks` is empty. Prior to the fix, `m.images` was silently dropped for all user messages because the `content_blocks` branch didn't encode them. Image encoding must happen in both branches.
