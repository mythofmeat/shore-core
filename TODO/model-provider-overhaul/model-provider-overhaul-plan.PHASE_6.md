# Phase 6: Model visibility filtering / ignorelist

## Goal

Prevent providers like OpenRouter from flooding model lists with hundreds of irrelevant models.

Filtering should hide models from normal lists and pickers without deleting them from the discovery cache.

## Config shape

Support one of these approaches.

Preferred simple approach:

```toml
[providers.openrouter.discovery]
visibility = [
  "*",
  "!anthropic/*",
  "!openai/*",
  "!google/gemini-*",
]
```

Meaning:

```text
patterns are evaluated in order
matched hidden by default if pattern has no !
matched visible if pattern starts with !
last match wins
```

Alternative explicit ignore-only approach:

```toml
[providers.openrouter.discovery]
ignore = [
  "llama/*",
  "meta-llama/*",
  "mistralai/*",
  "nousresearch/*",
  "qwen/*",
  "*/free",
]
```

If both are implemented, document precedence clearly.

## Matching rules

Match against upstream provider model id.

Examples:

```text
anthropic/claude-sonnet-4.5
openai/gpt-4.1
google/gemini-2.5-pro
meta-llama/llama-3.1-405b-instruct
```

Gitignore-style desired behavior:

```text
* matches anything
anthropic/* matches Anthropic-routed OpenRouter models
!anthropic/* un-hides Anthropic models after a broader hide rule
last matching rule wins
```

Do not apply visibility filters to manual/static models by default.

Manual models are intentional and should remain visible unless a separate explicit hide flag exists.

## Command behavior

Normal model lists:

```text
shore model
list_models
model picker UI
shell completions
```

should hide ignored models.

Add an escape hatch:

```text
include_hidden = true
```

or CLI:

```text
shore model --all
shore model --hidden
```

## Validation

Automated tests:

* `llama/*` hides matching models.
* `*` followed by `!anthropic/*` shows only Anthropic models.
* Last match wins.
* Hidden models remain in cache.
* Hidden discovered models do not appear in normal `list_models`.
* Hidden models can appear with `include_hidden`.
* Manual static models are not hidden by discovery filters unless explicitly configured.

Manual test:

* Configure OpenRouter visibility to only show Anthropic/OpenAI/Google.
* Refresh discovery.
* Confirm normal model list is short.
* Confirm hidden model count is reported somewhere useful.

---
