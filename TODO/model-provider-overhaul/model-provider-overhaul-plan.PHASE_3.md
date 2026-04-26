# Phase 3: Move active model and sampler settings to daemon-owned preferences

## Goal

Replace session/runtime active model behavior with daemon-owned durable preferences.

This is the first visible behavior change.

## Commands

Evolve existing commands rather than adding unrelated command families too early.

Update or add daemon commands:

```text
list_models
model_info
switch_model
reset_model
set_model_setting
model_settings
set_reasoning_effort
```

Possible command behavior:

```text
switch_model { name | provider, model_id }
```

* Sets the selected model for the active character, unless a global flag is provided.
* Does not copy sampler settings from the previous model.
* Does not reset sampler settings for the target model.
* If the target model has saved settings, those settings become effective immediately.
* If the target model has no saved settings, fall back to defaults.

```text
set_model_setting { key, value, scope? }
```

* Applies to the currently active provider/model.
* Default scope should probably be the current character.
* Optional global scope can be added if useful.

Examples:

```json
{ "key": "temperature", "value": 0.8 }
{ "key": "top_p", "value": 0.95 }
{ "key": "reasoning_effort", "value": "medium" }
{ "key": "reasoning_effort", "value": null }
{ "key": "thinking_enabled", "value": false }
```

## CLI behavior

Update CLI so it no longer needs to persist active model/reasoning in `shore-cli/src/state.rs`.

Current runtime files can remain as migration fallback for one release, but the daemon should become authoritative.

Suggested CLI commands:

```text
shore model
shore model <name>
shore model --info
shore model --reset

shore model setting temperature 0.8
shore model setting top_p 0.95
shore reasoning high
shore reasoning off
shore reasoning --reset
```

Keep existing `shore reasoning` if possible, but make it persist through the same model preference store.

## Important behavior

Sampler settings are sticky per model.

Scenario to test manually:

```text
1. Select model A.
2. Set temperature = 0.7.
3. Select model B.
4. Set temperature = 1.2.
5. Select model A.
6. Confirm effective temperature is still 0.7.
7. Select model B.
8. Confirm effective temperature is still 1.2.
```

## Request path

Update generation resolution in `shore-daemon/src/handler/task.rs`:

* Resolve active character.
* Load effective model preference for that character.
* Resolve the selected model against the static catalog for now.
* Patch sampler/settings onto the resolved model before building the LLM request.
* Apply one-shot message overrides last.

## Status/model info

Update status/model info so users can understand where settings come from.

Include:

```text
selected provider
selected model_id
display name / qualified name
effective temperature
effective top_p
effective reasoning_effort
effective thinking budget
scope: global or character
whether the model came from static config or discovered cache, once discovery exists
```

## Validation

Automated tests:

* `switch_model` writes daemon-owned preferences.
* `set_model_setting` writes per-model settings.
* Settings survive daemon restart by reading/writing files.
* One-shot send overrides do not mutate saved model preferences.
* Existing static model configs still work.

Manual tests:

* `shore model <A>`
* set sampler settings
* `shore model <B>`
* set different sampler settings
* switch back and confirm A’s settings are restored
* restart daemon and confirm settings persist

---
