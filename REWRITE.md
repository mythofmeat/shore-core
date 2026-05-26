# Daemon Rewrite — Rust → TypeScript (Bun)

**Status (2026-05-26):** Cache regression — the original motivation
for the rewrite — verified killed on Sonnet 4.6 with adaptive
thinking + effort=high through a multi-iter tool loop and a
follow-up turn. cache_read grew monotonically across all calls; the
only large cache_write was the cold start. Receipt:
[[project-cache-regression-killed]] memory + the
`adaptive + effort:high through tool loop + follow-up` test in
`tests/cache_regression.test.ts`.

The TS daemon is feature-complete relative to the Rust daemon and
exceeds it on the cache-preservation property the rewrite was built
to fix. The pre-soak parity gates that compared TS against Rust are
moot now that we've decided to retire Rust outright; the upcoming
"freeze examples" work converts the parity harness from a cross-daemon
diff into TS-vs-frozen-baseline regression coverage. What's left is
that conversion, the soak itself, and Rust retirement.

Historical detail — phased plan, audit findings, architecture, hard
constraints, decision rationale for descoped items, porting wisdom —
lives in [`docs/DAEMON_TS_REWRITE_HISTORY.md`](docs/DAEMON_TS_REWRITE_HISTORY.md).
Cutover runbook: [`docs/DAEMON_TS_CUTOVER.md`](docs/DAEMON_TS_CUTOVER.md).

## What's left

### Pre-soak

- [ ] **Update stale documentation.** Various docs still frame the
  rewrite as in-progress or assume the Rust daemon is the only daemon.
  Known offenders: `backend/daemon-ts/README.md` ("Current phase: 8d
  complete — cutover prep" is stale), top-level `README.md` +
  `ARCHITECTURE.md` (Rust-daemon-centric framing — the TS daemon is
  currently described as a preview gate, will need real rewriting at
  cutover), `AGENTS.md` (entry-map should mention daemon-ts as the
  live target). Pass once now to fix the obviously stale lines, then a
  second pass at cutover to rewrite the framing.
- [ ] **Freeze parity examples against the TS daemon.** The current
  parity-check scripts under `backend/daemon-ts/scripts/parity-check-*.ts`
  spawn both Rust and TS and diff them at runtime. Once Rust is going
  away, the comparison flips: TS becomes the reference, with frozen
  JSONL/JSON baselines captured from the current TS daemon. Re-capture
  every T1/T2/T3 fixture against the current TS daemon, rewrite the
  check scripts to diff against the committed baseline (drop the
  `--rust` flag and the proxy-intercept comparator), and drop
  `docs/DAEMON_TS_PARITY.md` once everything is converted (the parity
  doc exists to track cross-daemon parity; TS-vs-self regression is
  just standard testing). First slice landed 2026-05-26:
  `parity:generation` and `parity:generation:openai` now diff TS
  against frozen committed baselines under
  `backend/daemon-ts/parity-traces/frozen/`, pinning both SWP
  response summaries and provider request bodies without requiring
  live API calls or the Rust daemon.
- [x] **OpenAI-compatible adapter live-test coverage (done
  2026-05-26).** The
  Anthropic adapter is locked down by `tests/cache_regression.test.ts`
  on Sonnet 4.6. `scripts/live-tests/openrouter-sdk-parity.sh`
  sources `~/.config/shore/.env`, exercises OpenRouter
  `openai/gpt-5.4-mini` through the OpenAI-compatible SDK with the
  same send/regen/tool/log/model-info assertions as the Anthropic SDK
  path, and verifies mid-chat switching in both directions
  (Anthropic→OpenAI-compatible and OpenAI-compatible→Anthropic).
  Live receipt on 2026-05-26: 31/31 checks passed.
- [x] **Cache regression verified dead on Sonnet 4.6 (done
  2026-05-26).** The original motivation for the rewrite — Rust's
  cache-invalidation on adaptive thinking + multi-iter tool loop +
  follow-up turn — is verified killed. See
  `tests/cache_regression.test.ts` and the
  [[project-cache-regression-killed]] memory for the receipt.
- [x] **Automated parity coverage in place (done 2026-05-26).** T1
  persistence flows, T2 command dispatcher, and T3 content-level
  parity are all covered by scripts under
  `backend/daemon-ts/scripts/parity-check-*.ts`. The pre-2026-05-26
  cross-daemon comparison is being converted to TS-vs-frozen-baseline
  by the "Freeze parity examples" item above.
- [x] **Cache-breakpoint placement decided (done 2026-05-26).** The
  Rust/TS placement difference is resolved by retiring Rust and
  shipping the TS placement: system breakpoint on the last system
  block whose label is not `memory_index`, message breakpoints on
  the stable assistant turn + the tail message, no tools breakpoint
  (system breakpoint covers tools via Anthropic's
  tools→system→messages evaluation order). Verified on Sonnet 4.6
  with adaptive + effort=high.
- [x] **Audit all T3 fixtures for `cache_ttl = ""` blind spot
  (done 2026-05-26).** Every pre-2026-05-25 T3 fixture
  (`generation-basic`, `regen-basic`, `tool-loop-read`, original
  `inline-compaction`) disabled caching via `cache_ttl = ""`. This
  skipped the breakpoint-placement + label-strip code paths entirely
  — which is how the `_label` wire leak and breakpoint divergence
  both shipped without detection. Each T3 parity script now accepts
  `--cache-ttl <value>`; package.json carries paired `:cached`
  entries (`parity:<name>:cached[:compiled]`) that pass `1h`.

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
