# Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all P0-P2 findings from the adversarial code review, prioritized by data integrity risk.

**Architecture:** Seven independent fixes, all within `shore-daemon`. Each task targets a specific defect class: data races, non-atomic writes, resource leaks, and silent error suppression. No cross-task dependencies except Tasks 1 and 3 which both touch `active.jsonl` write paths.

**Tech Stack:** Rust stable, tokio, thiserror, rusqlite, reqwest, tempfile (new dev-dependency for atomic writes)

---

## File Map

| Task | Files Modified | Files Created |
|------|---------------|---------------|
| 1 — Autonomous race | `autonomy/manager.rs` | — |
| 2 — Compaction atomicity | `memory/compaction/mod.rs`, `memory/compaction/types.rs` | — |
| 3 — Atomic file writes | `engine/messages.rs`, `memory/compaction_impls.rs` | `engine/atomic.rs` |
| 4 — Generation abort | `handler.rs` | — |
| 5 — Collation indexing errors | `memory/collation/mod.rs` | — |
| 6 — LedgerStream finalization | `engine/tools.rs`, `handler.rs` | — |
| 7 — embed_text cleanup | `memory/vectorstore.rs` | — |

All paths relative to `shore-daemon/src/`.

---

## Task 1: Fix Autonomous Message Persistence Race (P0)

**Finding:** 3.2.2 / 2.1.1 — `manager.rs:1034-1042` appends to `active.jsonl` via raw `std::fs::OpenOptions::append()` without the engine lock. `handler.rs` `persist()` does full-file rewrites from in-memory state. These two paths race, causing data loss.

**Files:**
- Modify: `shore-daemon/src/autonomy/manager.rs:1034-1043`
- Modify: `shore-daemon/src/handler.rs` (the engine lock acquisition around persist)

**Root cause:** The autonomy manager bypasses the engine's `MessageStore` entirely, writing directly to the backing file. The fix is to route autonomous messages through the engine's `append_message()` API, which holds the lock and updates in-memory state before persisting.

- [ ] **Step 1: Read the autonomy manager's `persist_autonomous_message` context**

Read `manager.rs:990-1060` to understand the full method that contains the raw file append. Identify what data is available: the `engine_arc: Arc<Mutex<Engine>>` is already in scope (passed to the autonomy manager at construction).

- [ ] **Step 2: Write a test for autonomous message persistence through the engine**

In `manager.rs` tests section, add a test that:
1. Creates an `Engine` with an `Arc<Mutex<>>` wrapper
2. Appends a message via the engine lock
3. Reads back from `active.jsonl`
4. Confirms the message appears and the file is valid JSONL

```rust
#[tokio::test]
async fn autonomous_message_persists_through_engine() {
    let dir = tempfile::tempdir().unwrap();
    let active_path = dir.path().join("active.jsonl");
    // Set up engine with MessageStore pointing at active_path
    // ... (use existing test helpers from the module)

    let engine_arc = Arc::new(tokio::sync::Mutex::new(engine));

    let msg = Message {
        msg_id: "auto-1".to_string(),
        role: Role::Assistant,
        content: "autonomous thought".to_string(),
        images: vec![],
        content_blocks: vec![],
        alt_index: None,
        alt_count: None,
        timestamp: chrono::Local::now().to_rfc3339(),
    };

    {
        let mut engine = engine_arc.lock().await;
        engine.append_message(msg.clone()).unwrap();
    }

    let contents = std::fs::read_to_string(&active_path).unwrap();
    assert!(contents.contains("autonomous thought"));
}
```

- [ ] **Step 3: Run the test to confirm it fails**

Run: `cargo test -p shore-daemon autonomous_message_persists_through_engine`
Expected: FAIL (test infrastructure may need adjustment — the engine setup is the unknown)

- [ ] **Step 4: Replace raw file append with engine lock + append_message**

In `manager.rs:1034-1043`, replace:

```rust
// BEFORE (raw file append, races with handler persist):
if let Ok(line) = msg.serialize_for_storage() {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&active_path)
    {
        let _ = writeln!(f, "{line}");
    }
}
```

With:

```rust
// AFTER (route through engine, which holds lock and persists atomically):
if let Some(engine_arc) = engine_arc.as_ref() {
    let mut engine = engine_arc.lock().await;
    if let Err(e) = engine.append_message(msg.clone()) {
        error!(error = %e, "Failed to persist autonomous message through engine");
    }
}
```

This requires the `engine_arc` to be available in this scope. Verify it is passed to the method or accessible on `self`.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p shore-daemon autonomous_message_persists_through_engine`
Expected: PASS

- [ ] **Step 6: Run full crate tests**

Run: `cargo test -p shore-daemon`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add shore-daemon/src/autonomy/manager.rs shore-daemon/src/handler.rs
git commit -m "fix(autonomy): route autonomous messages through engine to prevent active.jsonl race"
```

---

## Task 2: Add Rollback to Compaction Pipeline (P0)

**Finding:** 3.1.1 — `compaction/mod.rs:250-282` performs SQLite INSERT, LanceDB index, changelog append, and `archive_and_retain` as non-transactional steps. Partial failure leaves inconsistent state.

**Files:**
- Modify: `shore-daemon/src/memory/compaction/mod.rs:250-282`
- Modify: `shore-daemon/src/memory/compaction/types.rs` (add rollback error variant if needed)

**Strategy:** Wrap the SQLite operations in a transaction. For the cross-system steps (LanceDB indexing, filesystem archive), implement compensating deletes on failure. The key insight: SQLite entries can be rolled back via transaction; LanceDB entries and filesystem writes need explicit cleanup.

- [ ] **Step 1: Write a test that verifies rollback on archive_and_retain failure**

In `compaction/mod.rs` tests, add a test using a mock `ConversationManager` that returns `Err` from `archive_and_retain`. Verify that no SQLite entries remain after the error.

```rust
#[tokio::test]
async fn compaction_rolls_back_on_archive_failure() {
    // Set up db, indexer, and a FailingConversationManager
    // Run compact()
    // Assert: db.get_entry() returns None for the would-be entry IDs
    // Assert: no changelog entries created
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test -p shore-daemon compaction_rolls_back_on_archive_failure`
Expected: FAIL — entries persist in SQLite despite archive failure

- [ ] **Step 3: Wrap SQLite operations in a transaction**

The `db: &dyn MemoryDb` trait likely doesn't expose transaction control directly. Check the trait definition. If it uses rusqlite, the approach is:

1. Collect all entries to create
2. Begin SQLite transaction
3. Create entries + changelog within transaction
4. Index to vector store
5. Call `archive_and_retain`
6. If either 4 or 5 fails: rollback SQLite transaction + delete any LanceDB entries created
7. If both succeed: commit SQLite transaction

```rust
// Pseudocode for the modified compact() method:
let txn = db.begin_transaction()?;

for ce in &compaction_entries {
    txn.create_entry(&entry)?;
    entry_ids.push(entry_id.clone());
}

// Index all entries (cross-system — needs compensating delete on failure)
for (id, text) in &entries_to_index {
    if let Err(e) = indexer.index_entry(id, text).await {
        // Compensating: transaction rollback handles SQLite cleanup
        txn.rollback()?;
        return Err(CompactionError::Index(e.to_string()));
    }
}

// Changelog
for (entry_id, conversation_id) in &changelog_entries {
    let cl_id = txn.append_changelog("compaction", &msg)?;
    txn.link_changelog_entry(cl_id, entry_id)?;
}

// Archive (filesystem)
let new_conversation_id = conversation_mgr.archive_and_retain(...)?;

// If we reach here, everything succeeded
txn.commit()?;
```

Note: If `MemoryDb` trait doesn't support transactions, the alternative is a two-phase approach: create entries as "pending", then mark as "committed" after all steps succeed. Check the trait before implementing.

- [ ] **Step 4: Run test to verify rollback works**

Run: `cargo test -p shore-daemon compaction_rolls_back_on_archive_failure`
Expected: PASS

- [ ] **Step 5: Run full compaction test suite**

Run: `cargo test -p shore-daemon compaction`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add shore-daemon/src/memory/compaction/mod.rs shore-daemon/src/memory/compaction/types.rs
git commit -m "fix(compaction): wrap multi-store writes in transaction with rollback on failure"
```

---

## Task 3: Atomic File Writes for active.jsonl (P1)

**Finding:** 3.5.2 — `messages.rs:227` and `compaction_impls.rs:429` use `std::fs::write()` which truncates then writes. A crash mid-write corrupts the file.

**Files:**
- Create: `shore-daemon/src/engine/atomic.rs`
- Modify: `shore-daemon/src/engine/messages.rs:227`
- Modify: `shore-daemon/src/engine/mod.rs` (add `mod atomic;`)
- Modify: `shore-daemon/src/memory/compaction_impls.rs:429`

**Strategy:** Write to a temp file in the same directory, then `rename()` atomically. On POSIX, `rename()` within the same filesystem is atomic.

- [ ] **Step 1: Write a test for atomic_write**

```rust
// In engine/atomic.rs
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn atomic_write_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");
        atomic_write(&path, b"hello\nworld\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\nworld\n");
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");
        std::fs::write(&path, "old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p shore-daemon atomic_write`
Expected: FAIL — function doesn't exist yet

- [ ] **Step 3: Implement atomic_write**

```rust
// shore-daemon/src/engine/atomic.rs

use std::io::Write;
use std::path::Path;

use crate::engine::EngineError;

/// Write `data` to `path` atomically via temp-file + rename.
///
/// The temp file is created in the same directory as `path` to guarantee
/// same-filesystem rename (atomic on POSIX).
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), EngineError> {
    let dir = path.parent().ok_or_else(|| EngineError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent directory"),
    })?;

    std::fs::create_dir_all(dir).map_err(|e| EngineError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| EngineError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    tmp.write_all(data).map_err(|e| EngineError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    tmp.persist(path).map_err(|e| EngineError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;

    Ok(())
}
```

- [ ] **Step 4: Add `mod atomic;` to engine/mod.rs**

Add `pub(crate) mod atomic;` to `shore-daemon/src/engine/mod.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p shore-daemon atomic_write`
Expected: PASS

- [ ] **Step 6: Replace std::fs::write in messages.rs:227**

Change `messages.rs:227`:

```rust
// BEFORE:
std::fs::write(&self.path, &buf).map_err(|e| EngineError::Io {
    path: self.path.clone(),
    source: e,
})?;

// AFTER:
super::atomic::atomic_write(&self.path, buf.as_bytes())?;
```

- [ ] **Step 7: Replace std::fs::write in compaction_impls.rs:429**

Change `compaction_impls.rs:429`:

```rust
// BEFORE:
std::fs::write(&active_path, &retained_content).map_err(|e| {
    CompactionError::ConversationManager(format!("failed to write retained messages: {e}"))
})?;

// AFTER:
crate::engine::atomic::atomic_write(&active_path, retained_content.as_bytes())
    .map_err(|e| {
        CompactionError::ConversationManager(format!("failed to write retained messages: {e}"))
    })?;
```

Note: `atomic_write` returns `EngineError`, but `compaction_impls` expects `CompactionError`. The `.map_err()` handles the conversion.

- [ ] **Step 8: Add tempfile as a dependency if not already present**

Check `shore-daemon/Cargo.toml` for `tempfile`. It's likely already a dev-dependency (used in tests). It needs to be a regular dependency now.

Run: `cargo add tempfile -p shore-daemon`

- [ ] **Step 9: Run existing persist tests**

Run: `cargo test -p shore-daemon persist`
Expected: All existing persist tests pass (behavior unchanged, just more durable)

- [ ] **Step 10: Run full crate tests**

Run: `cargo test -p shore-daemon`
Expected: All tests pass

- [ ] **Step 11: Commit**

```bash
git add shore-daemon/src/engine/atomic.rs shore-daemon/src/engine/mod.rs shore-daemon/src/engine/messages.rs shore-daemon/src/memory/compaction_impls.rs shore-daemon/Cargo.toml
git commit -m "fix(engine): use atomic temp+rename for active.jsonl writes"
```

---

## Task 4: Abort Previous Generation Handle (P1)

**Finding:** 3.2.1 — `handler.rs:314` replaces `generation_handle` without calling `.abort()`. Orphaned generation tasks continue running, wasting API credits.

**Files:**
- Modify: `shore-daemon/src/handler.rs:314`

**Strategy:** Before assigning a new `generation_handle`, abort any existing one.

- [ ] **Step 1: Write a test for generation handle abort**

In `handler.rs` test section, add a test that confirms a previous handle is aborted when a new generation starts. This can use a mock handle or a simple spawned task that sets a flag.

```rust
#[tokio::test]
async fn previous_generation_aborted_on_new_generation() {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = flag.clone();

    // Simulate a long-running generation
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        flag_clone.store(true, Ordering::SeqCst);
    });

    // Abort it (simulating what the fix should do)
    handle.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The flag should NOT have been set
    assert!(!flag.load(Ordering::SeqCst));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p shore-daemon previous_generation_aborted`
Expected: PASS (this tests the mechanism, not the integration)

- [ ] **Step 3: Add abort before handle replacement**

At `handler.rs:314`, change:

```rust
// BEFORE:
self.generation_handle = Some(tokio::spawn(async move {

// AFTER:
if let Some(prev) = self.generation_handle.take() {
    info!("Aborting previous generation (superseded by new request)");
    prev.abort();
}
self.generation_handle = Some(tokio::spawn(async move {
```

- [ ] **Step 4: Run full handler tests**

Run: `cargo test -p shore-daemon handler`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/handler.rs
git commit -m "fix(handler): abort previous generation handle before spawning new one"
```

---

## Task 5: Propagate Collation Indexing Errors (P1)

**Finding:** 3.1.2 — `collation/mod.rs:531, 634, 712` silently discard `index_entry()` errors with `let _ =`.

**Files:**
- Modify: `shore-daemon/src/memory/collation/mod.rs` (lines 531, 634, 712)

**Strategy:** Log the error and continue. Collation should not fail hard on indexing (entries are still in SQLite and can be re-indexed), but errors must be visible. Use `warn!()` to surface failures without breaking the collation pipeline.

The reviewer noted that compaction *propagates* indexing errors (line 254 uses `?`). We match that behavior but use `warn!` instead of `?` because collation operates on many entries per run and a single index failure shouldn't abort the entire batch.

- [ ] **Step 1: Write a test for indexing error visibility**

In `collation/mod.rs` tests, add a test with a mock indexer that returns `Err`. Verify collation completes (doesn't panic or abort) but the entry is still created in the DB.

```rust
#[tokio::test]
async fn collation_continues_on_indexing_error() {
    // Set up with a FailingIndexer that always returns Err
    // Run apply_merge()
    // Assert: entry exists in db (collation completed)
    // Assert: no panic
}
```

- [ ] **Step 2: Run test to confirm behavior**

Run: `cargo test -p shore-daemon collation_continues_on_indexing`
Expected: PASS (current behavior already continues — but silently)

- [ ] **Step 3: Replace `let _ =` with `warn!` at all three sites**

At line 531:
```rust
// BEFORE:
let _ = idx.index_entry(&new_id, &result.summary_text).await;

// AFTER:
if let Err(e) = idx.index_entry(&new_id, &result.summary_text).await {
    warn!(entry_id = %new_id, error = %e, "Failed to index merged entry");
}
```

At line 634:
```rust
// BEFORE:
let _ = idx.index_entry(&new_id, &replacement.summary_text).await;

// AFTER:
if let Err(e) = idx.index_entry(&new_id, &replacement.summary_text).await {
    warn!(entry_id = %new_id, error = %e, "Failed to index split entry");
}
```

At line 712:
```rust
// BEFORE:
let _ = idx.index_entry(entry_id, &result.summary_text).await;

// AFTER:
if let Err(e) = idx.index_entry(entry_id, &result.summary_text).await {
    warn!(entry_id = %entry_id, error = %e, "Failed to index updated entry");
}
```

- [ ] **Step 4: Run collation tests**

Run: `cargo test -p shore-daemon collation`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add shore-daemon/src/memory/collation/mod.rs
git commit -m "fix(collation): log vector indexing errors instead of silently discarding"
```

---

## Task 6: Finalize LedgerStream on Error Paths (P2)

**Finding:** 1.3 — `tools.rs:237-243` and `handler.rs:855-878` drop `LedgerStream` without calling `finalize()` when `consume()` returns `Err`.

**Files:**
- Modify: `shore-daemon/src/engine/tools.rs:237-243`
- Modify: `shore-daemon/src/handler.rs:855-878`

**Strategy:** Use a scope guard pattern or restructure error handling to ensure `finalize()` is always called. Since `finalize()` takes the result, on error paths we finalize with a zero-token error result.

- [ ] **Step 1: Check LedgerStream::finalize signature and Drop impl**

Read `shore-llm-client` or `shore-ledger` to understand:
- What `finalize()` actually does (records the call to the ledger)
- What `Drop` does (the defense.md says it only logs)
- Whether `finalize()` can be called with partial/error data

- [ ] **Step 2: Restructure tools.rs error handling**

At `tools.rs:237-243`, change:

```rust
// BEFORE:
let mut ledger_stream = client
    .stream_raw(request, CallType::ToolLoop, character, thinking_enabled)
    .await?;
result = consumer
    .consume(ledger_stream.reader_mut(), false, cache_ctx)
    .await?;
ledger_stream.finalize(&result);

// AFTER:
let mut ledger_stream = client
    .stream_raw(request, CallType::ToolLoop, character, thinking_enabled)
    .await?;
match consumer
    .consume(ledger_stream.reader_mut(), false, cache_ctx)
    .await
{
    Ok(r) => {
        ledger_stream.finalize(&r);
        result = r;
    }
    Err(e) => {
        ledger_stream.finalize_error();
        return Err(e.into());
    }
};
```

Note: `finalize_error()` may not exist yet. Check the `LedgerStream` API. If not, we may need to add it or call `finalize()` with a default/empty result. Alternatively, if `Drop` can be enhanced to call `finalize_error()`, that's cleaner — but that's a change to `shore-ledger` or `shore-llm-client`.

- [ ] **Step 3: Restructure handler.rs error handling**

At `handler.rs:873-878`, apply the same pattern:

```rust
// BEFORE:
let result = consumer
    .consume(ledger_stream.reader_mut(), regen, &cache_ctx)
    .await?;
ledger_stream.finalize(&result);
Ok(result)

// AFTER:
match consumer
    .consume(ledger_stream.reader_mut(), regen, &cache_ctx)
    .await
{
    Ok(result) => {
        ledger_stream.finalize(&result);
        Ok(result)
    }
    Err(e) => {
        ledger_stream.finalize_error();
        Err(e)
    }
}
```

- [ ] **Step 4: If finalize_error() doesn't exist, add it to LedgerStream**

Check the `LedgerStream` implementation. If no error finalization method exists, add one that records a zero-token failed call entry.

- [ ] **Step 5: Run tests**

Run: `cargo test -p shore-daemon`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add shore-daemon/src/engine/tools.rs shore-daemon/src/handler.rs
# If LedgerStream was modified:
# git add shore-llm-client/src/stream.rs (or wherever LedgerStream lives)
git commit -m "fix(ledger): finalize LedgerStream on error paths to prevent unrecorded calls"
```

---

## Task 7: Clean Up embed_text() (P2)

**Finding:** 3.6.1 — `vectorstore.rs:267-285` creates a new `reqwest::Client` per call and sends `api_key` in the JSON body rather than as an auth header.

**Files:**
- Modify: `shore-daemon/src/memory/vectorstore.rs:267-300`

**Strategy:** Accept a shared `&reqwest::Client` parameter instead of constructing one internally. Move `api_key` from JSON body to `Authorization: Bearer` header. The caller already has access to a client (or can create a shared one at `VectorStore` construction).

- [ ] **Step 1: Write a test for embed_text with shared client**

```rust
#[tokio::test]
async fn embed_text_uses_auth_header() {
    // This is a structural test — verify the function signature accepts &reqwest::Client
    // and that the body does not contain api_key
    // (Full integration test requires a running shore-llm instance)
}
```

- [ ] **Step 2: Change embed_text signature**

```rust
// BEFORE:
pub async fn embed_text(
    base_url: &str,
    provider: &str,
    model: &str,
    api_key: &str,
    input: &[&str],
) -> Result<Vec<Vec<f32>>, VectorStoreError> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "provider": provider,
        "model": model,
        "api_key": api_key,
        "input": input,
    });
    let resp = client
        .post(format!("{base_url}/v1/embed"))
        .json(&body)
        .send()
        .await

// AFTER:
pub async fn embed_text(
    client: &reqwest::Client,
    base_url: &str,
    provider: &str,
    model: &str,
    api_key: &str,
    input: &[&str],
) -> Result<Vec<Vec<f32>>, VectorStoreError> {
    let body = serde_json::json!({
        "provider": provider,
        "model": model,
        "input": input,
    });
    let resp = client
        .post(format!("{base_url}/v1/embed"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
```

- [ ] **Step 3: Update all callers**

Search for all call sites of `embed_text(` and update them to pass a shared `&reqwest::Client`. Callers will need access to a client — check if `VectorStore` already holds one, or add one to its constructor.

Run: `grep -rn "embed_text(" shore-daemon/src/`

Update each caller to pass the shared client reference.

- [ ] **Step 4: Check shore-llm /v1/embed endpoint**

Verify that the `/v1/embed` endpoint on the shore-llm side reads `api_key` from the `Authorization` header OR the JSON body. If it only reads from the body, the shore-llm endpoint needs updating too. Check before assuming the header approach works.

- [ ] **Step 5: Run tests**

Run: `cargo test -p shore-daemon vectorstore`
Expected: All tests pass

- [ ] **Step 6: Run full build**

Run: `cargo build --workspace`
Expected: Clean build (no caller missed)

- [ ] **Step 7: Commit**

```bash
git add shore-daemon/src/memory/vectorstore.rs
# Add any other files modified (callers, shore-llm if endpoint changed)
git commit -m "fix(vectorstore): share reqwest client and use auth header for embed_text"
```

---

## Execution Notes

**Task dependencies:**
- Tasks 1-7 are largely independent and can be parallelized across subagents.
- Exception: Task 1 (autonomous race) and Task 3 (atomic writes) both modify `active.jsonl` write behavior. If run in parallel, merge conflicts are possible on `messages.rs`. Recommend running Task 1 first, then Task 3.

**Verification after all tasks:**

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

**Out of scope (deferred):**
- Blocking I/O on async runtime (Finding 2.2 / 3.7.1) — pervasive; requires spawn_blocking audit across 20+ sites
- handler.rs god file split (Finding 3.1) — structural refactor, separate initiative
- AutonomyManager size (Finding 3.3) — same
- Per-request DB opens (Finding 2.3 / 3.7.2) — performance optimization, not correctness
- Mutex poisoning patterns (Finding 6) — low practical risk, audit-level change
