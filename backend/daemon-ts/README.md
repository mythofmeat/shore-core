# shore-daemon-ts

TypeScript reimplementation of `shore-daemon`. See `../../REWRITE.md` for the
plan.

## Status: cache regression killed, cutover-ready (2026-05-26)

The TS daemon is feature-complete and verified to fix the original
cache-invalidation regression on Sonnet 4.6 with adaptive thinking +
effort=high through a multi-iter tool loop and a follow-up turn (see
`tests/cache_regression.test.ts`). The daemon can handshake with
existing clients, persist messages, generate through provider SDKs,
run the full tool registry, compact memory, apply deferred prompt
edits, use the workspace embedding index, run AI-librarian dreaming,
write the usage ledger, track activity heatmaps, and drive
heartbeat/keepalive autonomy from the ticker.

Remaining work is the cutover itself: opt-in soak, default switch,
Rust daemon retirement. The cutover runbook lives at
`../../docs/DAEMON_TS_CUTOVER.md`.

Representative checks currently green:
- `handshake-empty` — no character selected.
- `handshake-character` — single character with seeded messages.
- `message-append` — client sends a message, restart, verify persistence.
- `bun run typecheck`
- `bun test` — unit/integration suite; provider-live tests are env-gated.

## Run

```sh
bun install
bun run src/main.ts --addr 127.0.0.1:0
bun run src/main.ts --config /path/to/config.toml --addr 127.0.0.1:0
```

The daemon prints the resolved listen address and registers itself in
`$SHORE_RUNTIME_DIR/instances.json` (same as the Rust daemon) so the Rust
CLI can discover it.

## Build a single binary

```sh
bun run build       # → dist/shore-daemon
```

The preview Arch package installs that binary as `shore-daemon-ts` so it can
live beside the Rust `shore-daemon`. See `../../contrib/shore-daemon-ts/`.

Preview package releases are published from `shore-daemon-ts-v*` tags by the
shared Arch packaging workflow. Use an Arch-safe tag suffix such as
`shore-daemon-ts-v0.0.0_preview`; `makepkg` rejects hyphens in `pkgver`.

## Opt-in systemd service

```sh
systemctl --user enable --now shore-daemon-ts.service
```

Do not run `shore-daemon.service` and `shore-daemon-ts.service` at the same
time against the same Shore directories unless you intentionally want two
daemon instances in the runtime registry.

## Smoketest

```sh
bun run smoketest             # runs against `bun src/main.ts`
bun run smoketest:compiled    # runs against ./dist/shore-daemon
```

The smoketest spawns the daemon on `127.0.0.1:0`, opens a TCP connection,
and verifies the 3-step handshake (ServerHello → ClientHello → History).

## Parity check

`parity-traces/` holds two kinds of baselines:

- `parity-traces/*.jsonl` — recorded SWP client transcripts replayed
  against the TS daemon by `parity-check.ts` (handshake / append /
  multi-turn / edit / delete / alt / commands).
- `parity-traces/frozen/*.json` — TS-vs-self regression baselines for
  generation, regen, tool-loop, inline-compaction, heartbeat-tick,
  dreaming, and scheduled-dreaming. Each script accepts
  `--baseline <path>` (default for the regression run) and
  `--write-baseline <path>` (regenerate after an intentional change).

```sh
bun run parity                          # T1/T2 SWP replays
bun run parity:generation               # T3 generation vs frozen baseline
bun run parity:regen[:cached]           # ... regen, etc.
bun run parity:tool-loop[:cached]
bun run parity:compaction[:cached]
bun run parity:heartbeat-tick[:cached]
bun run parity:dreaming[:cached]
bun run parity:scheduled-dreaming[:cached]
```

Each `parity:<name>` has a `:compiled` twin that runs against
`./dist/shore-daemon` instead of `bun src/main.ts`. Wall-clock time
markers are redacted via `redactHeartbeatMarkers` in `scripts/parity/_lib.ts`
so baselines survive minute-crossing reruns.

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
