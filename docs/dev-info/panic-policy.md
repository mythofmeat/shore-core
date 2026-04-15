# Panic Policy

This note records the panic-handling policy used for the refactor-plan hardening pass.
It is intentionally a policy note, not a whole-repo inventory of every `unwrap()` or
`expect()` in production code.

## Default Rule

- Shared-service and ordinary runtime paths should not rely on `unwrap()` or `expect()`.
  Return a structured error, or recover and log loudly when that is the chosen policy.
- Startup or process-fatal boundaries may fail fast when continuing would leave Shore
  partially initialized or unable to shut down safely.
- Module-owned invariants may still panic when the module itself creates and owns the
  invariant. If we later decide that corruption or dependency drift should be
  recoverable, these should become structured errors instead.

## Applied In This Refactor Slice

- `shore-ledger` shared-state locking follows the first rule: poisoned mutexes recover
  through `lock_or_recover()` instead of panicking in production request paths.
- `shore-daemon/src/main.rs` signal-registration `expect()` calls are classified as
  startup-fatal. If the daemon cannot install shutdown handlers, it should fail fast
  instead of running half-managed.
- `shore-daemon/src/memory/vectorstore.rs` Arrow record-batch `expect()` calls are
  classified as invariant-protecting. The module creates the `vectors` table schema
  itself, and the query paths expect LanceDB to return that owned schema plus the
  `_distance` column for nearest-neighbor results.

## Guidance For Future Changes

- If a new panic site can be triggered by an ordinary request, background job, or
  recoverable I/O failure, do not use panic-based handling.
- If you keep `unwrap()` or `expect()`, document which bucket it belongs to:
  startup-fatal or invariant-protecting.
- Prefer comments or panic messages that make the classification explicit at the call
  site, so future audits do not have to reconstruct intent from scratch.
