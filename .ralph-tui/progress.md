# Ralph Progress Log

This file tracks progress across iterations. Agents update this file
after each iteration and it's included in prompts for context.

## Codebase Patterns (Study These First)

- Workspace uses `resolver = "2"` in root Cargo.toml
- Library crates (`shore-protocol`, `shore-client`) use `src/lib.rs`; binary crates use `src/main.rs`
- `shore-llm` is a standalone TypeScript package outside the Cargo workspace
- Protocol types use `#[serde(tag = "type", rename_all = "snake_case")]` for JSON-Lines framing
- `NewMessage` uses `#[serde(flatten)]` to inline Message fields into the envelope
- `StreamChunk.content_type` defaults to `"text"` via `#[serde(default = "...")]`

---

## 2026-03-25 - US-001
- What was implemented: Full repo scaffolding with Cargo workspace, 6 Rust crates, TypeScript shore-llm package, docs/ and examples/ directories
- Files changed:
  - `.gitignore` — Rust + Node + IDE ignores
  - `Cargo.toml` — workspace root with all 6 Rust members
  - `shore-protocol/` — library crate (Cargo.toml + src/lib.rs)
  - `shore-client/` — library crate (Cargo.toml + src/lib.rs)
  - `shore-daemon/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-cli/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-tui/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-matrix/` — binary crate (Cargo.toml + src/main.rs)
  - `shore-llm/` — package.json, tsconfig.json, src/index.ts
  - `docs/SHORE-V2-ARCHITECTURE.md` — copied from root ARCHITECTURE.md
  - `examples/config.toml` — example daemon config
  - `examples/models.toml` — example model definitions
- **Learnings:**
  - Empty `src/lib.rs` files are valid for workspace compilation — no placeholder code needed
  - Cargo workspace with `resolver = "2"` compiles, tests, and lints cleanly with zero-content crates
---

## 2026-03-25 - US-002
- What was implemented: All SWP protocol message types as serde-serializable Rust structs in shore-protocol
- Files changed:
  - `shore-protocol/Cargo.toml` — added serde + serde_json dependencies
  - `shore-protocol/src/lib.rs` — module declarations, SWP_V1 constant, 28 unit tests
  - `shore-protocol/src/types.rs` — Role, ImageRef, Message, TokenCounts, TimingInfo, StreamMetadata, ConversationInfo, CharacterInfo
  - `shore-protocol/src/client_msg.rs` — ClientHello, ClientMessageBody, Regen, Command, ClientMessage enum
  - `shore-protocol/src/server_msg.rs` — ServerHello, History, Shutdown, Ping, CommandOutput, Error, StreamStart, StreamChunk, StreamEnd, Phase, NewMessage, ToolCall, ToolResult, SendImage, CacheWarning, ServerMessage enum
  - `shore-protocol/src/error.rs` — ErrorCode enum with 7 variants
- **Learnings:**
  - `#[serde(flatten)]` on NewMessage inlines the Message fields into the tagged envelope so `msg_id`, `role`, etc. appear at top level alongside `"type": "new_message"`
  - `#[serde(default = "fn_name")]` works well for default string values like content_type
  - `#[serde(skip_serializing_if = "Option::is_none")]` keeps JSON clean for optional fields (alt_index, alt_count, caption, guidance)
  - All 28 round-trip tests pass; cargo build, test, clippy all clean
---

