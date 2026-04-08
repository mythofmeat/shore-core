# Review Remediation Phase 2 — Remaining Findings

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address all remaining actionable findings from the adversarial code review (defense.md) after the P0/P1 data-integrity fixes were completed in phase 1.

**Architecture:** Each fix is isolated to 1–2 files. No cross-cutting changes. Fixes are ordered by priority, with independent tasks that can be parallelized.

**Tech Stack:** Rust stable, tokio async runtime, rusqlite, shore workspace crates.

---

## Task 1: Graceful MemoryDB::open() Degradation (P1 — 3.5.1)

**Files:**
- Modify: `shore-daemon/src/handler.rs:988-989`

The VectorStore::open() failure at handler.rs:1007 gracefully degrades (logs warning, sets to None). MemoryDB::open() at handler.rs:989 propagates with `?`, killing the entire generation. Apply the same graceful-degradation pattern.

- [ ] **Step 1: Modify run_tool_phase to gracefully degrade MemoryDB::open failures**

Replace the `?`-propagating open with a match that logs and returns an empty tool-loop result (no memory tools available):

```rust
// handler.rs — inside run_tool_phase(), around line 988
let memory_db = match MemoryDB::open(&db_path) {
    Ok(db) => Some(db),
    Err(e) => {
        warn!(character = char_name, error = %e, "Failed to open memory DB — memory tools disabled for this turn");
        None
    }
};
```

Then guard the tool-loop call on `memory_db` being `Some`. If `None`, skip the tool phase and return `ToolLoopResult::default()` (or equivalent "no tools ran" value).

- [ ] **Step 2: Build and verify**

Run: `cargo build --workspace`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/src/handler.rs
git commit -m "fix(handler): gracefully degrade MemoryDB::open failure instead of killing generation"
```

---

## Task 2: SSE Parser O(n²) Fix (P2 — S1.1)

**Files:**
- Modify: `shore-llm-client/src/providers/sse.rs:37-38`

The parser does `self.buf = self.buf[newline_pos + 1..].to_string()` inside a while loop, creating a new String each iteration. Use `drain(..=newline_pos)` instead, which shifts bytes in-place without allocating a new String.

- [ ] **Step 1: Fix the quadratic allocation**

Replace lines 37-38 in `SseParser::feed()`:

```rust
// Before:
let line = self.buf[..newline_pos].trim_end_matches('\r').to_string();
self.buf = self.buf[newline_pos + 1..].to_string();

// After:
let raw: String = self.buf.drain(..=newline_pos).collect();
let line = raw.trim_end_matches('\n').trim_end_matches('\r').to_string();
```

Note: `drain(..=newline_pos)` includes the newline itself in the drained range, leaving the remainder in `self.buf` without reallocation.

- [ ] **Step 2: Build and run tests**

Run: `cargo test -p shore-llm-client`
Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add shore-llm-client/src/providers/sse.rs
git commit -m "fix(sse): replace O(n²) string slice with drain to avoid per-line reallocation"
```

---

## Task 3: Remove Dead Double-Reload Check (P2 — S2.1.2)

**Files:**
- Modify: `shore-daemon/src/handler.rs:606-616`

The second `take_needs_reload` call at line 609 is dead code — the first call at line 473 already consumed (cleared) the flag. The second check always returns false.

- [ ] **Step 1: Remove the dead reload check**

Delete the block at lines 606-616 that contains the second `take_needs_reload` call. Keep the user-message notification and turn-count logic that follows if it's still needed outside the `if` block.

- [ ] **Step 2: Build and run tests**

Run: `cargo build -p shore-daemon && cargo test -p shore-daemon`
Expected: compiles and tests pass.

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/src/handler.rs
git commit -m "fix(handler): remove dead second take_needs_reload check (always returns false)"
```

---

## Task 4: Replace .lock().unwrap() in Handler Hot Path (P2 — S1.6 / 3.5.3)

**Files:**
- Modify: `shore-daemon/src/handler.rs` — lines 1110, 1132 (session_tokens and diagnostics Mutex)
- Modify: `shore-daemon/src/engine/tools.rs` — line 180 (diagnostics Mutex)

The autonomy manager at manager.rs:526-530 already uses the correct pattern: `.lock().unwrap_or_else(|e| e.into_inner())` which recovers from mutex poisoning. Apply the same pattern to the handler and tools hot paths.

- [ ] **Step 1: Fix handler.rs mutex sites**

At line 1110:
```rust
// Before:
let mut tokens = ctx.session_tokens.lock().unwrap();
// After:
let mut tokens = ctx.session_tokens.lock().unwrap_or_else(|e| e.into_inner());
```

At line 1132:
```rust
// Before:
ctx.diagnostics.lock().unwrap().api_calls.push(entry);
// After:
ctx.diagnostics.lock().unwrap_or_else(|e| e.into_inner()).api_calls.push(entry);
```

- [ ] **Step 2: Fix tools.rs mutex site**

At line 180:
```rust
// Before:
diag.lock().unwrap().tool_calls.push(entry);
// After:
diag.lock().unwrap_or_else(|e| e.into_inner()).tool_calls.push(entry);
```

- [ ] **Step 3: Build and run tests**

Run: `cargo build -p shore-daemon && cargo test -p shore-daemon`
Expected: compiles and tests pass.

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/src/handler.rs shore-daemon/src/engine/tools.rs
git commit -m "fix(handler): recover from mutex poisoning instead of panicking in hot path"
```

---

## Task 5: Add LLM Call Amplification Guard (P3 — 3.1.3)

**Files:**
- Modify: `shore-daemon/src/memory/researcher.rs:16`
- Modify: `shore-daemon/src/memory/agent/tool_loop.rs:18`

A single user message can trigger up to 15 × 40 = 600 LLM calls through researcher+agent nesting. Reduce the agent tool loop max from 40 to 20. The researcher loop at 15 is reasonable (it's the outer search loop). The inner agent loop at 40 is excessive — 20 iterations is more than enough for any single tool task.

- [ ] **Step 1: Reduce MAX_ITERATIONS in tool_loop.rs**

```rust
// Before:
const MAX_ITERATIONS: usize = 40;
// After:
const MAX_ITERATIONS: usize = 20;
```

This halves the worst-case amplification from 615 to 315.

- [ ] **Step 2: Build and run tests**

Run: `cargo build -p shore-daemon && cargo test -p shore-daemon`
Expected: compiles and tests pass.

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/src/memory/agent/tool_loop.rs
git commit -m "fix(agent): halve tool loop MAX_ITERATIONS from 40 to 20 to bound LLM call amplification"
```

---

## Task 6: Batch N+1 get_entry() Calls in Semantic Search (P3 — 3.1.5)

**Files:**
- Modify: `shore-daemon/src/memory/db.rs` (add `get_entries_by_ids` method)
- Modify: `shore-daemon/src/memory/agent/tool_handlers.rs:205-253`

The semantic search handler calls `db.get_entry(id)` in a loop for each result. Add a batch query method and use it.

- [ ] **Step 1: Add get_entries_by_ids to MemoryDB**

Add after the existing `get_entry` method (db.rs ~line 399):

```rust
/// Fetch multiple entries by ID in a single query.
pub fn get_entries_by_ids(&self, ids: &[&str]) -> SqlResult<Vec<Entry>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
    let sql = format!(
        "SELECT id, memory_type, source, reason, status, confidence,
                summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                message_count, source_entry_ids, related_entry_ids, superseded_by,
                created_at, updated_at, entry_type, image_path, collated_at
         FROM entries WHERE id IN ({})",
        placeholders.join(", ")
    );
    let mut stmt = self.conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> =
        ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_entry)?;
    rows.collect()
}
```

- [ ] **Step 2: Use batch query in tool_handlers.rs**

Replace the two per-ID loops (lines 205-215 and 226-253) with calls to `get_entries_by_ids`. Build a HashMap<&str, Entry> from the batch result for O(1) lookups during fusion and formatting.

- [ ] **Step 3: Build and run tests**

Run: `cargo build -p shore-daemon && cargo test -p shore-daemon`
Expected: compiles and tests pass.

- [ ] **Step 4: Commit**

```bash
git add shore-daemon/src/memory/db.rs shore-daemon/src/memory/agent/tool_handlers.rs
git commit -m "fix(memory): batch N+1 get_entry calls in semantic search with get_entries_by_ids"
```

---

## Task 7: BM25 Inverted Index for O(1) Term Lookup (P3 — 3.1.4)

**Files:**
- Modify: `shore-daemon/src/memory/search.rs:89-114`

The BM25 search scans all documents for each query term. The `doc_freq` HashMap already has a per-term set of entry IDs (line 90-93). Use it to iterate only matching documents instead of all documents.

- [ ] **Step 1: Use doc_freq for targeted iteration**

Replace the inner loop at lines 103-113:

```rust
// Before: iterate ALL documents per term
for (entry_id, doc_tokens) in &self.documents {
    let tf = doc_tokens.iter().filter(|t| *t == term).count() as f64;
    ...
}

// After: iterate only documents that contain the term
if let Some(matching_ids) = self.doc_freq.get(term.as_str()) {
    for entry_id in matching_ids {
        if let Some(doc_tokens) = self.documents.get(entry_id.as_str()) {
            let tf = doc_tokens.iter().filter(|t| *t == term).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let dl = doc_tokens.len() as f64;
            let tf_norm = (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));
            *scores.entry(entry_id.as_str()).or_insert(0.0) += idf * tf_norm;
        }
    }
}
```

This changes the inner loop from O(D) to O(df) per term — only documents containing the term are visited.

- [ ] **Step 2: Build and run tests**

Run: `cargo test -p shore-daemon`
Expected: all tests pass (BM25 tests should produce identical results).

- [ ] **Step 3: Commit**

```bash
git add shore-daemon/src/memory/search.rs
git commit -m "fix(search): use doc_freq index for O(df) BM25 lookup instead of O(D) full scan"
```

---

## Not Addressed (deferred)

These findings are confirmed but deferred as structural/design work beyond the scope of this remediation:

- **MessageStore O(n) persist (2.1)** — already uses atomic writes (phase 1 fix). Append-only optimization requires MessageStore redesign.
- **Blocking FS I/O on async (2.2 / 3.4.1 / 3.7.1)** — pervasive (20+ sites). Requires systematic spawn_blocking wrapping or migration to tokio::fs. Separate initiative.
- **Per-request MemoryDB/VectorStore (2.3 / 3.7.2)** — connection pooling requires lifetime/ownership redesign.
- **rid parameter propagation (S1.5)** — requires X-Request-ID plumbing through LlmClient. Separate tracing initiative.
- **handler.rs god file / AutonomyManager size / dual tool loops (3.1–3.3)** — structural refactoring.
- **No abort on client disconnect (3.4)** — requires connection health monitoring.
- **Broadcast channel capacity (4.4)** — unlikely to cause issues at current scale.
