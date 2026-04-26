# Shore provider/model selection rework plan

## Goal

Rework Shore’s model/provider system so users can:

- Configure providers once, instead of manually defining every model.
- Discover available models from provider APIs.
- Keep manual model definitions as an escape hatch.
- Persist selected models and sampler settings without editing config or restarting.
- Preserve sampler settings per provider/model and per character.
- Hide unwanted discovered models with gitignore-style visibility rules.
- Configure multiple API keys per provider and try them in order on quota/budget/key failures.
- Show a visible warning when Shore falls back from a budget key to an overflow key.

This should preserve Shore’s daemon-first architecture: clients remain thin, and the daemon owns provider/model state, persistence, and request resolution.

---

## Current architecture assumptions

Before implementing each phase, inspect the current code on the `dev` branch.

Relevant areas:

- `shore-config/src/models.rs`
  - Existing static model catalog.
  - `ResolvedModel`.
  - Provider/model defaults.
- `shore-config/src/lib.rs`
  - Config loading, includes, `conf.d`, per-character config overlays.
- `shore-llm-client/src/lib.rs`
  - `LlmClient::build_request`.
  - Current single API key resolution.
- `shore-llm-client/src/providers/*`
  - Provider-specific request handling.
- `shore-llm-client/src/retry.rs`
  - Existing transient retry logic.
- `shore-daemon/src/handler/task.rs`
  - Generation path and active model resolution.
- `shore-daemon/src/handler/generation.rs`
  - Streaming retry path.
- `shore-daemon/src/commands/state/models.rs`
  - `list_models`, `switch_model`, `model_info`, `set_reasoning_effort`.
- `shore-daemon/src/commands/mod.rs`
  - Command dispatch.
- `shore-cli/src/cli.rs`
  - CLI command shape.
- `shore-cli/src/run.rs`
  - Current client-side active model / reasoning override persistence behavior.
- `shore-cli/src/state.rs`
  - Current runtime files for active character/model/reasoning.
- `shore-tui/src/*`
  - TUI model switching and completions, if present.
- `shore-protocol`
  - SWP request/response types if command responses need shared type changes.

---

## Non-goals for this rework

Do not try to fix usage/cost accounting in this implementation.

Usage tracking and pricing accuracy need separate work later. The multiple-key fallback feature should rely on provider-enforced budget/quota errors, not Shore’s local usage estimates.

Do not remove manual `[chat.<provider>.<model>]` model definitions.

Manual models remain supported as explicit overrides/fallbacks for custom endpoints, unreleased models, local providers, and provider API weirdness.

Do not make key fallback sticky.

Provider keys should be tried in configured order on every request. If the first key works again later, Shore should use it again automatically.

---

# Suggested implementation order summary

1. Baseline tests and design note.
2. Provider registry config structs.
3. Durable model preference store.
4. Move active model/reasoning/sampler state into daemon-owned preferences.
5. Multiple API key support with non-sticky fallback warnings.
6. Provider discovery cache.
7. Visibility filtering.
8. Merge discovered models into effective catalog.
9. CLI/TUI/GUI integration.
10. Docs, migration, and full end-to-end validation.

---

# Critical constraints

* Keep daemon as the source of truth.
* Keep clients thin.
* Preserve existing static model config behavior.
* Do not remove manual model definitions.
* Do not store or log API secrets.
* Do not make key fallback sticky.
* Do not use Shore usage/cost accounting for the budgeting warning in this feature.
* One-shot send overrides must not mutate saved model preferences.
* Hidden discovered models must remain in cache.
* Per-model sampler settings must survive model switches and daemon restarts.
* Per-character settings must override global settings without damaging global settings.
