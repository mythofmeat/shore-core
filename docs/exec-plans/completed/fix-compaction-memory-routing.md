# Fix Compaction Memory Routing

Status: completed
Owner: agent
Started: 2026-05-01

## Goal

Resolve review findings that left compaction patches incomplete, routed
`MEMORY.md` writes to the wrong directory, and reused cached chat request
settings instead of compaction request settings.

## Context

- `backend/daemon/src/memory/compaction/mod.rs`
- `backend/daemon/src/memory/compaction_impls.rs`
- `backend/daemon/src/memory/dreams_log.rs`

## Work Items

- [x] Include the dreams log module in the patch.
- [x] Route compaction `MEMORY.md` writes to the workspace root and queue prompt refresh.
- [x] Preserve compaction model/provider/no-tool settings when using a cached prefix.
- [x] Add focused regression coverage.

## Validation

- [x] `cargo test -p shore-daemon memory::compaction`

## Decisions

- 2026-05-01: Keep compaction cache reuse by preserving cached system/messages as the prefix, but rebuild the request shell from the resolved compaction model so provider, token, sampler, provider-option, and tool settings come from compaction config.

## Handoff Notes

Completed in this worktree.
