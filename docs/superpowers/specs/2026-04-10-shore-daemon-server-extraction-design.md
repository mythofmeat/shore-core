# Design: Extract `shore-daemon-server` crate

**Date:** 2026-04-10
**Status:** Approved
**Approach:** Flat crate extraction (Approach A)

## Summary

Extract `shore-daemon/src/server/` (mod.rs + registry.rs, ~1,317 LOC) into a new top-level workspace crate `shore-daemon-server`. The server module has zero internal dependencies on other shore-daemon modules, making this the cleanest possible extraction.

## Crate Structure

```
shore-daemon-server/
├── Cargo.toml
└── src/
    ├── lib.rs        # Server, ServerConfig, ClientInfo, RoutedMessage
    └── registry.rs   # Registry, InstanceInfo
```

## Public API

All currently-public types move unchanged:

- `Server` — SWP listener (Unix socket + TCP), client handshake, message routing
- `ServerConfig` — Socket path, optional TCP config, server name
- `ClientInfo` — Connected client metadata (id, type, name, capabilities, character)
- `RoutedMessage` — Internal routing enum (Engine, Command, AllClientsDisconnected)
- `Registry` — File-locked instance registry for daemon discovery
- `InstanceInfo` — Single daemon instance entry

## Dependencies

```toml
[dependencies]
shore-protocol = { path = "../shore-protocol" }
shore-config = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
fs2 = "0.4"
libc = { workspace = true }
```

## Changes to `shore-daemon`

1. Remove `pub mod server` from `src/lib.rs`
2. Delete `src/server/` directory
3. Add `shore-daemon-server` dependency to `Cargo.toml`
4. Update imports in two files:
   - `src/main.rs`: `use shore_daemon::server::*` → `use shore_daemon_server::*`
   - `src/handler/mod.rs`: `use crate::server::RoutedMessage` → `use shore_daemon_server::RoutedMessage`

## Design Decisions

- **RoutedMessage stays in shore-daemon-server:** It's a server-internal routing concern, not a wire protocol type. Handler already needs to depend on the server crate for `Server`/`ServerConfig`, so `RoutedMessage` rides along for free.
- **Registry stays as a submodule:** At 221 LOC, it doesn't justify its own crate. If clients ever need daemon discovery without the full server, extract then.
- **No API changes:** Pure mechanical move. All types, methods, and tests transfer as-is.
- **No trait abstractions:** Direct struct usage, no indirection needed.

## What Doesn't Change

- Wire protocol (SWP) — lives in `shore-protocol`
- All existing tests move with the code
- `shore-daemon` dev-dependencies unchanged
- No behavioral changes to any binary
