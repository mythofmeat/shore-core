# Shore V2 — Quirks & Gotchas

Unexpected behavior, kludges, and idiosyncrasies that aren't obvious from reading the code. If you assumed something would work one way and it didn't, document it here.

## Provider Integrations

- **OpenRouter uses the OpenAI SDK path** (`Sdk::Openai`), not a dedicated provider. The `base_url` in hardcoded defaults is what routes requests to OpenRouter's API. If the base_url is missing or wrong, requests go to OpenAI instead — silently.

- **OpenRouter inconsistently forwards thinking signatures.** When proxying Claude's extended thinking, OpenRouter sometimes strips or fails to relay `signature_delta` SSE events. This means thinking blocks stored via OpenRouter may have `signature: null` even when the upstream model produced one. Shore handles this gracefully (signatures are `Option<String>`), but if Anthropic ever strictly requires signatures on cached thinking blocks in subsequent turns, this could break multi-turn thinking continuity through OpenRouter.

- **LLMs emit whitespace-only Text blocks before thinking/tool_use.** Claude (via OpenRouter) sometimes produces a `{"type": "text", "text": "\n\n"}` block before the thinking and tool_use blocks in a tool-loop response. The tool-loop merge must treat whitespace-only Text blocks as non-substantive — otherwise the merge predicate fails and tool results get orphaned.

- **OpenRouter intermittently drops prompt cache hits.** With identical, static, never-changing system prompt breakpoints and 1h TTL, OpenRouter returns `cache_read_tokens: 0` on ~30% of requests. The same requests against the direct Anthropic API get 100% cache hits. This is not deterministic — the misses appear random. Confirmed 2026-04-01 with controlled A/B testing. This is why the Anthropic SDK no longer supports `base_url` (i.e. proxying through OpenRouter).

## Anthropic API

- **Thinking blocks don't need client-side stripping.** The Anthropic API strips `thinking` and `redacted_thinking` blocks from prior assistant turns internally. Sending them intact does not affect the cache key — confirmed via live testing with adaptive thinking across multi-turn conversations including tool use. Pre-stripping on the client side is unnecessary and was removed.

- **Anthropic prompt cache has ~5s propagation delay.** After a cache write, identical requests within ~2s may miss. Requests after ~5s reliably hit. This is relevant for tool loops where the continuation call fires within ~1s of the initial call — the cache from the initial call won't be available yet, but this is expected and not a bug.

- **Cache breakpoints at depth 2 are sufficient.** Messages 1-3 of a conversation won't have cache breakpoints (not enough turns for depth-2 to exist), but once the breakpoint activates at message 4+, it works reliably across all subsequent turns including tool use exchanges. Multiple breakpoints at depths 4 and 8 were tested but provided no additional benefit on direct Anthropic.
