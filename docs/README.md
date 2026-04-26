# Shore Knowledge Base

This directory is the repo-local system of record for agent and human context.
`AGENTS.md` is the entry map; this file is the index for deeper docs.

## Product And Behavior

- [GOALS.md](../GOALS.md): source of truth for why Shore exists.
- [FEATURES.md](../FEATURES.md): current user-visible behavior.
- [CONFIGURATION.md](../CONFIGURATION.md): config keys and examples.
- [CHANGELOG.md](../CHANGELOG.md): release history.
- [product-specs/index.md](product-specs/index.md): product-facing specs.

## Architecture And Decisions

- [ARCHITECTURE.md](../ARCHITECTURE.md): crate map, data model, and runtime flow.
- [DECISIONS.md](../DECISIONS.md): current architectural decisions.
- [dev-info/INVARIANTS.md](dev-info/INVARIANTS.md): correctness constraints.
- [dev-info/PROMPT_CACHING.md](dev-info/PROMPT_CACHING.md): cache-stability model.
- [dev-info/QUIRKS.md](dev-info/QUIRKS.md): known sharp edges.
- [design-docs/index.md](design-docs/index.md): design-doc catalog.

## Agent Harness

- [HARNESS_ENGINEERING.md](HARNESS_ENGINEERING.md): Shore's interpretation of
  OpenAI harness-engineering practice.
- [PLANS.md](PLANS.md): execution-plan policy and template.
- [RELIABILITY.md](RELIABILITY.md): deterministic and live validation loops.
- [SECURITY.md](SECURITY.md): boundary, tool, remote-access, and secret-handling notes.
- [QUALITY_SCORE.md](QUALITY_SCORE.md): domain quality grades and gaps.
- [exec-plans/README.md](exec-plans/README.md): active/completed plan index.
- [exec-plans/tech-debt-tracker.md](exec-plans/tech-debt-tracker.md): small cleanup backlog.
- [references/harness-engineering.md](references/harness-engineering.md): source summary.

## Development Surfaces

- [dev/mcp/README.md](../dev/mcp/README.md): MCP profile and tool surface.
- `dev/test-harness`: deterministic in-process daemon harness with mock LLM.
- `scripts/cache-tests`: prompt-cache and heartbeat experiments.
- `scripts/live-tests`: live provider smoke/autonomy tests.

## Freshness Rules

- Keep `AGENTS.md` short and link to deeper sources instead of expanding it.
- Update docs with behavior, config, architecture, or invariant changes in the
  same change that modifies the code.
- Promote repeated review feedback into a doc, test, lint, or harness check.
- Run `python3 scripts/harness-check.py` when docs, architecture, prompt
  assembly, memory, tool surfaces, or agent guidance change.
