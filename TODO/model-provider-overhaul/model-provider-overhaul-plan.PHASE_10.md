# Phase 10: Final end-to-end test matrix

## Static-only setup

Config:

```toml
[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-5"
```

Verify:

```text
shore model
shore model sonnet
shore send "hello"
```

## Provider discovery setup

Config:

```toml
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "main"
env = "OPENROUTER_API_KEY"

[providers.openrouter.discovery]
enabled = true
```

Verify:

```text
shore provider refresh openrouter
shore model
shore model openrouter:anthropic/...
shore send "hello"
```

## Visibility setup

Config:

```toml
[providers.openrouter.discovery]
visibility = [
  "*",
  "!anthropic/*",
  "!openai/*",
]
```

Verify:

```text
normal model list is short
hidden models are not in completions
--all shows hidden models
```

## Per-model sampler persistence

Verify:

```text
select model A
set temperature 0.7
select model B
set temperature 1.2
select model A
confirm temperature 0.7
restart daemon
confirm model A still has temperature 0.7
select model B
confirm temperature 1.2
```

## Per-character preference persistence

Verify:

```text
character A selects model X with temperature 0.7
character B selects model Y with temperature 1.2
switch between characters
confirm each character keeps its own selected model/settings
restart daemon
confirm both persist
```

## API key fallback

Config:

```toml
[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BAD_OR_EXHAUSTED"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_VALID"
```

Verify:

```text
send message
warning appears
response succeeds with overflow key
send second message
warning appears again
first key was tried again
no secret values appear in logs or payload debug files
```

---
