# Shore V2 — Goals Audit

**Date:** 2026-04-16
**Scope:** How well the implementation matches the stated goals in `README.md`, `CLAUDE.md`, and `docs/FEATURES.md`.
**Out of scope (known, temporary):** `shore-matrix` disabled on rustc 1.94+ due to `matrix-sdk 0.16.0` recursion_limit; `shore-gui` Godot project not yet a shipping binary.

## Executive summary

~90% aligned with stated goals. No advertised feature is completely unimplemented. The one meaningful architectural gap is **`shore-daemon` having grown well past the project's own "small modules" rule**. Everything else the README promises is implemented and reachable through the CLI.

---

## Fully met

### 2. Daemon architecture
`shore-daemon` hosts persistent state; CLI / TUI / MCP clients speak SWP over TCP (`shore-protocol`). "Not a chat wrapper, a daemon" holds.

### 3. Durable memory (FTS + vector, compaction, collation)
`shore-daemon/src/memory/` implements the dual-index (SQLite FTS + vector). CLI subcommands `memory compact / changelog / reindex / purge / shell` are all wired. Idle-trigger compaction and background collation are live, not config-only.

### 5. Six LLM providers
Real implementations, not stubs:
- `shore-llm-client/src/providers/anthropic.rs` — 2,190 LOC
- `shore-llm-client/src/providers/openai.rs` — 1,224 LOC (covers DeepSeek, xAI, ZhipuAI, OpenRouter via `base_url`)
- `shore-llm-client/src/providers/gemini.rs` — 856 LOC
- `shore-llm-client/src/providers/zai.rs` — 880 LOC

### 6. Per-operation model slots
All six slots configurable and resolved at runtime: `model`, `tool_model`, `memory_agent`, `compaction`, `interiority`, `image_generation`.

### 7. Autonomy (interiority ticks, dormancy, recap)
- `shore-daemon/src/autonomy/interiority.rs` (638 LOC) — tick state machine, `InteriorityAction` enum, `InteriorityClock`, min-interval floor, idle guards.
- `shore-daemon/src/autonomy/recap_store.rs` (182 LOC) — persistent recap journal carrying state between ticks.
- `shore-daemon/src/autonomy/manager.rs` (2,841 LOC) — tick orchestration, LLM calls, `max_tool_rounds` cap.
- All documented knobs (`dormant_after_interiority_turns`, `dormant_after_idle_time`, `minimum_interiority_latency`, `fallback_interiority_interval`) are enforced, not just parsed.

### 8. Tool use (15+ handlers)
14 real tool handlers in `shore-daemon/src/tools/`, all dispatched from `tools/mod.rs`:
- Memory: `memory`
- Web: `web_search` (Tavily), `fetch_url`
- Time/chance: `check_time`, `roll_dice`
- Images: `send_image`, `list_images`, `recall_image`, `generate_image`, `remember_image`
- Scratchpad: `scratchpad_list / read / write / delete` (with path-traversal validation)
- Activity: `activity_heatmap`

No listed-but-stubbed tools. Loop budget (`max_iterations`, default 10) enforced.

### 10. Prompt caching + cache forensics
- `cache_ttl` per model wired through to Anthropic cache headers.
- `[advanced] cache_forensics = true` writes per-request accounting to `cache_forensics.jsonl` via `shore-llm-client/src/cache_forensics.rs`.

### 11. Remote access
`[daemon] unsafe_allow_remote_access` is required for any non-loopback bind; `allowed_hosts` IP allowlist optional. FEATURES.md honestly documents the "no TLS, no auth" caveat.

### 12. CLI command surface
Every command documented in FEATURES.md exists in `shore-cli/src/cli.rs`: `send`, `regen --guidance`, `log -f / --heartbeat / edit / delete`, `character --new / --info`, `model --reset`, `memory compact / changelog / reindex / purge / shell`, `status --diagnostics`, `config --check / --reset`, `completions`.

### 13. Testing policy (revised 2026-04-14)
- **Zero hand-written LLM response mocks in `shore-llm-client`.** Policy holds in practice.
- Live e2e gated behind `--ignored` (`cargo test --test e2e -- --ignored`); `./scripts/live-tests/live-test.sh` hits real APIs.
- `shore-mcp` integration tests (`shore-mcp/tests/mcp_integration.rs` + 3 others) drive a real daemon end-to-end per the 2026-04-15 MCP verification addendum.

---

## Partial

### 1. Small modular crates (~2–5K LOC/crate, ~500 LOC/module) — ⚠ drift

Crate sizes (active workspace members):

| Crate | LOC | Within budget? |
|---|---|---|
| shore-protocol | 2,301 | ✓ |
| shore-config | 3,950 | ✓ |
| shore-diagnostics | 268 | ✓ |
| shore-client | 2,356 | ✓ |
| shore-llm-client | 8,871 | ⚠ over |
| **shore-daemon** | **35,377** | **✗ 7× over** |
| shore-daemon-server | 1,992 | ✓ |
| shore-cli | 6,204 | ⚠ over |
| shore-mcp | 1,643 | ✓ |
| shore-tui | 6,669 | ⚠ over |
| shore-ledger | 3,129 | ✓ |
| shore-test-harness | 1,387 | ✓ |

Module-level hot spots inside `shore-daemon`:
- `autonomy/manager.rs`: **2,841 LOC** (~5× the 500-LOC module guideline)
- Multiple handler files in the 1K+ range

`STEELMAN.md` already flags this and suggests extracting the state-persistence layer as a first cut. This is the most visible deviation from the project's own stated architectural discipline — a correctness-neutral but debuggability-negative drift.

**Recommendation:** Pull out at minimum (a) the state-persistence layer and (b) the autonomy orchestrator into their own crates. Treat the 2-5K/500 LOC targets as enforceable budgets rather than aspirations.

---

## Honest notes from the code itself

- `TODO/review/STEELMAN.md` (dated today) already documents 3 real bugs + 12 new findings — discovery-string mismatch in `shore-client/src/discovery.rs`, non-atomic state persistence, path-traversal edge cases in image handling. Project is auditing itself.
- No `unimplemented!()` in hot paths. No large dead modules. No "listed feature, stubbed handler" cases found.

## Bottom line

Shore delivers on the core pitch: persistent daemon, durable memory with FTS + vector search, autonomy with real tick / dormancy / recap semantics, per-operation model routing, six-provider coverage, disciplined no-mock LLM testing, MCP-driven live verification. The one structural debt worth prioritizing is **breaking up `shore-daemon`** so the project's own "small discrete modules" rule applies to its largest crate.
