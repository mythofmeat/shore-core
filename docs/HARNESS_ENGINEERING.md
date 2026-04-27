# Harness Engineering Contract

Source: OpenAI, "Harness engineering: leveraging Codex in an agent-first world"
<https://openai.com/index/harness-engineering/>. Shore applies the article as a
repo-local operating contract, not as a requirement for zero human-written code.

## Principles

- Humans steer; agents execute through repo-local tools and feedback loops.
- Repository knowledge is the system of record. Context outside the repo does
  not exist for a future agent run.
- `AGENTS.md` is a table of contents, not an encyclopedia.
- Architecture and taste rules should be enforced mechanically where possible.
- Agent legibility matters as much as human legibility: tests, logs, MCP tools,
  scripts, and docs should let an agent validate its work directly.
- Entropy should be collected continuously through small cleanup tasks, not
  tolerated until it becomes a large rewrite.

## Shore Controls

| Practice | Shore control |
| --- | --- |
| Short agent entry point | [AGENTS.md](../AGENTS.md), mirrored from [CLAUDE.md](../CLAUDE.md) |
| Structured knowledge base | [docs/README.md](README.md), product specs, design docs, exec plans |
| Architecture map | [ARCHITECTURE.md](../ARCHITECTURE.md) plus workspace-member validation |
| Correctness invariants | [dev-info/INVARIANTS.md](dev-info/INVARIANTS.md) |
| Mechanical checks | `python3 scripts/harness-check.py`, CI agent-harness workflow |
| Deterministic feedback | `dev/test-harness`, focused cargo tests, protocol guardrail workflow |
| Agent-driven end-to-end path | [dev/mcp/README.md](../dev/mcp/README.md) |
| Reliability and live gates | [RELIABILITY.md](RELIABILITY.md), `scripts/live-tests`, `scripts/cache-tests` |
| Observability | [OBSERVABILITY.md](OBSERVABILITY.md), diagnostics, ledger, cache forensics |
| Security boundaries | [SECURITY.md](SECURITY.md), workspace-tool tests, daemon remote-access rules |
| Entropy tracking | [QUALITY_SCORE.md](QUALITY_SCORE.md), [exec-plans/tech-debt-tracker.md](exec-plans/tech-debt-tracker.md) |

## Required Agent Loop

1. Read `AGENTS.md`, then open only the linked docs relevant to the task.
2. Reproduce the issue or establish the current behavior with a focused test,
   MCP run, script, or code inspection.
3. Implement the smallest change that preserves the invariants.
4. Run focused validation first, then broaden to the affected crate/workspace.
5. Update docs, quality grades, decisions, or tech-debt notes when the change
   alters repository knowledge.
6. Run `python3 scripts/harness-check.py` before handoff.

## Mechanical Invariants

The harness checker currently enforces:

- required agent-system docs exist;
- `AGENTS.md` stays short and links to the source-of-truth docs;
- `docs/README.md` links the core knowledge sources;
- no unresolved merge-conflict markers exist in tracked or new files;
- every root Cargo workspace member appears in `ARCHITECTURE.md`;
- local markdown links resolve to existing files or directories;
- daemon prompt guidance does not reference removed `memory_search` or
  `memory_read` tool names.

When review feedback repeats, prefer adding another targeted check instead of
only adding prose.

## Limits

Shore has deterministic in-process harnesses, MCP-driven end-to-end checks,
runtime tracing, diagnostics, ledger/cache forensics, and cache/live scripts. It
does not yet ship a full ephemeral metrics/traces stack per worktree; until that
exists, [OBSERVABILITY.md](OBSERVABILITY.md) is the required legibility map.
