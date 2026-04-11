# Daemon Lock Hardening Plan

Focused follow-up to [`refactor-plan.md`](./refactor-plan.md) for the four
remaining runtime `lock().unwrap()` sites we called out in the daemon.

## 1. Goal

Remove panic-on-poison behavior from the following runtime daemon paths without
changing the current concurrency model:

- `shore-daemon/src/commands/state.rs`
- `shore-daemon/src/memory/agent/tool_handlers.rs`
- `shore-daemon/src/memory/agent/types.rs`
- `shore-daemon/src/autonomy/manager.rs`

This is intentionally narrower than a full panic audit. The purpose here is to
eliminate a small set of high-value `Mutex` call sites that currently panic on
ordinary poison recovery paths.

## 2. Why This Slice Matters

- These are production code paths, not tests.
- They sit in command handling, memory search, and autonomy bookkeeping.
- A prior panic in unrelated code can poison the mutex and make later calls
  panic even when recovery would be acceptable.
- Shore already adopted a better pattern in `shore-ledger`, so the daemon
  should converge on the same policy instead of keeping ad hoc
  `lock().unwrap()` calls.

## 3. Target Call Sites

### A. Command status and diagnostics

File:

- `shore-daemon/src/commands/state.rs`

Current behavior:

- `status()` reads `session_tokens` with `lock().unwrap()`.
- `diagnostics()` reads `diagnostics` with `lock().unwrap()`.

Risk:

- A poisoned in-memory status or diagnostics mutex can turn a read-only command
  into a panic.

Desired end state:

- Both commands recover from poison using the shared helper.
- The functions remain read-only and keep the same output shape.

### B. BM25 search read path

File:

- `shore-daemon/src/memory/agent/tool_handlers.rs`

Current behavior:

- Semantic search reads the BM25 index with `ctx.bm25.lock().unwrap()`.

Risk:

- A poisoned search index can panic during memory lookup instead of returning
  results or a structured error.

Desired end state:

- The search path uses the shared helper.
- The lock scope remains short and contains no async work.

### C. BM25 population path

File:

- `shore-daemon/src/memory/agent/types.rs`

Current behavior:

- Lazy population of the BM25 index uses `self.bm25.lock().unwrap()`.

Risk:

- A poison event in indexing can make future population attempts panic.

Desired end state:

- Population uses the shared helper.
- Existing initialization order and `bm25_populated` behavior stay intact.

### D. Autonomy task handle bookkeeping

File:

- `shore-daemon/src/autonomy/manager.rs`

Current behavior:

- Spawned task handles are pushed with `self.handles.lock().unwrap()`.
- Shutdown drains handles with `self.handles.lock().unwrap()`.

Risk:

- A poisoned bookkeeping mutex can disrupt autonomy startup or shutdown.

Desired end state:

- Handle tracking uses the shared helper.
- Critical sections stay as small as they are now.

## 4. Implementation Plan

### Step 1: Add a shared daemon mutex helper

Create a small daemon-side helper that encodes one explicit poison policy:

- lock the mutex
- if the mutex is poisoned, log loudly with the resource name
- recover via `into_inner()` instead of panicking

Notes:

- Keep the helper local to the daemon crate.
- Match the spirit of `shore-ledger`'s `lock_or_recover()` helper so the repo
  has a consistent pattern for short synchronous critical sections.
- Do not switch these sites to `tokio::sync::Mutex`; that would be solving a
  different problem.

### Step 2: Migrate command-path locks

Replace the two `lock().unwrap()` uses in `shore-daemon/src/commands/state.rs`
with the shared helper.

Checks:

- `status()` still reports tokens exactly as before.
- `diagnostics()` still serializes the same JSON payload.

### Step 3: Migrate BM25 locks

Replace the BM25 `lock().unwrap()` sites in:

- `shore-daemon/src/memory/agent/tool_handlers.rs`
- `shore-daemon/src/memory/agent/types.rs`

Checks:

- Search still reads the index under a short lock.
- Population still builds the index synchronously and drops the lock before
  returning.
- No new lock is held across `.await`.

### Step 4: Migrate autonomy handle locks

Replace the handle-list `lock().unwrap()` sites in
`shore-daemon/src/autonomy/manager.rs`.

Checks:

- New handles are still recorded immediately after `tokio::spawn`.
- Shutdown still drains the list before awaiting each handle.
- Logging and shutdown semantics stay unchanged.

### Step 5: Add focused poison-recovery tests

Add tests that deliberately poison each mutex family once and verify the daemon
recovers rather than panicking.

Minimum coverage:

- command status/diagnostics mutex recovery
- BM25 populate/search mutex recovery
- autonomy handle list recovery

Test style:

- Use `catch_unwind` to poison the mutex in a controlled way
- then exercise the normal path
- assert the operation succeeds and preserves expected behavior

## 5. Non-Goals

- Do not remove every `unwrap()` in the daemon as part of this slice.
- Do not change startup fail-fast policy in unrelated modules.
- Do not redesign the autonomy or memory-agent architecture.
- Do not replace `Mutex` with a different synchronization primitive unless a
  later measurement-driven change justifies it.

## 6. Acceptance Criteria

This plan is complete when all of the following are true:

- No runtime `lock().unwrap()` remains in the four target daemon files.
- The daemon has one explicit poison-recovery policy for these shared-state
  mutexes.
- No affected code path holds the recovered lock across `.await`.
- New tests demonstrate recovery from poisoning for each targeted area.
- Relevant daemon tests pass in a normal unrestricted environment.

## 7. Follow-On Work

After this lands, use the same classification approach for other runtime
`unwrap()` / `expect()` sites:

- shared-service or command path: convert to recovery or structured error
- startup boundary: fail-fast may remain
- internal invariant: may remain if documented and well-tested

That keeps this change set focused while still moving the broader panic audit
forward.
