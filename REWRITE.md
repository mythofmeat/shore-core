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
- [ ] **Live-API validation of every T3 fixture.** All T3 parity checks
  today run against `scripts/parity/llm-proxy.ts` serving canned
  responses. That proves logic parity *assuming the mock matches
  reality* — and the original cache regression that motivated this
  rewrite was precisely a mock-vs-real divergence, so passing-on-mock
  is not proof. Before preview soak, run each T3 check once in
  forward-record mode (real provider URL, `recordMissing: true`) with
  real API keys; both daemons must still agree on the canonical request
  body, and the recorded response gets committed as the canned fixture
  for the mock-mode CI run going forward. Covers Anthropic +
  OpenAI-compatible + (when ported) OpenRouter. See
  [`docs/DAEMON_TS_PARITY.md`](docs/DAEMON_TS_PARITY.md) "Infrastructure
  work" → "Live-API validation pass" for the runbook once all T3 checks
  are in place.
- [ ] **Cache-breakpoint placement parity (must-fix before soak).**
  Inline-compaction parity check with `cache_ttl = "1h"` surfaced
  divergent breakpoint placement on the chat call: Rust marks system
  block 1 (`tools_guidance`) and the second-to-last stable *user*
  message; TS marks system block 2 (`character`) and the second-to-last
  stable *assistant* message. Same conversation, different cache keys —
  cross-daemon cache fragmentation, and the "correct" placement strategy
  itself needs validation against real Anthropic before either daemon
  ships. Resolution requires live API runs (cache_creation vs
  cache_read accounting on a real conversation) to determine which
  strategy actually wins; document the answer in
  `docs/DAEMON_TS_PARITY.md` and bring both daemons into agreement.
  Listed in DAEMON_TS_PARITY.md "Known divergences" as the canonical
  reference.
- [x] **Audit all T3 fixtures for `cache_ttl = ""` blind spot
  (done 2026-05-26).** Every pre-2026-05-25 T3 fixture
  (`generation-basic`, `regen-basic`, `tool-loop-read`, original
  `inline-compaction`) disabled caching via `cache_ttl = ""`. This
  skipped the breakpoint-placement + label-strip code paths entirely
  — which is how the `_label` wire leak and breakpoint divergence
  both shipped without detection. Each T3 parity script now accepts
  `--cache-ttl <value>`; package.json carries paired `:cached`
  entries (`parity:<name>:cached[:compiled]`) that pass `1h`. Sweep
  surfaced two new TS-only divergences on top of the known system /
  stable-message breakpoint placement diffs — see "Known divergences"
  in `docs/DAEMON_TS_PARITY.md` for the full triage and resolution
  plan. The fixes themselves are bundled with the live-API
  breakpoint-placement gate below.

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
