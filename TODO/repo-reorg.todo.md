# Repo Reorg: Core, Backend, Clients, Bridges, and Dev

Status: implemented in this worktree. Kept as the migration record for the layout refactor.

## Summary

Regroup the repository by ownership and runtime role while keeping one main Cargo workspace for normal Rust crates. Use `workspace.default-members` and targeted CI/package builds to keep everyday compile paths fast, rather than splitting the repo into multiple Cargo workspaces. Keep Godot as the main nested/out-of-workspace exception because it has distinct tooling and experimental dependencies.

## Target Layout

- `core/`
  - `protocol/` -> package `shore-protocol`
  - `config/` -> package `shore-config`
  - `swp-client/` -> package `shore-swp-client`, renamed from `shore-client`
- `backend/`
  - `daemon/` -> package `shore-daemon`, binary `shore-daemon`
  - `swp-server/` -> package `shore-swp-server`, renamed from `shore-daemon-server`
  - `llm/` -> package `shore-llm`, renamed from `shore-llm-client`
  - `ledger/` -> package `shore-ledger`
  - `diagnostics/` -> package `shore-diagnostics`
- `clients/`
  - `cli/` -> package `shore-cli`, binary `shore`
  - `tui/` -> package `shore-tui`, binary `shore-tui`
  - `gui/` -> existing Tauri app, including frontend and `src-tauri`
  - `gui-godot/` -> experimental Godot client; keep its Rust cdylib name `shore_bridge`
- `bridges/`
  - `matrix/` -> package `shore-matrix`, binary `shore-matrix`
  - future external integrations land here
- `dev/`
  - `mcp/` -> package `shore-mcp`, binary `shore-mcp`
  - `test-harness/` -> package `shore-test-harness`

Leave `docs/`, `examples/`, `experiments/`, `scripts/`, `contrib/`, and `TODO/` at repo root.

## Workspace Strategy

- Keep one root Cargo workspace for normal Rust crates so they share one lockfile, dependency graph, target cache, workspace dependency versions, and CI/release metadata.
- Add `workspace.default-members` for the common terminal-centric build path:
  - `core/protocol`
  - `core/config`
  - `core/swp-client`
  - `backend/daemon`
  - `clients/cli`
- Keep `clients/tui`, `bridges/matrix`, `clients/gui/src-tauri`, `dev/mcp`, and `dev/test-harness` as workspace members but outside default members.
- Keep `clients/gui-godot/rust` outside the root workspace by default. It may remain a nested standalone Cargo project because it depends on Godot/gdext tooling and has a special dynamic-library loading contract.
- Use explicit package selectors for release/package builds instead of relying on `cargo build --workspace --release` for shipped binaries:
  - `cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix`
- Keep `cargo build --workspace` and `cargo test --workspace` as full-repo verification gates, not the default local iteration path.

## Key Changes

- Rename internal crates before moving paths:
  - `shore-client` -> `shore-swp-client`; Rust imports `shore_client` -> `shore_swp_client`
  - `shore-daemon-server` -> `shore-swp-server`; Rust imports `shore_daemon_server` -> `shore_swp_server`
  - `shore-llm-client` -> `shore-llm`; Rust imports/log targets `shore_llm_client` -> `shore_llm`
- Preserve user-facing binaries and commands exactly: `shore`, `shore-daemon`, `shore-tui`, `shore-matrix`, `shore-gui`; `shore matrix ...` must continue delegating to `shore-matrix`.
- Keep CLI and daemon behavior separate; do not introduce direct-mode CLI shortcuts or daemon consolidation in this refactor.
- Enforce dependency direction:
  - `clients/*` and `bridges/*` use `core/*` APIs, not daemon internals.
  - `backend/daemon` owns runtime orchestration and may depend on `core/*` plus backend libraries.
  - `dev/*` may depend on `core/*` and selected backend crates for testing.
  - backend tests may keep dev/test helper dependencies where needed.
- Move the Tauri app intact under `clients/gui/`; keep frontend and `src-tauri` together so Tauri relative paths remain local to the app.
- Move the Godot client intact under `clients/gui-godot/`; preserve `libshore_bridge.so` output via `[lib] name = "shore_bridge"` or matching `.gdextension` updates.
- Update active docs and tooling: root `README.md`, `ARCHITECTURE.md`, `dev/mcp/README.md`, CI workflows, package/build scripts, `.cargo` aliases, and current contributor notes.
- Update `TODO/TODO.md` to mark the old CLI/daemon-consolidation idea as superseded. Leave historical `CHANGELOG.md` and `docs/DECISIONS.md` entries unchanged unless they are actively misleading outside historical context.

## Migration Order

1. Rename `shore-client`, `shore-daemon-server`, and `shore-llm-client` one at a time while paths are still stable. For each rename, update package names, dependency keys, Rust imports, log targets, tests, and current docs; verify compilation before the next rename.
2. Move crates into the new directory buckets and update Cargo path dependencies, root workspace members, and `workspace.default-members`.
3. Move `shore-gui/` to `clients/gui/` intact and verify Tauri frontend/Rust relative paths.
4. Move `shore-gui-godot/` to `clients/gui-godot/` without adding it to the root workspace; preserve the `shore_bridge` dynamic-library contract.
5. Update non-Rust path consumers: CI package selectors, `.cargo` aliases, packaging scripts, shell scripts, MCP docs, architecture docs, README, and contributor instructions.
6. Split CI into targeted lanes where useful: core/protocol guardrails, daemon/backend, clients, matrix bridge, GUI/Tauri, and full workspace verification.
7. Run broad verification, then do a final `rg` pass for stale active references to old crate names and old top-level paths.

## Test Plan

- Default local path: `cargo build` succeeds and builds the configured default members.
- Shipped binary build: `cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix` produces `shore-daemon`, `shore`, `shore-tui`, and `shore-matrix`.
- Full Rust gate: `cargo test --workspace` passes.
- CI guardrail commands are updated and pass for renamed packages: protocol, SWP server, SWP client, daemon, TUI, and Matrix.
- CLI smoke: `shore` discovers/connects to the daemon, and `shore matrix ...` still delegates to `shore-matrix`.
- Daemon smoke: `shore-daemon` still supervises `shore-matrix` using PATH/sibling binary lookup.
- MCP smoke: `.cargo` aliases still target `dev/mcp`, and daemon auto-spawn still finds `shore-daemon`.
- GUI smoke: `clients/gui/src-tauri` builds with frontend path assumptions intact.
- Godot smoke: building `clients/gui-godot/rust` still produces the library path expected by `shore_bridge.gdextension`.

## Assumptions

- This is a layout, workspace, and internal naming refactor only; SWP protocol, CLI behavior, daemon behavior, and product architecture remain unchanged.
- Compile-time improvement should come from default members, targeted package builds, and CI job boundaries, not from splitting the main Cargo workspace.
- Internal Rust package/crate names may change, but user-facing binary names do not.
- Godot remains experimental and outside the default root workspace unless a later plan explicitly opts into that cost.
- Historical changelog/decision records remain historical; only active docs and misleading current references are updated.
