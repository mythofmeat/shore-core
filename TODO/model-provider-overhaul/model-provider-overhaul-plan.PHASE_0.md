# Phase 0: Baseline tests and architecture notes

## Goal

Create a safe starting point before changing behavior.

## Tasks

1. Run the existing test suite.
2. Note current behavior for:
   - `shore model`
   - `shore model <name>`
   - `shore reasoning`
   - `shore reasoning <value>`
   - `shore status`
   - one-shot `shore send --temperature ...`
3. Add a short design note in the repo, for example:

```text
TODO/provider-model-rework.md
````

Include:

* current static catalog behavior
* target provider registry behavior
* target preference merge order
* target key fallback behavior

## Validation

* Existing tests pass before implementation begins.
* The design note exists and reflects this phased plan.

---
