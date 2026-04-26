# Phase 8: CLI/TUI/GUI user-facing integration

## Goal

Expose the new system cleanly to users while keeping clients thin.

## CLI

Suggested commands:

```text
shore provider
shore provider refresh <provider>
shore provider models <provider>
shore provider models <provider> --all

shore model
shore model <name-or-provider-model-id>
shore model --info
shore model --reset
shore model --all

shore model setting
shore model setting temperature 0.8
shore model setting top_p 0.95
shore model setting reasoning_effort medium
shore model setting reasoning_effort off
shore model setting thinking_enabled false
shore model setting budget_tokens 4096
shore model setting --reset temperature
```

Keep compatibility:

```text
shore reasoning high
shore reasoning off
shore reasoning --reset
```

but internally route it through saved model settings.

## TUI

Add or update model picker behavior:

```text
provider list
model list for selected provider
refresh provider models action
show hidden/all toggle
current sampler settings display
edit sampler settings for selected model
warning area for API key fallback
```

## GUI

If GUI work exists, expose the same daemon commands.

Do not make GUI read/write preference files directly.

## Shell completions

Current dynamic model completion should use daemon `list_models`.

After visibility filtering, completions should only include visible models by default.

## Validation

Manual tests:

* CLI can refresh provider models.
* CLI model list respects visibility.
* CLI model switch persists after restart.
* CLI model settings persist after restart.
* TUI can select discovered models.
* TUI shows compact fallback key warnings.
* Existing simple static-config-only setups still work.

---
