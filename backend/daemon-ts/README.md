# shore-daemon-ts

TypeScript reimplementation of `shore-daemon`. See `../../REWRITE.md` for the
plan.

## Current phase: 1 — distribution story

Phase 0 (scaffold + handshake) is done. Phase 1 validates that
`bun build --compile` produces an acceptable single-binary distribution.

## Run

```sh
bun install
bun run src/main.ts --addr 127.0.0.1:0
```

The daemon prints the resolved listen address and registers itself in
`$SHORE_RUNTIME_DIR/instances.json` (same as the Rust daemon) so the Rust
CLI can discover it.

## Build a single binary

```sh
bun run build       # → dist/shore-daemon
```

## Smoketest

```sh
bun run smoketest             # runs against `bun src/main.ts`
bun run smoketest:compiled    # runs against ./dist/shore-daemon
```

The smoketest spawns the daemon on `127.0.0.1:0`, opens a TCP connection,
and verifies the 3-step handshake (ServerHello → ClientHello → History).

## Parity check

`parity-traces/` holds recorded SWP exchanges from the Rust daemon (see
`scripts/capture-rust-trace.ts`). `parity-check.ts` re-runs the recorded
client side against our TS daemon and diffs the emitted server frames
against the baseline.

```sh
bun run parity              # diff against `bun src/main.ts`
bun run parity:compiled     # diff against ./dist/shore-daemon
bun run capture-trace parity-traces/<scenario>.jsonl   # regenerate baseline
```

Expected differences (e.g. `server_name`) are listed in
`EXPECTED_DIFFS` at the top of `scripts/parity-check.ts`; that list
should shrink toward empty as phases progress.

## Phase 1 observations (Arch Linux, x86_64, bun 1.3.14)

| Metric                  | Value                                       |
| ----------------------- | ------------------------------------------- |
| Build time              | ~100ms (default), ~1.6s (`bun-linux-x64-musl`) |
| Binary size             | 74 MB (glibc target)                        |
| Cold start to listening | ~21 ms                                      |
| RSS at idle             | ~57 MB                                      |
| Dynamic libs            | glibc, libstdc++, libgcc_s, libm, **libicu*.so.78** |

For comparison, the Rust `shore-daemon` is 18 MB and pins to `libssl.so.3`
+ `libcrypto.so.3`. Both binaries have the same operational-churn pattern:
they re-link against system libraries with major versions that move on
rolling-release distros. The Arch PKGBUILD will need `icu` in `depends`
when we ship the TS daemon.

The musl cross-compile (`--target=bun-linux-x64-musl`) produces an 87 MB
binary that statically bundles ICU but requires the musl loader at
runtime — no portability gain on a typical Linux host. Not worth pursuing
right now.

Linux/x86_64 is the only supported target. Shore is single-user and there
is no Mac to validate on.
