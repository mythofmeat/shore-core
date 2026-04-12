# Shore Refactor Plan

Synthesis of [`review.md`](./review.md) and [`review-counterpoint.md`](./review-counterpoint.md).

## 1. Working Conclusion

Both reviews are usefully true in different ways.

- The audit is directionally right that Shore has real hardening work to do around blocking maintenance I/O, mutex poison handling, duplicated vector-store predicate construction, and panic-heavy shared-service code.
- The counter-review is also right that the current system is not a design collapse. Most of the worrying sync work lives in maintenance/background paths, and the code already preserves several important invariants through atomic writes, compensating rollback, config validation, short lock scopes, and focused tests.

The right response is therefore not a wholesale async rewrite. It is a targeted refactor for resilience and scale-readiness that preserves the current single-process architecture and its existing strengths.

## 2. Refactor Goals

1. Make maintenance-path blocking work explicit, measurable, and isolated from interactive async work.
2. Remove panic-on-poison behavior from core shared services.
3. Eliminate duplicated stringly predicate construction in the vector store.
4. Clarify which `unwrap()` / `expect()` calls are true invariants and which should become structured errors.
5. Preserve the current safety properties already present in compaction, file mutation, and lock scoping.

## 3. Non-Goals

- Do not convert the entire codebase from `std::fs` to `tokio::fs`.
- Do not replace every `std::sync::Mutex` with `tokio::sync::Mutex`.
- Do not remove fail-fast startup assertions just because they are `expect()`.
- Do not redesign compaction's rollback model, segment format, or file layout unless measurements show a concrete problem.
- Do not treat "sync in async exists" as proof that the architecture is wrong for Shore's workload.

## 4. Existing Strengths To Preserve

The refactor should explicitly preserve the following behavior, because these are already good design choices:

- `shore-daemon/src/engine/atomic.rs` uses temp-file-plus-rename writes.
- `shore-daemon/src/memory/compaction/mod.rs` tracks created resources and rolls them back in reverse order on failure.
- `shore-daemon/src/memory/compaction_impls.rs` uses pre-read `active_content` to avoid TOCTOU races during compaction.
- `shore-daemon/src/autonomy/manager.rs` already demonstrates the right lock discipline: collect data under the lock, drop it, then do async work.
- The current test suite already covers several compaction and vector-store invariants; the refactor should extend those tests, not replace them with hand-wavy safety claims.

## 5. Proposed Workstreams

### Workstream A: Add Measurement Before Concurrency Churn

Target files:

- `shore-daemon/src/memory/compaction/mod.rs`
- `shore-daemon/src/memory/compaction_impls.rs`
- `shore-daemon/src/memory/vectorstore.rs`
- `shore-ledger/src/ledger.rs`
- `shore-ledger/src/pricing.rs`

Actions:

- Add tracing spans and elapsed-time logging around compaction phases, especially:
  - LLM summarize
  - DB entry creation loop
  - vector indexing loop
  - archive/retain file mutation step
- Add similar timing around vector-store open/reindex and ledger/pricing cache fetches.
- Add at least one repeatable regression harness or integration test that exercises background compaction while other async work remains responsive.

Why first:

- This resolves the core dispute between the two reviews. We should stop arguing abstractly about whether sync-in-async is severe and start measuring where Shore actually stalls.

Exit criteria:

- We can point to concrete timing for compaction and vector-store maintenance paths.
- We have a baseline for "interactive request remains responsive while maintenance runs."

### Workstream B: Harden Shared-Service Locking Without Changing The Concurrency Model

Target files:

- `shore-ledger/src/ledger.rs`
- `shore-ledger/src/pricing.rs`

Actions:

- Replace `lock().unwrap()` on the ledger connection and pricing memory cache with a single explicit policy:
  - recover from poisoning and log loudly, or
  - convert poisoning into a structured fatal error at the API boundary.
- Centralize lock acquisition behind helper methods instead of repeating ad hoc `lock().unwrap()` calls.
- Narrow the public surface that exposes raw `MutexGuard<Connection>`. Prefer closure-based helpers or a small internal API so lock handling stays consistent.
- Add tests for the chosen poison-handling behavior.

Important constraint:

- Keep `std::sync::Mutex` for these short, synchronous critical sections unless measurements later prove contention is a real bottleneck. A switch to `tokio::sync::Mutex` would add complexity without solving the main resilience issue.

Why this is high priority:

- Unlike compaction, `shore-ledger` sits in a core shared-service path. Poison-triggered panics here have broader availability impact than background maintenance work.

Exit criteria:

- No production `lock().unwrap()` remains in `shore-ledger`.
- The ledger and pricing layers have a documented, tested poison-handling policy.

### Workstream C: Centralize Vector-Store Predicate Construction

Target files:

- `shore-daemon/src/memory/vectorstore.rs`

Actions:

- Introduce one internal helper for building entry-ID predicates.
- Use that helper for:
  - stale-row deletion during `index_entry()`
  - `delete_entry()`
  - `get_embeddings()` `IN (...)` queries
- Make ID validation rules explicit in one place instead of duplicating them at some call sites and omitting them at others.
- If LanceDB exposes a safer parameterized or structured filtering API, prefer it. If not, centralize escaping/validation in a single helper and test it thoroughly.

Why this matters:

- The practical issue today is consistency and future-proofing more than an immediate exploit path. The current code already constrains some IDs, but the construction pattern is duplicated and unevenly hardened.

Exit criteria:

- No ad hoc `format!("entry_id ...")` predicate assembly remains in vector-store operations.
- Unsafe or malformed IDs fail in one consistent way.

### Workstream D: Move Heavy Maintenance File I/O Behind An Explicit Boundary

Target files:

- `shore-daemon/src/memory/compaction/mod.rs`
- `shore-daemon/src/memory/compaction/types.rs`
- `shore-daemon/src/memory/compaction_impls.rs`
- `shore-daemon/src/memory/vectorstore.rs`

Actions:

- Treat `RealConversationManager::archive_and_retain()` as blocking maintenance work and move its invocation off the main async executor.
- Preferred short-term approach:
  - keep the file-mutation logic itself mostly synchronous
  - add an async adapter or `spawn_blocking` boundary at the compaction orchestration layer
  - preserve current rollback, TOCTOU, and atomic-write semantics
- Keep `VectorStore::open()` directory creation and any other obviously blocking setup steps small and explicit. Convert low-risk setup calls opportunistically, but do not start a blanket filesystem rewrite.
- Revisit `reindex()` only if measurements show it meaningfully affects interactive latency.

Why this is the right scope:

- The original review is right that long synchronous file work should not quietly run on a Tokio worker.
- The counter-review is right that this mostly lives in maintenance paths.
- The synthesis is: isolate the maintenance boundary, do not rewrite the whole daemon around it.

Implementation note:

- If the current sync `ConversationManager` trait makes `spawn_blocking` awkward, prefer a small interface adjustment that preserves testability over scattering blocking wrappers through call sites.

Exit criteria:

- Compaction archive/retain disk writes no longer run inline on the core async executor thread.
- Existing rollback and file-consistency tests still pass.

### Workstream E: Audit Panics By Category, Not By Raw Count

Target files:

- `shore-daemon/src/main.rs`
- `shore-daemon/src/memory/vectorstore.rs`
- `shore-ledger/src/*.rs`
- other production `src/` modules as found by audit

Actions:

- Classify each remaining `unwrap()` / `expect()` into one of three buckets:
  - recoverable shared-service path: replace with `Result` propagation or explicit logging + error return
  - startup/process-fatal boundary: may remain fail-fast
  - internal invariant owned by the module: may remain, but document the invariant
- Prioritize shared services first, not startup wiring and not schema assumptions that the module itself owns.
- Add a short developer note or decision record capturing this panic policy so future changes follow the same rules.

Why this is better than "remove all unwraps":

- Some panics are resilience bugs.
- Some encode valid process-fatal assumptions.
- Some protect internal invariants and are acceptable if they are local, documented, and tested.

Exit criteria:

- Remaining production panics are intentional and categorized.
- Shared-service panics caused by ordinary runtime failure are removed first.

### Workstream F: Re-Evaluate Only After The Low-Risk Hardening Lands

Possible follow-up work, only if warranted by instrumentation:

- introduce a dedicated maintenance executor or job queue for compaction
- switch specific sync-only hotspots to `parking_lot::Mutex`
- further split compaction orchestration from file mutation
- deeper async conversion in vector-store or filesystem-heavy code

Why this is last:

- These are architectural shifts. They should be earned by measurements, not pulled in preemptively.

## 6. Recommended Sequence

1. Land instrumentation and responsiveness checks.
2. Harden `shore-ledger` and `pricing` lock handling.
3. Centralize vector-store predicate construction.
4. Isolate compaction archive/retain behind an explicit blocking boundary.
5. Audit and classify remaining production panics.
6. Re-measure before considering larger concurrency changes.

This order front-loads the lowest-risk, highest-confidence improvements and delays architectural churn until data says it is worth doing.

## 7. Acceptance Criteria For The Refactor

The refactor should be considered successful when all of the following are true:

- `shore-ledger` no longer relies on production `lock().unwrap()` for shared state.
- vector-store predicate construction is centralized and consistently validated.
- compaction's archive/retain file writes are isolated from the main async executor.
- Shore has observable timings for compaction and maintenance operations.
- remaining `unwrap()` / `expect()` sites are documented as startup-fatal or invariant-protecting, not accidental availability hazards.
- existing safety properties still hold:
  - atomic file replacement
  - reverse-order rollback
  - no lock held across `.await` in async orchestration paths

## 8. Practical Summary

The synthesis is not "the audit was alarmist" and not "the counter-review says do nothing."

It is:

- keep the overall architecture
- harden the shared-service footguns first
- isolate clearly blocking maintenance work
- centralize unsafe-looking string construction
- use measurements to decide whether deeper async/concurrency changes are justified

That gives Shore a refactor plan that is realistic, incremental, and aligned with how the codebase already wants to work.
