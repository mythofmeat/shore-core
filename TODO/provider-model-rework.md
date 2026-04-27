# Provider/model rework — design note

Companion to `TODO/model-provider-overhaul/`. Captures the current shape of
Shore's model/provider plumbing on `feat/models-provider-overhaul` (forked
from `dev`/`main`) so subsequent phases can be measured against a fixed
baseline.

## Baseline test status (Phase 0 entry)

`cargo test --workspace` is green: ~1,450 tests pass, 10 ignored, 0 failed.
Largest groups: shore-daemon unit + suite (~610 tests), shore-tui (~205),
shore-llm (142), shore-config (49), shore-protocol (49). Phases 1+ must keep
this green except for tests we explicitly retire as behavior changes.

## Today: how model selection works

### Static catalog (`core/config/src/models.rs`)

- One section per model: `[chat.<provider>.<model>]`. Provider scalar keys
  cascade into model entries (`provider_config.fields.or_fallback(...)`).
- Hardcoded provider defaults in `hardcoded_defaults()` for `anthropic`,
  `openrouter`, `deepseek`, `gemini`, `xai`, `zhipuai`, `zai`, `nanogpt`.
  Sets sdk, default `api_key_env`, and (for non-Anthropic/Gemini) base_url.
- `ResolvedModel` is the single resolved record consumed everywhere
  downstream. ~20 optional sampler/wire fields plus required `model_id`,
  `sdk`, `provider_key`, `qualified_name`, `name`.
- `ModelCatalog::find_model` accepts both `chat.<provider>.<model>`
  qualified names and bare short names; ambiguous short names error.
- Categories: `chat`, `tools` (resolved), plus `embedding` and
  `image_generation` (still raw TOML).

### Active model + sampler runtime state

- Daemon side, per-character: `<character_data_dir>/runtime_state.json`
  containing `{active_model}`. Loaded at character attach in
  `handler/command_dispatch.rs`, written back after every command.
- Daemon side, per-session in memory: `reasoning_effort_override:
  Option<Option<String>>` (tri-state: unset / forced-off / forced-value).
  No durable backing.
- CLI side: flat files in `$SHORE_RUNTIME_DIR`:
  `active_character`, `active_model`, `active_reasoning_effort`. Mirror
  copies kept so a one-shot `shore` invocation can re-apply the choice on
  the next connect (`clients/cli/src/state.rs`, `run.rs:75-108`,
  `run.rs:360-433`).
- Result: model selection is durable per-character (daemon JSON), reasoning
  override is durable only via the CLI runtime mirror, and per-model
  sampler tweaks (temperature/top_p/budget_tokens) have no durable layer
  at all — they live in static config.

### LLM request building (`backend/llm/src/lib.rs`)

- `LlmClient::build_request(&ResolvedModel, ...)` reads exactly one env
  var (`model.api_key_env` falling back to `default_api_key_env(provider_key)`)
  into `LlmRequest.api_key`.
- `LlmRequest` is fully self-contained per call. No notion of credentials
  beyond the single string.
- `retry.rs` handles transient retries (5xx/429, network, IncompleteStream)
  and one optional `fallback_model`. No credential-failure classification,
  no key rotation.

### Generation path (`backend/daemon/src/handler/task.rs`)

- Resolves `active_model` from session state; falls back to
  `app.defaults.model`; finds it in the catalog; applies session
  `reasoning_effort_override`; calls `LlmClient::build_request`.

### CLI commands (current behavior)

- `shore model` — `switch_model {}`: prints active model.
- `shore model <name>` — `switch_model { name }`: validates against catalog,
  sets `ctx.active_model`, mirrors to CLI runtime file.
- `shore model --info [name]` — `model_info { name? }`: returns the
  serialized `ResolvedModel`.
- `shore model --reset` — `reset_model {}`: clears `active_model`; CLI
  removes its mirror file.
- `shore reasoning` — `set_reasoning_effort {}`: read-only; returns
  override + effective + config_default.
- `shore reasoning <value>` / `shore reasoning off` / `shore reasoning --reset`
  — `set_reasoning_effort { value | clear }`. CLI mirrors to runtime file.
- `shore status [--section X] [--diagnostics]`: dumps the daemon's status
  snapshot. Today the snapshot exposes `active_model` directly; no
  per-model sampler scope info.
- `shore send --temperature 0.8 --top-p 0.9 --thinking [N] ...`: one-shot
  per-message overrides applied at send time. They do not (and must not)
  mutate any durable state.

## Target shape (what each phase moves us toward)

### Provider registry (Phase 1)

New top-level `[providers.<name>]` table with: `enabled`, `sdk`, `base_url`,
optional compatibility `api_key_env`, repeated `[[providers.<name>.keys]]`
(name/env/enabled/warn_on_fallback), and `discovery.enabled`/visibility.
Existing static `[chat.<provider>.<model>]` and `hardcoded_defaults()`
entries continue to work; provider registry takes precedence for
connection/credential defaults but explicit per-model fields still win.

### Daemon-owned durable preferences (Phases 2–3)

- New files in the **data dir**:
  - `$SHORE_DATA_DIR/preferences/models.toml` (global)
  - `$SHORE_DATA_DIR/<character>/preferences/models.toml` (per-character)
- Schema: `[selected]`, `[defaults.sampler]`, and
  `[models."<provider>:<model_id>"]` keyed by **stable provider key + upstream
  model_id** (not display name / alias).
- Sampler keys carried per model: temperature, top_p, reasoning_effort,
  thinking_enabled, thinking_budget/budget_tokens, max_tokens?, cache_ttl?.
- Daemon becomes authoritative for active model AND reasoning AND per-model
  sampler. CLI runtime mirrors are kept for one release as a migration
  fallback, then become read-only.
- Sticky-per-model sampler invariant: switching A→B→A restores A's
  sampler exactly.

### Effective settings merge order

```
hardcoded provider defaults
  → provider registry defaults (Phase 1+)
  → static/manual model profile (today's [chat.X.Y])
  → discovered model metadata (Phase 5+)
  → global saved per-model preferences
  → character saved selected model + preferences
  → one-shot request overrides (--temperature etc.)
```

`build_request` in `backend/llm/src/lib.rs` patches the resolved model with
this stack just before the LLM call. One-shot overrides never write back.

### Multi-key fallback (Phase 4)

- Per-provider ordered `[[providers.X.keys]]`. Resolve enabled keys in
  order at request time.
- Try first key; on credential failure (Missing / Invalid / QuotaExhausted /
  BudgetExhausted / RateLimitedCredential), warn (no secret values) and
  retry the same request with the next key. Restart from key 1 on the
  next request — fallback is **never sticky**.
- Distinct from `retry.rs` transient retries: 5xx/429 without credential
  signals stays on the current key. Mid-stream failures do not rotate keys.
- Plumbing: extend `LlmClient::build_request` to take a candidate key
  list (or extract credential resolution from `build_request` entirely);
  add `CredentialFailureKind` classifier; surface a warning event over SWP
  for clients to render.
- Diagnostics record `(rid, provider, model, character, from_key_name,
  to_key_name, status, reason)` — never the secret.

### Discovery + visibility (Phases 5–7)

- Provider trait `ModelDiscoveryProvider::list_models(...)`. Initial impl:
  OpenAI-compatible `/v1/models` (covers OpenAI and OpenRouter shapes).
- Cache at `$SHORE_DATA_DIR/providers/<provider>/models.json`. Disk cache
  survives daemon restart. Discovery failures preserve previous cache.
- Visibility: gitignore-style `visibility = ["*", "!anthropic/*", ...]`
  matched against upstream model_id, last-match-wins. Hidden discovered
  models stay in the cache but are excluded from `list_models` /
  completions / pickers unless `include_hidden`. Static manual models are
  exempt by default.
- Effective catalog merges static + discovered: manual entries win
  explicit fields, discovered fills gaps, saved prefs override sampler.

## Critical constraints (from `model-provider-overhaul.md`)

- Daemon stays the source of truth for active model, sampler, and
  preferences.
- Manual `[chat.<provider>.<model>]` definitions remain a supported
  escape hatch indefinitely (custom endpoints, unreleased models, local
  inference, provider weirdness).
- Never log API key values, env var values, or the key itself in debug
  payloads or diagnostics.
- Key fallback is non-sticky.
- One-shot overrides do not mutate saved preferences.
- Per-model sampler must survive switches and daemon restarts.
- Per-character settings override global without damaging global.
- Hidden discovered models stay in the cache; do not delete them.
