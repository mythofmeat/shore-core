# Extract `shore-daemon-server` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract `shore-daemon/src/server/` into a standalone `shore-daemon-server` workspace crate with zero behavioral changes.

**Architecture:** Pure mechanical move. The server module (mod.rs + registry.rs, ~1,317 LOC) has zero internal dependencies on other shore-daemon modules. It only depends on `shore-protocol` and `shore-config`. Two consumers (`main.rs`, `handler/mod.rs`) update their imports.

**Tech Stack:** Rust, Cargo workspaces

---

## File Map

| Action | File | Responsibility |
|--------|------|---------------|
| Create | `shore-daemon-server/Cargo.toml` | New crate manifest |
| Create | `shore-daemon-server/src/lib.rs` | Server, ServerConfig, ClientInfo, RoutedMessage + all private helpers/tests from mod.rs |
| Move | `shore-daemon-server/src/registry.rs` | Registry, InstanceInfo (copied from `shore-daemon/src/server/registry.rs`) |
| Modify | `Cargo.toml:3` | Add `shore-daemon-server` to workspace members |
| Modify | `shore-daemon/Cargo.toml:6-19` | Add `shore-daemon-server` dep, remove `fs2` (only used by server) |
| Modify | `shore-daemon/src/lib.rs:13` | Remove `pub mod server;` |
| Delete | `shore-daemon/src/server/mod.rs` | Moved to new crate |
| Delete | `shore-daemon/src/server/registry.rs` | Moved to new crate |
| Modify | `shore-daemon/src/main.rs:11-12` | Update imports to `shore_daemon_server` |
| Modify | `shore-daemon/src/handler/mod.rs:43` | Update import to `shore_daemon_server::RoutedMessage` |

---

### Task 1: Create the `shore-daemon-server` crate scaffold

**Files:**
- Create: `shore-daemon-server/Cargo.toml`
- Create: `shore-daemon-server/src/lib.rs` (empty placeholder)

- [ ] **Step 1: Create `shore-daemon-server/Cargo.toml`**

```toml
[package]
name = "shore-daemon-server"
version = "0.1.0"
edition = "2021"

[dependencies]
shore-protocol = { path = "../shore-protocol" }
shore-config = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
fs2 = "0.4"

[dev-dependencies]
tokio = { version = "1", features = ["test-util"] }
tempfile = { workspace = true }
```

- [ ] **Step 2: Create empty `shore-daemon-server/src/lib.rs`**

```rust
pub mod registry;
```

- [ ] **Step 3: Add to workspace members in root `Cargo.toml`**

In `Cargo.toml`, add `"shore-daemon-server"` to the `members` array after `"shore-daemon"`:

```toml
members = [
    "shore-protocol",
    "shore-client",
    "shore-diagnostics",
    "shore-config",
    "shore-llm-client",
    "shore-daemon",
    "shore-daemon-server",
    "shore-cli",
    "shore-tui",
    "shore-ledger",
    "shore-test-harness",
    # "shore-matrix",  # disabled: matrix-sdk 0.16.0 hits recursion_limit on rustc 1.94+
]
```

- [ ] **Step 4: Verify scaffold compiles**

Run: `cargo check -p shore-daemon-server`

Expected: Compile error about missing `registry` module (that's fine — we haven't moved code yet). The crate itself should resolve in the workspace.

- [ ] **Step 5: Commit**

```bash
git add shore-daemon-server/Cargo.toml shore-daemon-server/src/lib.rs Cargo.toml
git commit -m "chore: scaffold shore-daemon-server crate"
```

---

### Task 2: Move server code into the new crate

**Files:**
- Write: `shore-daemon-server/src/lib.rs` (full content from `shore-daemon/src/server/mod.rs`)
- Copy: `shore-daemon-server/src/registry.rs` (from `shore-daemon/src/server/registry.rs`)

- [ ] **Step 1: Copy `registry.rs` to the new crate**

Copy `shore-daemon/src/server/registry.rs` → `shore-daemon-server/src/registry.rs` byte-for-byte. No changes needed — the file has no `crate::` imports.

- [ ] **Step 2: Write `lib.rs` from `mod.rs`**

Copy the full content of `shore-daemon/src/server/mod.rs` into `shore-daemon-server/src/lib.rs`. The only change: the first line `pub mod registry;` is already there — just replace the entire file with the content of mod.rs (which also starts with `pub mod registry;`).

No `use crate::` references exist in this file, so no import rewrites are needed.

- [ ] **Step 3: Verify the new crate compiles**

Run: `cargo check -p shore-daemon-server`

Expected: PASS — clean compile with zero warnings.

- [ ] **Step 4: Run tests in the new crate**

Run: `cargo test -p shore-daemon-server`

Expected: All existing server + registry tests pass (handshake, routing, broadcast, TCP ACL, registry CRUD).

- [ ] **Step 5: Commit**

```bash
git add shore-daemon-server/src/lib.rs shore-daemon-server/src/registry.rs
git commit -m "feat: move server + registry code into shore-daemon-server"
```

---

### Task 3: Rewire `shore-daemon` to use the new crate

**Files:**
- Modify: `shore-daemon/Cargo.toml` — add `shore-daemon-server` dep, remove `fs2`
- Modify: `shore-daemon/src/lib.rs:13` — remove `pub mod server;`
- Delete: `shore-daemon/src/server/mod.rs`
- Delete: `shore-daemon/src/server/registry.rs`
- Modify: `shore-daemon/src/main.rs:11-12` — update imports
- Modify: `shore-daemon/src/handler/mod.rs:43` — update import

- [ ] **Step 1: Add `shore-daemon-server` to `shore-daemon/Cargo.toml` dependencies**

Add after the `shore-protocol` line:

```toml
shore-daemon-server = { path = "../shore-daemon-server" }
```

- [ ] **Step 2: Remove `fs2` from `shore-daemon/Cargo.toml`**

`fs2 = "0.4"` is only used by `server/registry.rs`. Remove it from `[dependencies]`.

Verify no other file in shore-daemon uses `fs2`:

Run: `grep -r "use fs2" shore-daemon/src/ --include="*.rs"`

Expected: Only `shore-daemon/src/server/registry.rs` (which we're about to delete).

- [ ] **Step 3: Remove `pub mod server;` from `shore-daemon/src/lib.rs`**

Delete line 13 (`pub mod server;`). The file becomes:

```rust
#[cfg(test)]
pub mod test_support;

pub mod autonomy;
pub mod characters;
pub mod commands;
pub mod compat;
pub mod content_util;
pub mod engine;
pub mod handler;
pub mod memory;
pub mod notifications;
pub mod templates;
pub mod tools;
```

- [ ] **Step 4: Update imports in `shore-daemon/src/main.rs`**

Replace lines 11-12:

```rust
// Before:
use shore_daemon::server::registry::{InstanceInfo, Registry};
use shore_daemon::server::{Server, ServerConfig};

// After:
use shore_daemon_server::registry::{InstanceInfo, Registry};
use shore_daemon_server::{Server, ServerConfig};
```

- [ ] **Step 5: Update import in `shore-daemon/src/handler/mod.rs`**

Replace line 43:

```rust
// Before:
use crate::server::RoutedMessage;

// After:
use shore_daemon_server::RoutedMessage;
```

- [ ] **Step 6: Delete the old server module**

```bash
rm shore-daemon/src/server/mod.rs
rm shore-daemon/src/server/registry.rs
rmdir shore-daemon/src/server
```

- [ ] **Step 7: Verify shore-daemon compiles**

Run: `cargo check -p shore-daemon`

Expected: PASS — clean compile.

- [ ] **Step 8: Run full workspace tests**

Run: `cargo test --workspace`

Expected: All tests pass. The server/registry tests now run under `shore-daemon-server`, everything else unchanged.

- [ ] **Step 9: Commit**

```bash
git add shore-daemon/Cargo.toml shore-daemon/src/lib.rs shore-daemon/src/main.rs shore-daemon/src/handler/mod.rs
git rm shore-daemon/src/server/mod.rs shore-daemon/src/server/registry.rs
git commit -m "refactor: rewire shore-daemon to use shore-daemon-server crate"
```

---

### Task 4: Verify no stale references remain

- [ ] **Step 1: Search for any remaining `shore_daemon::server` references**

Run: `grep -r "shore_daemon::server" --include="*.rs" .`

Expected: Zero matches.

- [ ] **Step 2: Search for any remaining `crate::server` references**

Run: `grep -r "crate::server" --include="*.rs" shore-daemon/src/`

Expected: Zero matches.

- [ ] **Step 3: Search for `use fs2` in shore-daemon**

Run: `grep -r "use fs2" --include="*.rs" shore-daemon/src/`

Expected: Zero matches.

- [ ] **Step 4: Full workspace build (release mode)**

Run: `cargo build --workspace --release`

Expected: PASS — all binaries compile.

- [ ] **Step 5: Run full workspace tests one more time**

Run: `cargo test --workspace`

Expected: All tests pass.

---

### Task 5: Update documentation

**Files:**
- Modify: `docs/ARCHITECTURE.md`
- Modify: `docs/DECISIONS.md`

- [ ] **Step 1: Update ARCHITECTURE.md**

Add entry for the new crate in the workspace crates section:

```markdown
### shore-daemon-server
SWP (Shore Wire Protocol) server and instance registry. Handles Unix socket + TCP listeners,
client handshake, message routing, and broadcast. Also provides the daemon instance registry
for service discovery. Depends on `shore-protocol` and `shore-config` only.
```

- [ ] **Step 2: Update DECISIONS.md**

Add entry:

```markdown
## 2026-04-10: Extract shore-daemon-server crate

Extracted `shore-daemon/src/server/` (~1.3K LOC) into a standalone `shore-daemon-server`
workspace crate. The server module had zero internal dependencies on other daemon modules,
making it the cleanest extraction candidate. `RoutedMessage` enum stays in the server crate
because it's a server routing concern (not a wire protocol type) and handler already depends
on the server crate. Registry stays as a submodule (221 LOC, not worth its own crate).
```

- [ ] **Step 3: Commit documentation**

```bash
git add docs/ARCHITECTURE.md docs/DECISIONS.md
git commit -m "docs: record shore-daemon-server extraction in architecture and decisions"
```
