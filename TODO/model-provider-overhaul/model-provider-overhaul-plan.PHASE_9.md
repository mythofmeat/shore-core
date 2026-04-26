# Phase 9: Documentation and migration

## Goal

Document the new model/provider system and provide a gentle migration path.

## Docs to update or add

Add/update:

```text
docs/CONFIGURATION.md
docs/MODELS.md
docs/PROVIDERS.md
examples/config.toml
```

If the docs directory does not currently exist on `dev`, create appropriate files or update the closest existing documentation.

## Document

Explain:

```text
provider registry
single key config
multiple key config
budget/overflow key pattern
model discovery
manual model fallback
model visibility filters
per-model sampler persistence
per-character model preferences
merge order
one-shot overrides
```

## Example config

Include a practical OpenRouter example:

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
visibility = [
  "*",
  "!anthropic/*",
  "!openai/*",
  "!google/gemini-*",
]

[defaults]
model = "openrouter:anthropic/claude-sonnet-4.5"
```

Include manual fallback example:

```toml
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
temperature = 0.8
top_p = 0.95
```

## Migration behavior

If possible:

* Existing static model configs should require no migration.
* Existing CLI runtime active model files can be read once as a migration fallback.
* After the daemon writes the new preference file, the old runtime model state should no longer be needed.
* Do not delete old runtime files automatically unless there is already a clear cleanup pattern.

## Validation

* Docs match implemented config names.
* Example config parses in a test.
* Existing config examples still parse.
* New config examples parse.
* Migration fallback test, if implemented.

---
