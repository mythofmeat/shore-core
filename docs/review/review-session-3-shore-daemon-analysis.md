# Session 3: shore-daemon Semi-Formal Analysis

**Scope**: `shore-daemon` crate — the central service binary.
**Date**: 2026-04-07
**Prior sessions**: `docs/review-session-1-foundation-crates.md` (foundation crates), `docs/review-session-2-shore-daemon.md` (daemon mapping)

---

## Phase 3: Semi-Formal Analysis

### 3.1 Memory System Architecture

---

#### Finding 3.1.1 — SQLite-LanceDB Atomicity Gap in Compaction

```
FINDING: Compaction writes memory entries to SQLite, indexes them to LanceDB,
and then rewrites active.jsonl as three separate non-transactional steps,
meaning a failure at any point leaves the stores in an inconsistent state
with no recovery mechanism.

PREMISES:
- P1: compaction/mod.rs:250 calls db.create_entry() (SQLite INSERT), then
  line 254 calls indexer.index_entry() (LanceDB upsert), then line 275
  calls archive_and_retain() (filesystem rewrite of active.jsonl)
- P2: There is no BEGIN TRANSACTION / COMMIT wrapping the entry loop
  (compaction/mod.rs:224-271)
- P3: There is no rollback logic on any error path in the compact() function
- P4: archive_and_retain() uses bare std::fs::write (not temp+rename)
  for active.jsonl (compaction_impls.rs:429)
- P5: Each entry write is committed immediately to SQLite (no batching)

TRACED PATH:
- Step 1: compact() iterates parsed entries (compaction/mod.rs:224)
- Step 2: For entry[0]: SQLite INSERT succeeds (line 250)
- Step 3: LanceDB index_entry succeeds (line 254)
- Step 4: For entry[1]: SQLite INSERT succeeds (line 250)
- Step 5: LanceDB index_entry FAILS (line 254) — error propagates
- Step 6: compact() returns Err — entry[0] is in SQLite AND LanceDB,
  entry[1] is in SQLite ONLY, active.jsonl is UNCHANGED
- Step 7: Next compaction attempt re-processes the same messages,
  creating DUPLICATE entries for those messages

CONCLUSION: correctness + data durability. Partial compaction failure
leaves orphan SQLite entries (invisible to vector search), and
re-compaction creates duplicate entries. The non-atomic active.jsonl
rewrite (bare std::fs::write, not temp+rename) risks data corruption
on process crash mid-write.

CONFIDENCE: HIGH
SEVERITY: CRITICAL

WHAT I COULD NOT VERIFY:
Whether the LLM summarization step (step 4 in compact()) is deterministic
enough that duplicate entries from re-compaction would produce identical
summary_text, enabling deduplication. There is no explicit dedup logic.
```

---

#### Finding 3.1.2 — Silent Vector Indexing Failures in Collation

```
FINDING: Collation's merge, split, and update operations silently discard
vector indexing errors via `let _ = idx.index_entry(...)`, creating
entries that exist in SQLite but are permanently invisible to semantic
search.

PREMISES:
- P1: collation/mod.rs:531: `let _ = idx.index_entry(&new_id, &result.summary_text).await;`
  in apply_merge
- P2: collation/mod.rs:634: same pattern in apply_split
- P3: collation/mod.rs:712: same pattern in apply_update
- P4: After indexing, source entries are superseded (collation/mod.rs:536-539),
  meaning the merged entry is the ONLY active version
- P5: Superseded entries are excluded from search by lifecycle scoring
  (rag.rs uses status weight 0.3x for superseded)

TRACED PATH:
- Step 1: Collation clusters entries, LLM suggests a merge action
- Step 2: apply_merge creates new entry in SQLite (line 527)
- Step 3: Vector indexing silently fails (line 531, error discarded)
- Step 4: Source entries are superseded (line 537)
- Step 5: The merged entry has NO vector embedding — invisible to
  semantic_search tool
- Step 6: FTS5 text search still works (SQLite trigger-based), but
  semantic retrieval is permanently degraded for these entries

CONCLUSION: correctness. Silent data loss in the semantic search path.
The entries exist but cannot be found by the primary retrieval mechanism
used during conversation. Over time, repeated collation rounds could
systematically degrade semantic recall quality.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
How frequently indexing failures occur in practice. If LanceDB is stable,
this may never trigger. But the pattern is architecturally wrong — a
failure in a secondary store should not be silently swallowed when it
affects the primary retrieval path.
```

---

#### Finding 3.1.3 — Unbounded LLM Call Amplification via Researcher + Agent Nesting

```
FINDING: A single user message can trigger up to ~615 LLM API calls
through the nested researcher→agent→tool loop chain, with no per-query
cost cap or token budget.

PREMISES:
- P1: engine/tools.rs:62 — tool loop has max_iterations (default 10),
  each iteration can call the `memory` tool
- P2: tools/memory_tools.rs routes `memory` tool to researcher when configured
- P3: researcher.rs:16 — MAX_RESEARCHER_ITERATIONS = 15
- P4: memory/agent/tool_loop.rs:17 — MAX_ITERATIONS = 40
- P5: Each researcher iteration can call ask_memory_agent, which invokes
  the full agent tool loop (up to 40 LLM calls each)
- P6: There is no total budget across the nested call tree
- P7: embed_text() creates a new reqwest::Client per call
  (vectorstore.rs:274), preventing connection reuse

TRACED PATH:
- Step 1: User sends message → handler spawns generation task
- Step 2: LLM responds with tool_use for `memory` tool
- Step 3: Engine tool loop dispatches to memory_tools handler
- Step 4: Researcher begins: up to 15 iterations
- Step 5: Each iteration: 1 researcher LLM call + possible ask_memory_agent
- Step 6: ask_memory_agent triggers agent loop: up to 40 iterations
- Step 7: Each agent iteration: 1 LLM call + possible embedding call
- Step 8: Worst case: 10 (engine) * [15 * (1 + 40)] = 6,150 LLM calls
  if every engine iteration triggers a full researcher+agent cycle
- Step 9: More realistic worst case (1 memory invocation): 15 + 15*40 = 615

CONCLUSION: cost risk. A single user message can trigger hundreds of
API calls. The ledger tracks costs accurately but does not enforce any
spending cap. With expensive models (Claude Opus, GPT-4), this could
cost $5-20+ per message. The per-call reqwest::Client in embed_text
adds connection overhead.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether in practice the researcher typically calls ask_memory_agent on
every iteration, or whether the agent loop typically exhausts its 40
iterations. The worst case may be theoretical but the architecture
allows it.
```

---

#### Finding 3.1.4 — BM25 O(n) Scan Per Search Query

```
FINDING: The in-memory BM25 index performs an O(T * N * L) scan on every
search query, where T = query terms, N = documents, L = avg document
length, degrading linearly as stored memories grow.

PREMISES:
- P1: search.rs:103-113 iterates all documents for each query term:
  `for (entry_id, doc_tokens) in &self.documents`
- P2: For each document, it counts term frequency with linear scan:
  `doc_tokens.iter().filter(|t| *t == term).count()`
- P3: The Bm25Index is built in-memory and populated per-session
  (agent/types.rs:154)
- P4: No inverted index or posting list structure is used

TRACED PATH:
- Step 1: Agent calls semantic_search tool
- Step 2: search_ctx.search() calls bm25_search (tool_handlers.rs:159)
- Step 3: bm25.search() scans ALL documents × ALL query terms (search.rs:103)
- Step 4: For 10K entries with avg 50 tokens each, 5 query terms:
  5 * 10K * 50 = 2.5M comparisons per search
- Step 5: This runs on the async runtime, blocking the tokio thread
  during the entire scan

CONCLUSION: scalability. BM25 search latency grows linearly with entry
count. At 10K+ entries, search becomes measurably slow. At 100K entries,
it becomes a significant bottleneck.

CONFIDENCE: HIGH
SEVERITY: MINOR (in-memory scan is fast enough for current scale; becomes
a concern at 10K+ entries)

WHAT I COULD NOT VERIFY:
Current typical entry counts per character. If characters typically have
<1K entries, this is not yet a problem.
```

---

#### Finding 3.1.5 — N+1 Query Pattern in Semantic Search

```
FINDING: Semantic search fetches entry metadata with individual SQL queries
per result ID instead of a batch WHERE IN clause, creating an N+1 query
pattern that fires up to 60 queries per search.

PREMISES:
- P1: tool_handlers.rs:196-206 iterates all_ids and calls
  db.get_entry(id) individually for each
- P2: tool_handlers.rs:217-244 repeats this pattern for final output
- P3: Over-retrieval fetches top_k * 3 = ~60 results per source
  (tool_handlers.rs:159)

TRACED PATH:
- Step 1: semantic_search called with top_k=20
- Step 2: Over-retrieve 60 from vector search + 60 from BM25
- Step 3: RRF fusion merges results (rag.rs:126-157)
- Step 4: Fetch metadata: 60 individual db.get_entry() calls
  (tool_handlers.rs:196)
- Step 5: After RRF ranking, fetch full entries: another N calls
  (tool_handlers.rs:217)
- Step 6: Total: up to 120 individual SQL queries per semantic search

CONCLUSION: scalability. The N+1 pattern is correct but wasteful. Each
db.get_entry() is a prepared statement execution, but the overhead of
120 individual queries vs 2 batch queries is measurable.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY: None.
```

---

### 3.2 Agentic Behavior

---

#### Finding 3.2.1 — Generation Task Replacement Without Abort

```
FINDING: When a new Message or Regen arrives while a generation task is
in-flight, the previous task's JoinHandle is silently dropped without
aborting the task, allowing the orphaned task to continue consuming LLM
API credits.

PREMISES:
- P1: handler.rs:314: `self.generation_handle = Some(tokio::spawn(...))`
  replaces the previous handle
- P2: In Rust, dropping a JoinHandle does NOT abort the task — the task
  continues running in the background
- P3: Cancel handling (handler.rs:236-261) only aborts the current
  generation_handle, not any previously orphaned tasks
- P4: Each generation task holds a clone of engine_arc and llm_client,
  so the orphaned task has full access to make LLM API calls

TRACED PATH:
- Step 1: User sends Message A → handle_generation spawned (handle A)
- Step 2: User sends Message B before A completes
- Step 3: handler.rs:314 replaces generation_handle with new spawn
- Step 4: Handle A is dropped — but task A continues running
- Step 5: Task A is making LLM API calls, consuming credits
- Step 6: Task A eventually tries to persist_and_notify, acquiring the
  engine lock, potentially interfering with task B's state
- Step 7: Cancel only aborts handle B — task A is unrecoverable

CONCLUSION: cost risk + correctness. Orphaned tasks waste API credits and
can cause state corruption when they eventually acquire the engine lock.
Under rapid message sending, multiple orphaned tasks could accumulate.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether the UI or client protocol prevents rapid message sending. If the
TUI disables the send button during generation, this may not occur in
practice. But the protocol allows it, and a misbehaving client could
trigger it.
```

---

#### Finding 3.2.2 — Autonomous Message Persistence Race Condition

```
FINDING: The interiority tick loop writes autonomous messages directly to
active.jsonl via file append WITHOUT holding the engine lock, while the
handler's persist_and_notify rewrites the entire file from in-memory state,
creating a data loss race condition.

PREMISES:
- P1: autonomy/manager.rs:1034-1042 uses std::fs::OpenOptions::append()
  to write directly to active.jsonl without acquiring the engine lock
- P2: handler.rs:1037-1041 calls engine.append_message() which holds the
  engine lock and calls MessageStore::persist() — a full file rewrite
  (messages.rs:227: std::fs::write(&self.path, &buf))
- P3: The engine's in-memory MessageStore is not updated when autonomy
  appends directly to the file
- P4: The handler cloned messages from the engine at handler.rs:599
  BEFORE the autonomous message was appended

TRACED PATH:
- Step 1: Interiority tick fires → autonomous message generated
- Step 2: Autonomy appends message to active.jsonl (manager.rs:1041)
- Step 3: Broadcasts NewMessage to clients (manager.rs:1046)
- Step 4: Meanwhile, user message arrives → handle_generation starts
- Step 5: Handler clones messages from engine (handler.rs:599) — does NOT
  include the autonomous message (engine's in-memory state is stale)
- Step 6: Handler calls persist_and_notify after generation
- Step 7: engine.append_message() → MessageStore::persist() rewrites
  active.jsonl from in-memory state (messages.rs:227)
- Step 8: The autonomous message from Step 2 is OVERWRITTEN — data loss

CONCLUSION: correctness. Data loss race condition between autonomous
message persistence and handler's full-file rewrite. The autonomous
message is also present in the broadcast channel but lost from durable
storage.

CONFIDENCE: HIGH
SEVERITY: CRITICAL

WHAT I COULD NOT VERIFY:
Whether the engine's reload logic (triggered by take_needs_reload after
compaction) could accidentally recover the message by re-reading from
disk. But this only happens on compaction, not on every generation.
```

---

#### Finding 3.2.3 — Interiority Prompt Reuses Full Conversation History

```
FINDING: The interiority tick reuses the cached last_request which contains
the full conversation context, meaning autonomous actions consume the same
token budget as a user-facing request with no separate cost control.

PREMISES:
- P1: autonomy/manager.rs:842-861 clones the last_request for interiority
  ticks
- P2: last_request contains the full assembled prompt including conversation
  history, system prompt, and character/user definitions
- P3: There is no separate token budget or trimmed context for autonomous
  actions
- P4: Interiority fires on a configurable interval (default 3600s)

CONCLUSION: cost risk. Each interiority tick sends the full conversation
context to the LLM. For conversations with 100K+ tokens of history, each
hourly tick costs the same as a user message. With prompt caching, the
marginal cost is lower, but the absolute cost scales with conversation
length.

CONFIDENCE: MEDIUM
SEVERITY: OBSERVATION

WHAT I COULD NOT VERIFY:
Whether prompt caching is effective for interiority ticks. If the same
last_request is sent with cache control headers, the LLM provider may
return mostly cache-read tokens, reducing cost significantly.
```

---

### 3.3 Conversation Context Assembly

---

#### Finding 3.3.1 — Token Estimation Uses Chars/4 Heuristic

```
FINDING: Token counting uses a characters-divided-by-4 heuristic that
systematically under-counts short English text and over-counts CJK/emoji,
leading to potential context window overflow or over-truncation.

PREMISES:
- P1: engine/prompt.rs:426-429: `estimate_tokens(text: &str) -> usize {
  text.len().div_ceil(CHARS_PER_TOKEN) }` where CHARS_PER_TOKEN = 4
- P2: This is a byte-length heuristic, not an actual tokenizer
- P3: Short English text like "I am a" (6 bytes) estimates 2 tokens but
  real tokenizers produce ~3
- P4: CJK text like "日本語の文" (15 bytes) estimates 4 tokens but
  real tokenizers may produce more
- P5: RedactedThinking blocks are counted as 0 tokens (prompt.rs:445)

TRACED PATH:
- Step 1: assemble_prompt() estimates system tokens (prompt.rs:324)
- Step 2: trim_messages() estimates per-message tokens (prompt.rs:494)
- Step 3: Budget allocation uses these estimates
- Step 4: If estimates are too low, actual tokens sent to LLM may exceed
  the model's context window → API error (429 or truncation by provider)
- Step 5: If estimates are too high, conversation history is over-truncated,
  losing relevant context

CONCLUSION: architectural. The heuristic is a pragmatic trade-off
(avoids adding a tokenizer dependency), but creates a systematic mismatch
between estimated and actual token usage. For CJK-heavy conversations,
this could cause API errors. For English, it over-truncates slightly.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY:
Whether the LLM providers (OpenRouter, Anthropic) enforce strict context
limits or silently truncate. If they enforce strictly, the heuristic's
under-counting for CJK could cause user-facing errors.
```

---

#### Finding 3.3.2 — Truncation Drops Oldest Messages Without Priority

```
FINDING: Context window trimming uses a pure recency-based strategy that
always drops the oldest messages, with no priority for system-critical
content embedded in early messages.

PREMISES:
- P1: engine/prompt.rs:488-551: trim_messages iterates from newest to
  oldest, accumulating until budget exhausted
- P2: No message is given special priority — the first user message,
  persona-establishing exchanges, and critical instructions are treated
  identically to casual messages
- P3: Leading tool-loop intermediates are stripped (prompt.rs:514-517),
  which is correct
- P4: Time-gap markers are injected after trimming (prompt.rs:519-548)

CONCLUSION: architectural. Pure recency-based truncation is standard for
chat applications but may lose important early context (establishing
shot, key facts, character definitions). This is a design choice, not
a bug — but worth noting for recall-critical use cases.

CONFIDENCE: HIGH
SEVERITY: OBSERVATION

WHAT I COULD NOT VERIFY:
Whether character and user definitions are part of the system prompt
(never truncated) or embedded in messages. If they're in the system
prompt block (prompt.rs:237-331), they're protected. If they're in
early messages, they're vulnerable to truncation.
```

---

### 3.4 Concurrency and State Safety

---

#### Finding 3.4.1 — Blocking Filesystem I/O While Holding Engine Lock

```
FINDING: MessageStore::persist() performs synchronous full-file rewrite
(std::fs::write) while the engine's tokio Mutex is held, blocking any
other task needing the engine for the duration of the I/O operation.

PREMISES:
- P1: handler.rs:1038: `let mut engine = engine_arc.lock().await;`
- P2: handler.rs:1041: `engine.append_message(msg)?` → persist()
- P3: engine/messages.rs:227: persist() calls `std::fs::write(&self.path, &buf)`
  which rewrites the ENTIRE active.jsonl synchronously
- P4: The engine lock is held across persist() (it's called inside the
  lock guard's scope)
- P5: For conversations with 500+ messages, active.jsonl could be
  multiple MB, making this a multi-millisecond blocking operation

TRACED PATH:
- Step 1: Generation completes → persist_and_notify called
- Step 2: Engine lock acquired (handler.rs:1038)
- Step 3: append_message() calls persist() → serializes ALL messages to
  JSON → writes entire file (messages.rs:227)
- Step 4: During this time, any other task needing the engine (autonomy
  state creation, another generation) is blocked
- Step 5: If another client sends a message, it waits for the lock

CONCLUSION: scalability + architectural. The engine lock scope includes
blocking I/O, creating a contention bottleneck that worsens with
conversation length. This is the P1 latency cliff identified in Session 2,
confirmed in code.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Actual I/O times for typical conversation sizes. NVMe SSDs may make this
<1ms for most conversations. The concern is for conversations with 500+
messages on slower storage.
```

---

#### Finding 3.4.2 — Activity Tracker Backfill Reads All Segments Under Lock

```
FINDING: On first autonomy state creation, the handler reads every message
in every archived segment while holding the engine lock, blocking the
engine for potentially seconds on characters with large histories.

PREMISES:
- P1: handler.rs:535-575: activity backfill loop runs inside
  `engine_arc.lock().await` scope
- P2: engine/segments.rs: read_segment() performs synchronous file I/O
  to read JSONL segment files
- P3: For characters with many compaction cycles, there could be dozens
  of segment files

TRACED PATH:
- Step 1: First message to a character triggers autonomy state creation
- Step 2: Handler acquires engine lock (handler.rs:536)
- Step 3: Iterates all messages in engine (handler.rs:541)
- Step 4: Iterates all segments, reading each from disk (handler.rs:555-575)
- Step 5: Engine lock held during ALL file reads
- Step 6: For a character with 20 segments of 500 messages each, this
  reads ~10K messages from disk under lock

CONCLUSION: scalability. One-time cost per character per daemon restart,
but potentially seconds of engine lock contention. Should be moved outside
the lock or done lazily.

CONFIDENCE: HIGH
SEVERITY: MINOR (one-time per character startup)

WHAT I COULD NOT VERIFY:
Whether the autonomy state persists across daemon restarts or is recreated
each time. If recreated each time, this cost is incurred on every daemon
restart for every active character.
```

---

### 3.5 Failure Modes and Resilience

---

#### Finding 3.5.1 — MemoryDB Open Failure Kills Entire Generation

```
FINDING: If MemoryDB::open() fails in run_tool_phase (SQLite locked,
corrupted, or disk full), the entire generation fails rather than
degrading gracefully to a conversation without memory tools.

PREMISES:
- P1: handler.rs:931: `MemoryDB::open(&db_path).map_err(|e| format!(...))?`
- P2: This error propagates through run_tool_phase → handle_generation,
  which sends ServerMessage::Error to the client
- P3: The VectorStore open failure IS gracefully degraded (handler.rs:955-959)
- P4: The LLM's tool_use response may reference the memory tool, which
  cannot be dispatched without a MemoryDB

CONCLUSION: architectural. The asymmetry between VectorStore degradation
(graceful) and MemoryDB failure (fatal) is inconsistent. A corrupted
SQLite DB prevents all conversation, not just memory-augmented conversation.
The fix is to make MemoryDB optional in the tool phase and return a
descriptive error from the memory tool handler instead.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
How common SQLite open failures are in practice. With WAL mode and proper
file permissions, this may be rare. But disk-full conditions or hardware
failures could trigger it.
```

---

#### Finding 3.5.2 — Non-Atomic active.jsonl Rewrite on Crash

```
FINDING: Both MessageStore::persist() and compaction's archive_and_retain()
use bare std::fs::write() to rewrite active.jsonl, meaning a process crash
or power loss mid-write corrupts the active conversation file.

PREMISES:
- P1: engine/messages.rs:227: `std::fs::write(&self.path, &buf)`
- P2: compaction_impls.rs:429: `std::fs::write(&active_path, &retained_content)`
- P3: std::fs::write is open() + write() + close() — the file is truncated
  before new content is written
- P4: Neither uses the safe pattern of write-to-temp-then-rename,
  which is atomic on POSIX filesystems

TRACED PATH:
- Step 1: persist() serializes messages to JSON buffer
- Step 2: std::fs::write opens active.jsonl, truncates it, begins writing
- Step 3: Process crashes after truncation but before write completes
- Step 4: active.jsonl contains partial data — corrupted JSON lines
- Step 5: On restart, engine reload encounters malformed JSON
- Step 6: Depending on error handling in reload, messages may be silently
  dropped or the engine may fail to initialize

CONCLUSION: data durability. Active conversation history is vulnerable to
corruption on crash. The fix is to use temp-file-then-rename (std::fs::rename
is atomic on the same filesystem).

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
How the engine handles malformed JSONL during reload. If it skips bad lines
with a warning, data loss is partial. If it fails fatally, the character
becomes inaccessible.
```

---

#### Finding 3.5.3 — Mutex Poisoning Panics in Production Hot Path

```
FINDING: The diagnostics and session_tokens Mutexes are accessed via
.lock().unwrap() on every generation (the hot path), meaning a panic
anywhere else holding these locks will crash the handler loop.

PREMISES:
- P1: handler.rs:1046: `session_tokens.lock().unwrap()`
- P2: handler.rs:1068: `diagnostics.lock().unwrap()`
- P3: engine/tools.rs:179: `diag.lock().unwrap()`
- P4: These are std::sync::Mutex — a panic while holding the lock
  poisons it, causing all subsequent .unwrap() calls to panic
- P5: The autonomy manager correctly uses unwrap_or_else for poison
  recovery (manager.rs:526-530), but the handler does not

TRACED PATH:
- Step 1: Tool loop iteration calls diag.lock().unwrap() (tools.rs:179)
- Step 2: Suppose the tool dispatch panics (unexpected error path)
- Step 3: The diagnostics Mutex is poisoned
- Step 4: persist_and_notify calls diagnostics.lock().unwrap() (handler.rs:1068)
- Step 5: PANIC — the spawned tokio task panics, no error sent to client
- Step 6: User sees stream cut off with no error message

CONCLUSION: robustness. Mutex poisoning in production causes silent
failures. The fix is to use unwrap_or_else(|e| e.into_inner()) like
the autonomy manager already does.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether any current code path actually panics while holding these mutexes.
The risk is from future code changes or unexpected error conditions.
```

---

### 3.6 Daemon-Foundation Contract

---

#### Finding 3.6.1 — embed_text() Bypasses LlmClient with Plaintext API Key

```
FINDING: The embed_text() function in vectorstore.rs creates its own
reqwest::Client, constructs a raw JSON payload with the API key in
plaintext, and sends it directly to the embedding endpoint, bypassing
all LlmClient infrastructure (connection pooling, retry, logging,
ledger tracking).

PREMISES:
- P1: memory/vectorstore.rs:274: `let client = reqwest::Client::new()`
  — new client per embedding call
- P2: memory/vectorstore.rs:275-280: raw JSON with `"api_key": api_key`
  in the body
- P3: No retry logic, no exponential backoff, no ledger recording
- P4: This is called for every semantic search and every memory entry
  indexing operation
- P5: The compaction path uses RealVectorIndexer which goes through
  LlmClient.embed() (compaction_impls.rs:308), but the agent path
  uses embed_text() directly (via AgentSearchContext)

TRACED PATH:
- Step 1: Agent calls semantic_search tool during conversation
- Step 2: AgentSearchContext.embed_query() calls embed_text()
  (agent/types.rs:193-210)
- Step 3: New reqwest::Client created (vectorstore.rs:274)
- Step 4: API key sent in plaintext JSON body (vectorstore.rs:278)
- Step 5: No retry on failure — single attempt
- Step 6: No ledger recording — cost not tracked

CONCLUSION: architectural. Two different embedding paths exist:
LlmClient.embed() (for compaction) and embed_text() (for agent search).
The agent path lacks connection pooling, retry, logging, and cost tracking.
This is inconsistent with the rest of the LLM access architecture.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
Whether embed_text() is intended to call the shore-llm proxy (which
would handle API key management) or the embedding provider directly.
The `base_url` parameter suggests it goes through shore-llm, but the
api_key in the body suggests it may bypass the proxy's auth.
```

---

#### Finding 3.6.2 — Protocol and Config Compliance Are Clean

```
FINDING: The daemon correctly handles all ClientMessage variants defined
in shore-protocol and reads all configuration exclusively through
shore-config. No compliance gaps found.

PREMISES:
- P1: server/mod.rs handles all 5 ClientMessage variants: Hello, Message,
  Regen, Command, Cancel
- P2: handler.rs routes Engine messages to handle_generation and
  Command messages to dispatch_command
- P3: main.rs reads config via shore_config::load_config() exclusively
- P4: No daemon-specific config files are read outside shore-config
- P5: SHORE_TCP_ADDR env var in main.rs is a documented fallback

CONCLUSION: The daemon properly honors foundation crate contracts.
Protocol and config boundaries are clean.

CONFIDENCE: HIGH
SEVERITY: OBSERVATION (positive finding)

WHAT I COULD NOT VERIFY: None.
```

---

### 3.7 Rust-Specific Concerns

---

#### Finding 3.7.1 — Pervasive Blocking I/O on Async Runtime

```
FINDING: At least 20 std::fs operations execute on the tokio async runtime
without spawn_blocking, including multi-MB image reads and full-file
rewrites of active.jsonl.

PREMISES:
- P1: handler.rs:198: std::fs::read for image encoding (build_content)
- P2: handler.rs:781: std::fs::read for image encoding (build_llm_messages)
- P3: handler.rs:1170: std::fs::write for image save (ingest_images)
- P4: engine/messages.rs:227: std::fs::write for active.jsonl persistence
- P5: compaction_impls.rs:402-441: multiple std::fs writes
- P6: None of these use tokio::task::spawn_blocking or tokio::fs

CONCLUSION: architectural. For small files this is acceptable. For
image-heavy conversations or large active.jsonl, this blocks tokio
worker threads, causing latency spikes for all concurrent operations
on the runtime.

CONFIDENCE: HIGH
SEVERITY: SIGNIFICANT

WHAT I COULD NOT VERIFY:
The tokio runtime's thread pool size. With multi-threaded runtime
(default = num_cpus), blocking one thread may not be critical. But
with many concurrent I/O operations, it could exhaust the pool.
```

---

#### Finding 3.7.2 — Per-Request SQLite and LanceDB Construction

```
FINDING: Every tool loop invocation opens a fresh MemoryDB (SQLite
connection) and potentially a fresh VectorStore (LanceDB connection),
adding latency to every generation.

PREMISES:
- P1: handler.rs:929-931: MemoryDB::open() called fresh per run_tool_phase
- P2: handler.rs:949: VectorStore::open() called fresh per run_tool_phase
- P3: MemoryDB::open creates a new rusqlite::Connection, sets WAL mode,
  enables foreign keys, and runs schema migrations (db.rs:236)
- P4: VectorStore::open creates a new LanceDB connection (vectorstore.rs:57-67)
- P5: These are opened INSIDE the generation task, not shared across tasks

TRACED PATH:
- Step 1: User message triggers handle_generation
- Step 2: LLM responds with tool_use
- Step 3: run_tool_phase opens MemoryDB + VectorStore (handler.rs:929-962)
- Step 4: SQLite initialization: open file, run migrations, prepare statements
- Step 5: LanceDB initialization: open table, potentially create if missing
- Step 6: Tool loop runs with these fresh connections
- Step 7: Connections dropped at end of run_tool_phase

CONCLUSION: performance. Per-request DB construction adds latency to
every tool-using generation. SQLite connection pooling or a shared
MemoryDB instance per character would eliminate this overhead.

CONFIDENCE: HIGH
SEVERITY: MINOR

WHAT I COULD NOT VERIFY:
The actual latency of MemoryDB::open. SQLite connection creation is
typically <1ms, but schema migration checks add overhead.
```

---

## Phase 4: Synthesis

### 4.1 Architecture Assessment

Shore is a single-binary character chat engine built around a layered architecture: Unix socket SWP server → message handler → conversation engine → LLM client. The core idea — a daemon managing character state, conversation history, and LLM interactions through a well-defined protocol — is sound and well-executed. The layering between protocol, config, and LLM client crates is clean, with no compliance gaps. The system demonstrates iterative refinement: the `unsafe impl Sync` for MemoryDB is documented and justified, the autonomy manager correctly recovers from mutex poisoning, and the tool loop has hard iteration bounds.

The strongest parts of the design are the separation of concerns between crates (protocol, config, diagnostics, LLM client), the robust tool loop safety (hard bounds, proper error propagation back to the LLM), the graceful degradation of VectorStore failures, and the comprehensive ledger-based cost tracking. The conversation engine's broadcast-based history push model correctly implements the architecture doc's state-change notification pattern. The interiority system's 5-minute timeout and poison recovery show defensive engineering.

The most significant risks cluster around **data integrity** and **cost control**. The compaction system has no transactional guarantees across SQLite, LanceDB, and filesystem writes — a failure at any point leaves the system in an inconsistent state with no recovery mechanism. The autonomous message persistence bypasses the engine lock, creating a race condition with the handler's full-file rewrite. The memory retrieval pipeline's nesting (researcher → agent → tool loop) allows a single user message to trigger hundreds of API calls with no spending cap. These are not theoretical concerns — they are architecturally inevitable under load or failure conditions.

### 4.2 Top 10 Findings (Ranked by Severity, All Sessions)

| # | Finding | Severity | Source | Actionable? |
|---|---------|----------|--------|-------------|
| 1 | **Autonomous message race condition** — interiority writes to active.jsonl without engine lock, handler's persist() overwrites it (Finding 3.2.2) | CRITICAL | Session 3 | Yes — route autonomous messages through engine |
| 2 | **Compaction has no cross-store transactions** — SQLite + LanceDB + filesystem writes can diverge on failure, with no rollback and no dedup on retry (Finding 3.1.1) | CRITICAL | Session 3 | Yes — wrap in compensating transactions or use temp+rename |
| 3 | **Non-atomic active.jsonl rewrite** — bare std::fs::write risks data corruption on crash for both persist() and compaction (Finding 3.5.2) | SIGNIFICANT | Session 3 | Yes — use temp-file-then-rename |
| 4 | **Silent vector indexing failures in collation** — `let _ =` discards errors, creating entries invisible to semantic search (Finding 3.1.2) | SIGNIFICANT | Session 3 | Yes — propagate errors or retry |
| 5 | **Unbounded LLM call amplification** — researcher+agent nesting allows ~615 API calls per user message with no cost cap (Finding 3.1.3) | SIGNIFICANT | Sessions 2+3 | Yes — add per-query budget |
| 6 | **Generation task replacement without abort** — orphaned tasks continue consuming API credits (Finding 3.2.1) | SIGNIFICANT | Session 3 | Yes — abort previous handle before spawning |
| 7 | **Mutex poisoning panics in hot path** — diagnostics and session_tokens use .unwrap(), will panic on poisoning (Finding 3.5.3) | SIGNIFICANT | Sessions 1+3 | Yes — use unwrap_or_else recovery |
| 8 | **MemoryDB open failure kills generation** — no graceful degradation when SQLite unavailable (Finding 3.5.1) | SIGNIFICANT | Session 3 | Yes — make MemoryDB optional in tool phase |
| 9 | **MessageStore O(n) persist under engine lock** — full-file rewrite blocks concurrent access (Finding 3.4.1, carried from Session 2) | SIGNIFICANT | Sessions 2+3 | Yes — append-only for append(), batched rewrites |
| 10 | **embed_text() bypasses LlmClient** — no connection pooling, retry, logging, or ledger tracking (Finding 3.6.1) | SIGNIFICANT | Session 3 | Yes — unify through LlmClient |

### 4.3 Questions for the Developer

1. **Is the generation handle replacement intentional?** handler.rs:314 replaces `generation_handle` without aborting the previous task. Is this a deliberate design choice (let the previous generation complete) or a bug? If intentional, should the previous task's result be persisted?

2. **How common are conversations with 500+ messages?** The O(n) persist and context assembly costs scale with message count. Are there users with very long conversations, or is the typical conversation short enough that this doesn't matter?

3. **Is embed_text() going through shore-llm or directly to the provider?** The base_url parameter and api_key in the request body suggest it may bypass the shore-llm proxy. This affects whether the proxy handles auth, retry, and logging.

4. **Are the `let _ = idx.index_entry(...)` patterns in collation deliberate?** The compaction path treats indexing failures as hard errors (propagates via `?`), but collation silently swallows them. Is this an intentional difference (collation is best-effort) or an oversight?

5. **Should autonomous messages go through the engine?** The current direct-to-file append bypasses the engine's in-memory state and lock. Is there a reason autonomous messages can't be routed through `engine.append_message()` (possibly via a channel)?

6. **What is the expected BM25 entry count per character?** The O(n) scan in search.rs is fine for <1K entries but degrades at scale. Is there a typical upper bound?

7. **Is the chars/4 token heuristic acceptable for your primary use case?** If the user base is primarily English-speaking, the heuristic slightly over-truncates (conservative). If CJK-heavy, it under-counts and could cause API errors.

8. **Does the TUI disable the send button during generation?** This determines whether the orphaned-task scenario (Finding 3.2.1) can actually occur in practice.

### 4.4 What This Review Could Not Assess

1. **Runtime/load testing**: The latency cliffs (O(n) persist, BM25 scan, activity backfill) are analytically identified but their real-world impact depends on actual conversation sizes, hardware speed, and concurrent load. A load test with realistic conversation histories would quantify these.

2. **LLM provider behavior on context overflow**: If the token heuristic under-counts, the provider may reject the request, silently truncate, or charge for the full context. Testing against each provider (OpenRouter, Anthropic, OpenAI) would clarify the failure mode.

3. **LanceDB stability**: Several findings relate to LanceDB failures (indexing, reindex). The actual failure rate of LanceDB operations in production would determine whether these are theoretical or practical concerns.

4. **Interiority tick cost in practice**: Finding 3.2.3 identifies that interiority reuses full conversation context. The actual cost depends on prompt caching effectiveness and conversation length. Runtime measurement needed.

5. **Full collation system**: The collation module (1971 lines) was analyzed for the `let _ =` pattern and merge atomicity, but the clustering algorithm, candidate selection, and refine prompt were not fully analyzed. The O(N^2) clustering (clustering.rs:66) is flagged but its practical impact depends on batch_limit configuration.

6. **Compaction re-entry deduplication**: Finding 3.1.1 flags that re-compaction creates duplicate entries. The actual behavior depends on whether the LLM summarization produces sufficiently different entries on retry that they wouldn't be caught by a hypothetical content-hash check.

7. **Notification subsystem**: `notifications.rs` was not analyzed. It uses direct HTTP calls to ntfy/command backends, which may have its own failure modes and security considerations.
