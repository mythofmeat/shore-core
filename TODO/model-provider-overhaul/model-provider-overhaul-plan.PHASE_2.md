# Phase 2: Add daemon-owned durable model preference store

## Goal

Add a persistent preference layer in the data dir, but do not fully switch all behavior yet.

This phase introduces the storage and merge model.

## Storage location

Use the data dir, not the config dir and not the runtime dir.

Suggested files:

```text
$SHORE_DATA_DIR/preferences/models.toml
$SHORE_DATA_DIR/<character>/preferences/models.toml
```

If character data currently lives at `$SHORE_DATA_DIR/<character>/`, use that convention.

## Preference model

Support global preferences:

```toml
[selected]
provider = "openrouter"
model_id = "anthropic/claude-sonnet-4.5"

[defaults.sampler]
temperature = 1.0

[models.openrouter."anthropic/claude-sonnet-4.5"]
temperature = 0.8
top_p = 0.95
reasoning_effort = "medium"

[models.openrouter."google/gemini-2.5-flash"]
temperature = 1.2
top_p = 0.9
reasoning_effort = "off"
```

Support per-character preferences:

```toml
[selected]
provider = "openrouter"
model_id = "anthropic/claude-sonnet-4.5"

[models.openrouter."anthropic/claude-sonnet-4.5"]
temperature = 0.72
reasoning_effort = "high"
```

## Keying rule

Use stable provider/model keys:

```text
provider_key + upstream model_id
```

Examples:

```text
openrouter:anthropic/claude-sonnet-4.5
anthropic:claude-sonnet-4-5-20250929
openai:gpt-4.1
```

Do not key user preferences only by display name or short alias.

## Sampler/settings fields

Support at least:

```text
temperature
top_p
reasoning_effort
thinking_enabled
thinking_budget / budget_tokens
max_tokens, optional
cache_ttl, optional
```

Keep one-shot request overrides separate.

## Merge order

Implement an effective model/settings resolver with this order:

```text
hardcoded defaults
→ provider registry defaults
→ static/manual model profile
→ discovered model metadata, once discovery exists
→ global saved per-model preferences
→ character saved selected model/preferences
→ one-shot request overrides
```

For now, discovery metadata can be skipped because discovery does not exist yet.

## Behavior in this phase

* Add load/save code.
* Add tests for merge behavior.
* Do not yet remove current runtime active model behavior.
* Do not yet change CLI commands heavily.

## Validation

Add tests for:

* Missing preference files produce defaults.
* Global selected model loads.
* Character selected model overrides global selected model.
* Global per-model sampler settings apply.
* Character per-model sampler settings override global per-model settings.
* Switching from model A to model B and back preserves model A settings in storage.
* Unknown/malformed preference files produce clear errors or warnings.

---
