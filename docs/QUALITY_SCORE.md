# Quality Score

Last reviewed: 2026-04-27

Grades are operational signals for agents. Raise or lower them when evidence
changes; do not let this file become a trophy case.

| Area | Grade | Evidence | Known gaps |
| --- | --- | --- | --- |
| Product intent | A- | `GOALS.md`, `FEATURES.md`, `CONFIGURATION.md` are explicit and current | Product specs are still mostly index links rather than detailed specs |
| Daemon state model | A- | Daemon/client split, SWP protocol, protocol guardrail CI | More long-run crash/restart scenarios could move into deterministic harnesses |
| Markdown memory | A- | File-oriented memory docs, compaction/dreaming tests, prompt-visible `MEMORY.md` model | Live memory quality still depends on provider behavior |
| Prompt/cache stability | A- | Deferred protected edits, cache docs, cache scripts, ledger/cache forensics | Add more automated cache-script coverage to CI once credentials are not needed |
| Workspace tools/security | A- | Path sandboxing, memory gates, narrow `exec`, focused tests | Keep pressure on symlink and path-like argument edge cases |
| Agent harness | B+ | Short `AGENTS.md`, structured docs, harness checker, MCP and test harness | No full per-worktree observability stack yet |
| Clients | B | CLI/TUI/protocol tests cover core flows | GUI/Godot paths have less automated coverage |
| Matrix bridge | B | Bridge tests and clear client-bridge docs | Live homeserver verification remains external |
| Documentation freshness | B+ | Harness checker guards required docs, links, workspace member drift, conflict markers | Add markdown link validation and stale-doc ownership checks |

## Garbage Collection Queue

- Add a markdown link checker once docs stabilize.
- Add a recorded-provider fixture path for `shore-llm` streaming and cache
  behavior.
- Convert repeated cache-test shell flows into a CI-safe no-credential suite.
- Expand MCP-driven end-to-end examples with expected transcripts.
- Keep root docs and `docs/` split clean: root docs are canonical entry points;
  deeper operational detail belongs under `docs/`.
