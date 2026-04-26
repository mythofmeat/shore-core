# Phase 5: Provider model discovery and cache

## Goal

Add provider model discovery without changing the manual static model escape hatch.

Start with one provider family, preferably OpenAI-compatible `/models`, because it covers OpenAI-like APIs and OpenRouter-style APIs.

## Cache location

Use data/cache, not config.

Suggested location:

```text
$SHORE_DATA_DIR/providers/<provider>/models.json
```

or:

```text
$SHORE_CACHE_DIR/providers/<provider>/models.json
```

Prefer data dir if the cache is user-visible and useful across restarts. Prefer cache dir if it is purely disposable.

## Discovered model record

Suggested fields:

```text
provider_key
model_id
display_name
sdk
base_url
created_at, optional
owned_by, optional
description, optional
context_length, optional
max_output_tokens, optional
supports_tools, optional
supports_images, optional
supports_reasoning, optional
supports_prompt_cache, optional
raw_provider_metadata
discovered_at
```

Capabilities can be unknown.

Do not pretend unknown capabilities are false unless the provider clearly says so.

## Provider trait

Add provider discovery abstraction somewhere appropriate, likely `shore-llm-client` or a new module in `shore-daemon`.

Possible trait:

```rust
trait ModelDiscoveryProvider {
    async fn list_models(...) -> Result<Vec<DiscoveredModel>, DiscoveryError>;
}
```

Initial implementations:

```text
openai-compatible /v1/models
openrouter, if it needs special metadata parsing
```

## Commands

Add daemon commands:

```text
list_providers
refresh_provider_models
list_provider_models
```

Behavior:

```text
list_providers
  returns configured providers, enabled status, discovery enabled, key names, no secrets

refresh_provider_models { provider }
  calls provider API and updates cache

list_provider_models { provider, include_hidden? }
  returns discovered + manual models after visibility filtering
```

`list_models` should eventually return the effective chat-selectable model list:

```text
manual static chat models
+ discovered visible chat models
```

## Discovery auth

Use the first configured usable provider key for discovery.

If discovery fails on a key due to credential/quota failure, key fallback can apply here too, but keep it simple if needed.

## Validation

Automated tests:

* Cache file writes and reads.
* Discovery result merges with static models.
* Static/manual models remain available if discovery fails.
* Discovery failure does not delete previous cache.
* Provider with discovery disabled is skipped.
* Provider with no keys gives a clear error.

Manual tests:

* Configure OpenRouter.
* Refresh models.
* Confirm cache appears.
* Confirm `shore model` or equivalent can list discovered models.
* Confirm daemon restart still sees cached models.

---
