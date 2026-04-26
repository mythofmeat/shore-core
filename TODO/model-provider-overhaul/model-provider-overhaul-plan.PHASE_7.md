# Phase 7: Merge discovered models with static model catalog

## Goal

Make discovered models first-class selectable chat models.

Manual model entries remain valid and can override discovered metadata.

## Effective catalog behavior

The daemon should build an effective model catalog from:

```text
static/manual model catalog
+ provider-discovered model cache
+ provider registry defaults
+ saved model preferences
```

## Conflict rules

If a manual static model and discovered model refer to the same provider/model_id:

```text
manual static config wins for explicit fields
discovered metadata fills missing metadata
saved preferences still override sampler settings
```

If a manual static model uses a short alias:

```toml
[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
```

Then the alias should remain usable:

```text
shore model sonnet
```

Discovered models may need generated display names or qualified names.

Suggested generated identifier:

```text
chat.<provider>.<sanitized_model_id>
```

But user preferences should still key by provider/model_id.

## Important UX requirement

A user should be able to:

```text
shore provider refresh openrouter
shore model anthropic/claude-sonnet-4.5
```

or:

```text
shore model openrouter:anthropic/claude-sonnet-4.5
```

without manually adding that model to TOML.

## Validation

Automated tests:

* Discovered model can be selected.
* Static alias can be selected.
* Static alias overrides discovered metadata.
* Saved sampler settings apply to discovered model.
* Character-specific saved sampler settings apply to discovered model.
* Hidden discovered models cannot be selected by ambiguous/simple list name unless explicitly allowed.
* Direct selection of a hidden model should either:

  * be allowed with a clear warning, or
  * fail with “model is hidden; use --include-hidden or unhide it”

Choose one behavior and test it.

---
