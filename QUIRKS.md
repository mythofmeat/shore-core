# Shore V2 — Quirks & Gotchas

Unexpected behavior, kludges, and idiosyncrasies that aren't obvious from reading the code. If you assumed something would work one way and it didn't, document it here.

## Provider Integrations

- **OpenRouter uses the OpenAI SDK path** (`Sdk::Openai`), not a dedicated provider. The `base_url` in hardcoded defaults is what routes requests to OpenRouter's API. If the base_url is missing or wrong, requests go to OpenAI instead — silently.

- **OpenRouter inconsistently forwards thinking signatures.** When proxying Claude's extended thinking, OpenRouter sometimes strips or fails to relay `signature_delta` SSE events. This means thinking blocks stored via OpenRouter may have `signature: null` even when the upstream model produced one. Shore handles this gracefully (signatures are `Option<String>`), but if Anthropic ever strictly requires signatures on cached thinking blocks in subsequent turns, this could break multi-turn thinking continuity through OpenRouter.
