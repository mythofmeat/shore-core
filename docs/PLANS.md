# Execution Plans

Plans are first-class repo artifacts when work is too large or risky to fit in
one short task update.

## When To Add A Plan

Create a plan under `docs/exec-plans/active/` when work spans multiple modules,
changes architecture, touches prompt/cache behavior, changes security
boundaries, or needs staged validation.

Small single-file fixes can use ephemeral chat planning. Do not create a plan
just to satisfy ceremony.

## Lifecycle

1. Add `docs/exec-plans/active/<slug>.md`.
2. Keep a concise progress log and decision log in the plan.
3. Link tests, scripts, or MCP runs used for validation.
4. Move the plan to `docs/exec-plans/completed/` when finished.
5. Move unresolved follow-ups into `docs/exec-plans/tech-debt-tracker.md`.

## Template

```md
# <Plan Title>

Status: active
Owner: agent
Started: YYYY-MM-DD

## Goal

What user-visible or architectural outcome this plan delivers.

## Context

Links to the relevant docs, code paths, issues, or prior decisions.

## Work Items

- [ ] Item

## Validation

- [ ] Focused test or script
- [ ] Broader check if needed

## Decisions

- YYYY-MM-DD: Decision and reason.

## Handoff Notes

Anything the next agent needs to continue without external context.
```
