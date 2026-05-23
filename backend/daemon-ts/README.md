# shore-daemon-ts

TypeScript reimplementation of `shore-daemon`. See `../../REWRITE.md` for the
plan.

## Current phase: 0 — Bun scaffold + SWP handshake echo

The daemon accepts a TCP connection, completes the SWP handshake, and sends
an empty `History` snapshot. Nothing else works yet. The Rust CLI talking
against this daemon should connect, see the empty conversation, and exit
cleanly.

## Run

```sh
bun install
bun run src/main.ts --addr 127.0.0.1:0
```

The daemon prints the resolved listen address and registers itself in
`$SHORE_RUNTIME_DIR/instances.json` (same as the Rust daemon) so the Rust
CLI can discover it.

## Test (Phase 0)

```sh
# Start the TS daemon
bun run src/main.ts --addr 127.0.0.1:0 &

# Connect via the Rust CLI (any read-only command will do)
shore log --limit 1
```

The handshake should complete and the CLI should print the (empty) history.
