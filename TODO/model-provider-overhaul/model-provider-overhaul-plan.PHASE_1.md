# Phase 1: Add provider registry config without changing model behavior

## Goal

Introduce provider-level configuration as a first-class concept while keeping the existing static model catalog working exactly as before.

## Desired config shape

Support a new top-level provider registry:

```toml
[providers.openrouter]
enabled = true
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"

[providers.openrouter.discovery]
enabled = true
```

Also allow a compact compatibility form if it is easy:

```toml
[providers.openai]
api_key_env = "OPENAI_API_KEY"
```

But prefer the named-key form internally.

## Data model

Add new config structs, probably in `shore-config`, such as:

```rust
ProviderRegistryConfig
ProviderConfig
ProviderKeyConfig
ProviderDiscoveryConfig
```

Fields to support initially:

```text
provider key
enabled
sdk
base_url
api_key_env, for compatibility
keys[]
discovery.enabled
discovery.ignore / visibility rules, can be added later
```

Provider key entries should include:

```text
name
env
enabled, default true
warn_on_fallback, default false
```

## Behavior

* Existing `[chat.<provider>.<model>]` static models must still work.
* Existing hardcoded provider defaults must still work.
* Existing single `api_key_env` model/provider fields must still work.
* No discovery yet.
* No key fallback yet.
* No preference changes yet.

## Implementation notes

* `shore-config` currently parses model sections separately from app config.
* Add `providers` as another extracted top-level section before `AppConfig` parsing, or add it to `AppConfig` if that fits better.
* Preserve unknown-field validation for normal config.
* Decide how provider registry and existing model provider defaults merge:

  * provider registry should define connection/credential defaults
  * static model entries can still override SDK/base URL/api key env if explicitly configured

## Validation

Add tests for:

* Empty config still loads.
* Existing static model config still loads.
* New `[providers.openrouter]` config loads.
* Multiple `[[providers.openrouter.keys]]` entries load in order.
* Disabled keys are parsed but excluded from active credential resolution later.
* Existing `api_key_env` behavior remains unchanged.

---
