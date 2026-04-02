# Code Quality Audit — 2026-04-02

## Overview

Full audit of all 9 crates (~55K LOC). Findings organized by severity.

---

## CRITICAL: Duplicated Patterns

### 1. Model config structs defined 3x

**Location**: `shore-config/src/models.rs`

`ProviderConfig`, `ModelEntry`, and `ResolvedModel` all define the same ~19 fields (`sdk`, `api_key_env`, `base_url`, `max_context_tokens`, `temperature`, etc.). Every new provider setting must be added in 3 places, and the `merge_provider()` function has 18 identical `merge_opt!()` calls.

**Fix**: Extract a shared `ModelConfig` struct, compose it into the three types.

### 2. Model resolution chain repeated 5+ times

**Location**: `shore-daemon/src/commands/state.rs` (lines 315, 377, 558, 806), `handler.rs`, `autonomy/manager.rs`

```rust
ctx.config.app.defaults.memory_agent.as_deref()
    .and_then(|name| ctx.config.models.find_model(name).ok())
    .or_else(|| ctx.active_model.as_deref()
        .and_then(|name| ctx.config.models.find_model(name).ok()))
    .or_else(|| ctx.config.models.first_chat_model())
    .ok_or_else(|| (ErrorCode::InternalError, "No model configured".to_string()))?
```

**Fix**: Extract `fn resolve_agent_model(ctx: &CommandContext) -> Result<ResolvedModel, ...>`.

### 3. Vector store setup repeated 5+ times

**Location**: `shore-daemon/src/commands/state.rs` (lines 336, 441, 826), `handler.rs` (line 748), `autonomy/manager.rs` (line 1001)

```rust
let search_ctx = match resolve_embed_config(...) { ... };
let vs_path = memory_dir(ctx, &char_name).join("vectorstore");
VectorStore::open(&vs_path, embed_config.dimensions).await.ok()...
```

**Fix**: Extract `fn setup_semantic_search(ctx, char_name) -> Option<AgentSearchContext>`.

### 4. LLM provider duplication (~15-20% of shore-llm-client)

**Location**: `openai.rs`, `anthropic.rs`, `gemini.rs`

- **HTTP status checking** — identical pattern repeated 5x
- **Done event JSON** — same structure built 3x
- **Start event emission** — same flag-check + emit pattern 3x
- **Usage token extraction** — duplicated 6x (Anthropic has it in both streaming and non-streaming)
- **Finish reason normalization** — OpenAI and Gemini have `normalize_finish_reason()`, Anthropic doesn't

**Fix**: Extract shared helpers (`check_http_response()`, `build_done_event()`, `normalize_finish_reason()` enum).

---

## HIGH: Oversized Functions

| Function | File | Lines | What it does |
|----------|------|-------|-------------|
| `handle_generation()` | `shore-daemon/src/handler.rs:372-901` | **530** | Engine reload + message append + model resolution + prompt assembly + tool loop + persistence |
| `draw_conversation()` | `shore-tui/src/ui.rs:232-484` | **253** | Entry rendering + scroll + streaming + padding |
| `compact()` | `shore-daemon/src/commands/state.rs:507-746` | **240** | Config + LLM execution + collation conditional + response |
| `print_log()` | `shore-cli/src/output.rs:521-707` | **187** | Message rendering with roles, blocks, images |
| `print_diagnostics()` | `shore-cli/src/output.rs:1593-1730` | **138** | 3 nearly identical sections (API calls, tool calls, errors) — pure copy-paste |

---

## HIGH: Copy-Paste Code

### 1. `print_diagnostics()` — 3 identical sections

**Location**: `shore-cli/src/output.rs:1603-1729`

API calls, tool calls, and errors sections each repeat the same ~50-line pattern (timestamp parsing, time formatting, color wrapping). Extract a generic `print_diagnostics_section()`.

### 2. Color handling boilerplate — 98 instances

**Location**: `shore-cli/src/output.rs`

```rust
if use_color() { let _ = crossterm::execute!(out, SetForegroundColor(Color::DarkGrey)); }
let _ = write!(out, "text");
if use_color() { let _ = crossterm::execute!(out, ResetColor); }
```

Extract `write_colored(out, text, color)`.

### 3. Collation variable setup — 2 identical blocks

**Location**: `shore-daemon/src/commands/state.rs:659-667` vs `845-853`

Same HashMap construction with char/user/definition/user_definition. Extract `build_collation_vars()`.

### 4. XDG directory resolution — 3x identical fallback chain

**Location**: `shore-config/src/lib.rs:59-101`

Config, data, and runtime paths follow the same env -> XDG -> dirs -> fallback pattern. Extract `resolve_xdg_dir()`.

### 5. Config validation — 3x identical model checks

**Location**: `shore-config/src/lib.rs:423-460`

Same pattern for `defaults.model`, `defaults.tool_model`, `defaults.memory_agent`. Extract `validate_model_reference()`.

---

## MEDIUM: Cross-Crate Inconsistencies

### 1. Default value mismatch (potential bug)

`idle_trigger_minutes` defaults to **30** in `shore-config/src/app.rs` but **15** in `shore-daemon/src/memory/compaction.rs`. The daemon's internal default gets overridden by config, but if config omits the field, the values disagree.

### 2. Two `ToolError` enums

- `shore-daemon/src/engine/tools.rs:19-22` — minimal: `Llm(LlmError)`
- `shore-daemon/src/tools/mod.rs:67-78` — rich: `InvalidArgs`, `Agent`, `NotImplemented`, `Io`, `Http`

Different purposes but naming collision is confusing.

### 3. Two content derivation functions

- `shore-protocol/src/types.rs:125-152` — `derive_content_from_blocks()` (includes ToolResult)
- `shore-protocol/src/merge.rs:33-48` — `derive_content_text_only()` (excludes ToolResult)

Same loop, same trimming. Parameterize with `include_tool_results: bool`.

### 4. CompactionConfig / CollationConfig defined in both shore-config and shore-daemon

Config-layer versions have serde annotations; daemon versions have runtime defaults. Field types differ (`u32` vs `u64`). Should share a base or daemon should consume the config type directly.

### 5. OpenAI provider does JSON round-trip for ContentBlock

`openai.rs:692-741` builds content blocks as `Value::Array`, then deserializes via `serde_json::from_value()`. Anthropic and Gemini construct `ContentBlock` variants directly.

---

## LOW: Minor Issues

- `find_model()` returns `CatalogError::AmbiguousName` for "not found" — wrong variant semantics
- `flush_thinking()` and `flush_tools()` in `shore-tui/src/ui.rs` share identical width/wrapping patterns
- `write_row()` / `write_row_colored()` in `shore-cli/src/output.rs:736-761` are near-identical
- 15+ single-line `default_*()` functions in `shore-config/src/app.rs` — could use a macro
- `ToolToggles::is_enabled()` has 15 match arms following `"name" => self.name` — could use const table

---

## Remediation Plan

### Phase 1: Daemon helpers (quick wins, high impact)
1. Extract `resolve_agent_model()` helper
2. Extract `setup_semantic_search()` helper
3. Extract `build_collation_vars()` helper
4. Fix `idle_trigger_minutes` default mismatch

### Phase 2: shore-config consolidation
5. Consolidate 3 model config structs into shared `ModelConfig`
6. Extract `resolve_xdg_dir()` helper
7. Extract `validate_model_reference()` helper
8. Consolidate content derivation functions in shore-protocol

### Phase 3: shore-llm-client provider dedup
9. Extract shared HTTP/streaming helpers
10. Unify finish reason normalization
11. Fix OpenAI ContentBlock round-trip

### Phase 4: CLI/TUI cleanup
12. Extract color helper + refactor `print_diagnostics()`
13. Consolidate `write_row()` variants
14. Decompose `handle_generation()` (530 lines)

---

## Positives

- Well-tested — comprehensive test suites across all crates
- Clear crate boundaries — protocol, config, client, daemon, frontends logically separated
- Good trait design — `CollationLlm`, `CompactionLlm`, `AgentIndexer` provide clean abstraction
- No dead code — all public exports appear used
- No circular dependencies
- Good error context — error types generally provide helpful messages
