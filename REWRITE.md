# Daemon Rewrite — Rust → TypeScript (Bun)

**Status (2026-05-25):** parity gaps closed. Phase 9b soak + cutover
remaining.

All Phase 9b parity gaps from the 2026-05-24 audit have been ported or
explicitly descoped. The TS daemon is feature-complete relative to the
Rust daemon for the user's actual usage. What's left is automated parity
coverage, preview soak, default switch, and Rust retirement.

Historical detail — phased plan, audit findings, architecture, hard
constraints, decision rationale for descoped items, porting wisdom —
lives in [`docs/DAEMON_TS_REWRITE_HISTORY.md`](docs/DAEMON_TS_REWRITE_HISTORY.md).
Cutover runbook: [`docs/DAEMON_TS_CUTOVER.md`](docs/DAEMON_TS_CUTOVER.md).
Parity coverage build-out: [`docs/DAEMON_TS_PARITY.md`](docs/DAEMON_TS_PARITY.md).

## What's left

### Pre-soak

- [ ] **Update stale documentation.** Various docs still frame the
  rewrite as in-progress or assume the Rust daemon is the only daemon.
  Known offenders: `backend/daemon-ts/README.md` ("Current phase: 8d
  complete — cutover prep" is stale; phase 9b is done), top-level
  `README.md` + `ARCHITECTURE.md` (Rust-daemon-centric framing — the TS
  daemon is currently described as a preview gate, will need real
  parity rewriting at cutover), `AGENTS.md` (entry-map should mention
  daemon-ts as the live target). Pass once now to fix the obviously
  stale lines, then a second pass at cutover to rewrite the framing.
- [ ] **Automated parity coverage lands.** The existing harness only
  covers handshake, one message-append, and offline prompt-assembly. The
  cutover gate ("one full release cycle with no live failures") is a
  user-observation parity test; this work makes it a defense-in-depth
  observation, not the only signal. Tier breakdown + infrastructure plan
  in [`docs/DAEMON_TS_PARITY.md`](docs/DAEMON_TS_PARITY.md) (T1
  persistence flows, T2 command dispatcher round-trips, T3 content-level
  parity via LLM proxy stub).

### Soak + cutover

- [ ] **Preview soak starts.** Merge the rewrite branch to `origin/main`,
  publish a `shore-daemon-ts-v*` tag from that main commit, verify the
  repo-arch package, install/run `shore-daemon-ts.service`, record start
  evidence per the cutover runbook.
- [ ] **Preview soak completes.** One full release cycle of opt-in TS
  daemon traffic finishes with no live failures attributable to the TS
  daemon. Any code fix for a live TS-daemon failure restarts the soak
  clock from the fixed preview release.
- [ ] **Default switch lands.** `shore-daemon-ts` becomes the default
  daemon package/service path, with migration and rollback notes
  captured in the cutover PR.
- [ ] **Rust daemon retired.** `backend/daemon` is moved to `attic/` or
  deleted by the cutover decision.

**Exit criterion:** preview soak complete, TS daemon is the default, and
the Rust daemon is retired.
