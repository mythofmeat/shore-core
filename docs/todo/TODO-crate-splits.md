# Crate Split TODO

Tracking structural debt against the project's 2-5K LOC per crate / ~500 LOC per module guidelines.

## Overview

| Crate | LOC | Status | Action |
|-------|-----|--------|--------|
| shore-daemon | 37K | Critical | Split into 5-8 crates |
| shore-llm-client | 9.2K | Over budget | Split providers or accept as justified |
| shore-tui | 5.6K | Slightly over | Module-level splits only |
| shore-cli | 5.3K | Slightly over | Module-level splits only |
| shore-config | 3.8K | OK | — |
| shore-matrix | 3.7K | OK | — |
| shore-protocol | 3.0K | OK | — |
| shore-ledger | 2.8K | OK | — |
| shore-client | 1.8K | OK | — |
| shore-test-harness | 1.3K | OK | — |
| shore-diagnostics | 268 | OK | — |

---

## shore-daemon (37K LOC) — Critical

The daemon has 7 internal subsystems, several of which are crate-sized. The dependency
graph flows downward: `main → server/handler → commands/autonomy → engine/memory/tools`.

### Proposed extractions (high to low priority)

#### 1. shore-memory (extract memory/)

**~12K LOC.** Largest internal module by far. Contains 4 distinct subsystems:

| Submodule | LOC | Responsibility |
|-----------|-----|----------------|
| db.rs + vectorstore.rs | ~1.9K | SQLite + LanceDB storage layer |
| agent/ | ~2.9K | Memory management agent (tool loop, handlers) |
| compaction/ + compaction_impls.rs | ~2.7K | Background summarization/retention |
| collation/ + collation_impls.rs | ~2.7K | Memory organization/clustering |
| rag.rs + search.rs + researcher.rs | ~1.4K | Retrieval and semantic search |

Could become a single `shore-memory` crate (12K, then internally split into modules) or
split further into `shore-memory-db`, `shore-memory-agent`, etc. The storage layer
(db + vectorstore) is the natural foundation — everything else depends on it.

**Internal dependency chain:** db ← agent ← researcher ← rag; db ← compaction; db ← collation

**External deps:** rusqlite, lancedb, arrow-array, shore-llm-client (for agent LLM calls)

#### 2. shore-engine (extract engine/)

**~3.7K LOC.** Conversation state management: active message store, prompt building,
frozen history segments, tool spec generation.

Almost self-contained — only depends on tools/ for spec building. Could be reused by
client libraries. Prompt building (prompt.rs, 1.7K) is the largest file and would benefit
from a module-level split even if the crate extraction doesn't happen.

#### 3. shore-daemon-server (extract server/)

**~1.3K LOC.** SWP listener (Unix socket + TCP), client handshake, message routing,
instance registry. **Zero internal dependencies** — the cleanest extraction candidate.

#### 4. shore-notifications (extract notifications.rs)

**~250 LOC.** Push notification dispatcher (ntfy, notify-send, shell). Only depends on
shore-config. Tiny but completely self-contained.

#### 5. shore-daemon-tools (extract tools/)

**~2.8K LOC.** Tool definitions and dispatch for LLM agent execution (memory tools, web
search, images, scratchpad, activity). Depends on memory and autonomy — harder to extract
cleanly without trait abstractions.

### Modules to keep in shore-daemon

- **handler/** (~2.3K) — Orchestration hub, touches everything. Not worth extracting.
- **commands/** (~3.6K) — Dispatch layer, tightly coupled to CommandContext.
- **autonomy/** (~4K) — Per-character scheduler, feedback loop with handler.
- **compat.rs** (~1.3K) — Legacy migration shims, needs global visibility.
- **characters.rs, content_util.rs, templates.rs** — Small, cross-cutting.

### Key architectural notes

- Heavy `Arc<Mutex<T>>` and `Arc<DashMap>` for shared state — splits will need careful
  trait boundaries to avoid passing 10 Arcs through every function.
- handler ↔ autonomy form a cycle (handler triggers activity, autonomy triggers
  generation). Requires interface design before splitting.
- `ToolContext` trait already abstracts tool deps — good pattern to extend.

---

## shore-llm-client (9.2K LOC) — Over budget

The providers/ module is 6.3K LOC (73% of the crate). The core (types, retry, stream,
cache_forensics) is only ~2.4K.

### File breakdown

| File | LOC | Notes |
|------|-----|-------|
| providers/anthropic.rs | 2,102 | Largest provider |
| providers/openai.rs | 1,229 | OpenAI-compat (OR, DeepSeek, X.AI) |
| providers/zai.rs | 884 | Z.AI API |
| providers/gemini.rs | 860 | Google native protocol |
| providers/stream_helpers.rs | 568 | Shared provider utilities |
| providers/context.rs | 233 | Provider-specific config builder |
| providers/sse.rs | 214 | SSE parser |
| providers/mod.rs | 179 | Dispatch router |
| stream.rs | 1,033 | Stream consumer + cache invalidation |
| types.rs | 453 | Request/response types |
| retry.rs | 394 | Retry + fallback policy |
| lib.rs | 394 | LlmClient, request builder |
| cache_forensics.rs | 155 | Diagnostic logging |

### Options

**A) Extract per-provider crates** (`shore-provider-anthropic`, `shore-provider-openai`, etc.)
Each provider is fairly self-contained. They share sse.rs and stream_helpers.rs which
would become a shared `shore-provider-core` crate.

**B) Accept as justified.** Multi-provider dispatch is inherently wide. The crate is
well-structured internally and each provider is a natural module boundary. Adding another
provider would tip the scales toward option A.

**Recommendation:** Accept for now. Revisit if a 5th provider is added or any single
provider file exceeds ~2.5K LOC.

---

## shore-tui (5.6K LOC) — Module splits only

Slightly over the crate limit. The real problem is module-level: ui.rs is 2.3K LOC.

| File | LOC | Status |
|------|-----|--------|
| ui.rs | 2,355 | 4.7x module limit |
| main.rs | 905 | 1.8x |
| input.rs | 809 | 1.6x |
| app.rs | 683 | 1.4x |
| images.rs | 658 | 1.3x |
| markdown.rs | 211 | OK |

### Suggested module splits

- **ui.rs** → `ui/conversation.rs`, `ui/text_formatting.rs`, `ui/panels.rs`
- **input.rs** → `input/normal.rs`, `input/insert.rs`, `input/command.rs` (by mode)
- **main.rs** → extract daemon connection/discovery to `connection_setup.rs`

No crate extraction needed — just internal reorganization.

---

## shore-cli (5.3K LOC) — Module splits only

Slightly over the crate limit. Three files over 1K LOC.

| File | LOC | Status |
|------|-----|--------|
| cli.rs | 1,413 | 2.8x module limit |
| output/commands.rs | 1,339 | 2.7x |
| run.rs | 1,015 | 2x |
| output/transcript.rs | 567 | 1.1x (borderline) |

### Suggested module splits

- **output/commands.rs** → split by domain: `output/models.rs`, `output/characters.rs`,
  `output/memory.rs`, `output/config.rs`, `output/diagnostics.rs`
- **cli.rs** → extract subcommand enums or helper functions to submodules
- **run.rs** → extract streaming handlers or split by command category

No crate extraction needed — just internal reorganization.

---

## Execution order

If tackling these incrementally:

1. **shore-daemon-server** — zero-dep extraction, quick win, proves the pattern
2. **shore-memory** — biggest LOC reduction, clearest boundaries
3. **shore-engine** — nearly self-contained, high reuse potential
4. **shore-tui module splits** — internal only, low risk
5. **shore-cli module splits** — internal only, low risk
6. **shore-llm-client** — only if it grows further
