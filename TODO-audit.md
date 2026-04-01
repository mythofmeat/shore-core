# Codebase Quality Audit — TODO

Audit performed 2026-04-01. Findings organized by priority.

## Tier 1 — Copy-Paste Duplication

- [ ] **1A. connection.rs duplicated between TUI and Matrix**
  - `shore-tui/src/connection.rs` (137 LOC) and `shore-matrix/src/connection.rs` (134 LOC) are 95%+ identical
  - Only difference: `SWPConnection::connect()` params (`"tui"/"shore-tui"/character` vs `"bridge"/"shore-matrix"/None`)
  - Fix: move `ConnEvent`, `ConnCommand`, `resolve_addr()`, generic `spawn_connection()` into `shore-client`

- [ ] **1B. LLM response text extraction — tripled**
  - Identical 12-line block in `memory/agent_llm.rs:109-120`, `memory/collation_impls.rs:84-95`, `memory/compaction_impls.rs:264-275`
  - Fix: add `fn extract_text(resp: &LlmResponse) -> String` to `shore-llm-client`

- [ ] **1C. ContentBlock-to-JSON conversion — tripled**
  - `engine/tools.rs:38-63` already has `content_block_to_json()` as a proper function
  - `memory/researcher.rs:195-219` and `memory/agent/tool_loop.rs:141-165` reimplement it inline
  - `autonomy/manager.rs:787-793` and `handler.rs:473-474` have partial versions
  - Fix: move `content_block_to_json()` to shared location, replace all inline copies

- [ ] **1D. Tool use extraction — doubled**
  - `memory/researcher.rs:168-177` and `memory/agent/tool_loop.rs:59-68` — identical `filter_map(ContentBlock::ToolUse)` pattern
  - Fix: add `fn extract_tool_uses()` next to the content_block_to_json utility

## Tier 2 — Structural Duplication

- [ ] **2A. Manual Display/Error impls — should use thiserror**
  - 5 error types in `shore-daemon/src/memory/` hand-roll Display+Error despite `thiserror = "2"` being a dep
  - `CompactionError` (compaction.rs:111-135), `CollationError` (collation.rs:48-62), `AgentLlmError` (agent_llm.rs:22-38), `AgentError` (agent/types.rs:52-72), `VectorStoreError` (vectorstore.rs:16-46)
  - Fix: convert all to `#[derive(thiserror::Error)]`

- [ ] **2B. LLM build_request+generate+error_map — tripled**
  - `agent_llm.rs:99-106`, `collation_impls.rs:74-82`, `compaction_impls.rs:251-261`
  - Fix: add `LlmClient::simple_generate()` convenience method

- [ ] **2C. Image protocol detection — duplicated between CLI and TUI**
  - `shore-cli/src/images.rs` and `shore-tui/src/images.rs` both define `ImageProtocol` + detection with subtle inconsistencies
  - Fix: extract to shared module

- [ ] **2D. Memory DB open pattern — repeated in commands/state.rs**
  - Lines ~197-238 and ~258-294 both open character memory DB with identical boilerplate
  - Fix: extract `fn open_character_memory_db()`

## Tier 3 — Complexity / Structure

- [ ] **3A. `src/shore-git/` — stale 5.1GB clone of local bare repo**
  - `shore-git/` (top level) = bare git repo (local backup/mirror)
  - `src/shore-git/` = full clone of that bare, 4 days behind HEAD, not tracked by parent git
  - Neither is a submodule. Pure dead weight.
  - Fix: delete both after confirming not needed

- [ ] **3B. `compat.rs` — 1290 LOC monolith**
  - Handles 8 unrelated V1 compat concerns in one file
  - Fix: split into `src/compat/` module directory

- [ ] **3C. `execute_interiority_tick()` — 271 lines** (autonomy/manager.rs:631-902)
  - Fix: extract 4-5 helper functions

- [ ] **3D. `run_tool_loop()` — 639 lines** (engine/tools.rs:74-713)
  - Fix: extract tool execution, message building, result handling

- [ ] **3E. `handle_user_message()` — 373 lines** (handler.rs:257-630)
  - Fix: extract into ~5 helper functions

- [ ] **3F. Autonomy manager notification boilerplate** (manager.rs:283-350)
  - 6 methods all do lock→get→lock→operate
  - Fix: extract `fn with_state()` helper

## Tier 4 — Minor

- [ ] **4A. Debug eprintln!s in compaction_impls.rs** (lines 254, 260-261) — should use tracing or remove
- [ ] **4B. Inconsistent dirty-state marking** — `notify_user_message()` calls `mark_dirty()` but `notify_assistant_message()` doesn't
- [ ] **4C. Test mock duplication** in memory_integration.rs — shared test helpers would reduce boilerplate
- [ ] **4D. `extract_json()` in collation_impls.rs** — candidate for shared utility if more callers appear
