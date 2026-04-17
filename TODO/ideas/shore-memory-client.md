# Shore Memory Client

A separate GUI client focused entirely on browsing, inspecting, and editing Shore's memory layer. Not a chat client, not a daemon admin tool — a dedicated lens on the memory subsystem.

## Goals

- Make the memory DB legible. Today the only way to see what's in there is `memory_query` (RAG-scored), the TUI's limited inspector, or opening SQLite directly.
- Make memory editable without raw SQL. Fix typos, correct facts, pin entries, delete noise.
- Make retrieval behavior debuggable. When Shore surfaces the wrong thing, you should be able to replay the query and see the scored candidate list.
- Make pipeline effects auditable. Compaction and the 5-phase collation mutate memory in batches — you should be able to review what changed.

### Scope ceiling: maximum

- Table view of memory rows: inspect full text, edit text, delete.
- RAG-scored retrieval preview: for a given query, show which memories would surface and at what score.
- Metadata visibility: created time, last-accessed time, access count, origin.
- Embedding-space / cluster visualization (LanceDB-backed).
- Memory graph view (relationships, references).
- Audit trail per memory entry.

## Non-goals

- Chat. Conversation state belongs in the TUI / GUI / matrix clients. This client never renders a conversation turn.
- Autonomy, model switching, character switching, config editing. Orthogonal surfaces — they belong to other clients.
- Live "tail the memory system" feed. Not a log viewer. If that's useful, it's a separate thing.
- Replacing the SQLite/LanceDB files as source of truth. The client is a view over daemon state, never a parallel persistence layer.

## User workflows

Ranked by primacy. MVP must cover 1–2. Stretch covers the rest.

1. **Triage a bad retrieval.** Shore said something wrong or stale. Open the client, run the same query, see the scored candidate list the RAG would have produced, inspect the offending entry's full text and metadata, then edit / mark superseded / delete. The "why did Shore think that?" loop.

2. **Manual curation.** Direct CRUD on a specific entry — fix a typo, correct a factual error, toggle `status=protected` to pin something, delete an entry outright. Table view with filter + inline edit. The "I just want to fix this one thing" loop.

3. **Audit a collation pass.** After the 5-phase pipeline runs (backfill → collate → tidy → normalize → decay), review what changed: merges into canonicals, splits, confidence decays. Driven by the `changelog` table. The "did the pipeline do something dumb?" loop.

4. **Spot-check a compaction.** When a conversation gets compacted into memory, review the new entries before they drift into the general pool. Filter by recent `created_at` + `source=summary`.

5. **Investigate an entity.** Pick a person/place/thing from `entities`, see all linked entries, surface contradictions, watch confidence over time. The "what does Shore believe about X?" loop.

6. **Explore the embedding space.** LanceDB-backed cluster visualization. Orphans (low cosine similarity to everything) and tight pairs (duplicates the collator missed) are the two high-value signals.

7. **Resolve flags.** The `flags` table surfaces entries the system has marked as suspicious. Work through them, set `resolved_at` + `resolution`.

## MVP cut

Workflows 1–2 only. Everything else is a follow-up.

Concretely, MVP is:
- Paginated table over `entries` with sort + filter (status, memory_type, source, created_at range, topic_key, text contains).
- Row detail pane: full `summary_text`, all metadata, linked entities, source/related entry IDs, superseded_by, flags.
- Edit `summary_text`, toggle `status`, delete.
- Query bar that runs RAG against the daemon and shows scored results — clicking a result opens its row.

Everything in workflows 3–7 assumes MVP is solid.

## Remote capability

Yes. LAN / Tailscale. Same transport as the TUI — SWP over TCP with the same auth flow.

## SWP surface audit

### What exists today

Evidence: `shore-daemon/src/commands/state/memory.rs`, MCP wrappers at `shore-mcp/src/tools/memory.rs`.

| Command | What it does | R/W |
|---|---|---|
| `memory` (no query) | Entry / entity / active counts for current character | R |
| `memory` (with query) | RAG → LLM agent synthesizes a prose answer | R (may write via agent tools) |
| `memory_changelog` | Last N changelog rows. No filtering. | R |
| `compact` | Conversation → memory entries. Has `dry_run`. | W |
| `collate` | 5-phase refinement pipeline. No `dry_run`. | W |
| `memory_purge` | Bulk delete old superseded entries. Only deletion path. | W |
| `memory_reindex` | Rebuild FTS5 + LanceDB. | W |
| `memory_shell_start` / `_query` / `_end` | Interactive LLM-driven memory session | R/W |

### The mismatch

Every entry/entity mutation today goes through an LLM tool loop. The memory agent has ten tools (`create_entry`, `update_entry`, `supersede_entry`, `update_entity`, `merge_entity`, `create_flag`, `resolve_flag`, `query_db`, `search_entries`, `semantic_search`) — that's the entire write surface. There is no direct wire command to edit a row by ID, list entries, get an entry by ID, list entities, or retrieve scored RAG candidates.

That's fine inside a chat loop where an LLM is already spinning. It's the wrong model for a GUI inspector/editor where clicking "save" should fire a deterministic round-trip, not an LLM call. MVP is blocked on adding a direct, LLM-free memory CRUD lane to SWP.

### Gap analysis — MVP (workflows 1–2)

| Need | Current path | Status |
|---|---|---|
| List + paginate entries with filters (status, memory_type, source, topic_key, date range, text contains) | Agent's `query_db` only | Missing |
| Get entry by ID with full metadata, linked entities, flags | Agent's `query_db` only | Missing |
| Edit `summary_text` / `topic_tags` / `confidence` | Agent tool `update_entry` (LLM-mediated) | Needs direct command |
| Toggle `status` (active / protected / superseded) | Agent tools `update_entry` / `supersede_entry` | Needs direct command |
| Delete single entry by ID | None — `memory_purge` is bulk-by-age-and-status | Missing |
| Scored RAG candidate list | RAG pipeline computes scores but never puts them on the wire — `memory_query` returns synthesized text only | Missing |

### Gap analysis — stretch (workflows 3–7)

| Need | Current path | Status |
|---|---|---|
| Filter changelog by entry_id / operation / time range | `memory_changelog` is last-N only | Needs filters |
| Resolve changelog row → affected entries/entities | `changelog_entries` / `changelog_entities` tables exist, unexposed | Missing |
| List entities; get entries for an entity | Agent's `query_db` only | Missing |
| List flags; resolve a flag | Agent tools only | Needs direct command |
| Vector neighbors for a given entry | Vector store is daemon-internal | Missing |
| Dry-run a `collate` pass | Not supported — only `compact` has `dry_run` | Add flag |

### Proposed additions

All direct, no LLM in the path. The existing agent tools stay — this is a parallel lane for deterministic UIs.

**MVP**

- `memory_list { status?, memory_type?, source?, topic_key?, contains?, created_after?, created_before?, limit, offset, sort_by?, sort_dir? } → { entries, total_count }`
- `memory_get { id } → { entry, entities, flags, linked: { superseded_by, source_entry_ids, related_entry_ids } }`
- `memory_update { id, summary_text?, topic_tags?, confidence?, status?, expected_updated_at } → { entry }` — optimistic lock on `expected_updated_at`; conflict → client reload
- `memory_delete { id, reason, force? } → { deleted }` — single-entry delete, writes changelog. Default enforces `memory_purge`'s guards (no image, replacements must be active); `force: true` bypasses with a changelog note
- `memory_query_ranked { query, limit?, include_superseded? } → { results: [{ entry, score, score_breakdown }] }` — exposes the RAG pipeline's real output, no agent synthesis

**Stretch**

- `memory_changelog { entry_id?, operation?, since?, until?, limit, offset }` — extend existing command with filters, include `affected_entries` / `affected_entities` per row
- `entity_list { type?, contains?, limit, offset }`
- `entity_get { id_or_name } → { entity, entry_count, recent_entries }`
- `memory_flag_list { resolved?, flag_type?, entry_id?, limit, offset }`
- `memory_flag_resolve { flag_id, resolution }`
- `memory_vector_neighbors { entry_id | embedding, k } → { neighbors: [{ entry_id, cosine }] }` — raw vector search, no BM25 fusion, no lifecycle scoring
- `collate { full?, limit?, dry_run? }` — add `dry_run` symmetric with `compact`

### Design notes

- **Concurrent edits.** `memory_update` takes `expected_updated_at`; daemon rejects with a conflict response if stale. Client re-reads and re-prompts. No locks, no CAS complexity.
- **Agent tools keep existing.** Do not replace the agent's ten tools — those stay for LLM-driven workflows (memory shell, autonomy, in-conversation writes). This is a parallel surface for deterministic clients.
- **Changelog coverage.** Every new mutating command writes `changelog` + `changelog_entries`. `memory_update` records old→new deltas so the audit trail is actually useful downstream.
- **No raw SQL on the wire.** Resist exposing `query_db` client-side. It's fine as an agent tool (the LLM tolerates schema coupling) but a bad SWP primitive (injection, migration fragility, schema leak). Build specific queries.
- **Columns vs docs.** `docs/ARCHITECTURE.md` §9 still lists `canonical` on `entries`; code dropped it in migration v2.3 (`shore-daemon/src/memory/db.rs:95–100`). Update the docs as part of this work.

## Execution plan

Phased so each phase is independently shippable. Client scaffolding can start against Phase 1 while Phase 2 / 3 land.

### Phase 1 — Docs drift + SWP read surface

Unblocks triage and curation reads, plus the client prototype.

- Audit `docs/ARCHITECTURE.md` §9 for schema drift. `canonical` is the known case (dropped in migration v2.3); may not be the only one. Broader than a single-column fix.
- Add `memory_list` and `memory_get` to SWP.
- MemoryDB gains one pagination helper and one "fetch entry + joins" helper. No mutation risk.

### Phase 2 — SWP write surface

Closes out workflows 1–2.

- Add `memory_update` with an `UpdateConflict` error type. Conflict response returns the current row so the client can diff and re-prompt.
- Add `memory_delete`. Default enforces `memory_purge`'s guards (image-attached, replacements-active). `force: true` bypasses with a changelog note.
- Both write `changelog` + `changelog_entries` rows. `memory_update` records field-level old→new deltas so the audit trail is actually useful downstream.

### Phase 3 — Scored RAG exposure

Closes workflow 1's retrieval-preview piece.

- Add `memory_query_ranked`. Returns `{ entry, score, score_breakdown }` with labeled breakdown components (`vector`, `bm25`, `lifecycle_multiplier`, `recency_boost`, `confidence_penalty`) so future scoring-pipeline changes stay additive.
- Does not replace `memory_query`. They coexist — the synthesized path remains for agent callers.

### Stretch — after Phase 3

Each is independent and cheap once the above is in:

- Changelog filters + affected_entries on rows.
- `entity_list` / `entity_get`.
- `memory_flag_list` / `memory_flag_resolve`.
- `memory_vector_neighbors`.
- `collate { dry_run }`.

### Risks to name before starting

- **Names are one-way doors.** Once Phase 1 ships and any external caller (MCP, future client) binds to them, renames are painful. Lock command and field names before Phase 1 merges.
- **`score_breakdown` is a contract.** Label components, don't order-depend. Additions should be non-breaking.
- **Drift almost certainly isn't just `canonical`.** Phase 1's docs audit must be broader than the one column I caught in this pass.

### Parallel track

Language/framework is decided (see next section). Client scaffolding can start against Phase 1 in parallel with Phase 2/3 daemon work.

## Language & framework

**Decision: TypeScript + Tauri, with a thin Rust backend inside Tauri that owns the SWP wire.**

### Why

- **Best-in-class UI ecosystem where it matters most.** Table (TanStack Table, AG Grid), embedding viz (deck.gl, plotly, d3), graph view (cytoscape.js, react-flow). Rust equivalents exist but lag badly on the stretch goals. MVP is table-heavy; stretch is viz-heavy. TS wins both.
- **Rust backend reuses `shore-client`.** The TS side never touches raw SWP — it calls `tauri::command`s that the Rust side translates into `shore-client` calls. Zero protocol duplication. The Rust shell is ~500 LOC at most.
- **Type sharing via `ts-rs` or `typeshare`.** Types are defined once in Rust and generated into TS from the Tauri command signatures — not from `shore-protocol` directly. The Tauri command layer is a narrower, more stable contract than the full wire protocol.
- **Distribution:** single binary per platform, ~5–15 MB. Tailscale/LAN remote works because the Rust shell opens a TCP socket like any other SWP client.
- **Consistent with the language-agnostic clients policy** (`feedback_clients_language_agnostic.md`): pick the best tool for the job, not Rust-by-default.

### Ruled-out alternatives

- **egui (pure Rust).** Great for MVP table. Bad story for cluster viz and graph viz — forces a stretch-phase rewrite or a separate sidecar tool. The viz gap is the deciding factor.
- **Pure TS with `bun build --compile` (no Tauri, no Rust).** Means re-implementing SWP framing in TS. Per the language-agnostic memory, that kind of friction is worth fixing in SWP itself — but as of today it's real, and keeping Rust as the wire owner is cheaper than paying that cost up front.
- **Dioxus / Iced.** Webview-based Rust UI inherits the "need JS viz libs" problem without the TS ecosystem convenience.
- **Native Qt / GTK.** Best-in-class table widgets, but build-system complexity and weak macOS story.
- **SwiftUI.** macOS-only, disqualifying for Linux primary.
- **Godot.** Already in-stack as `shore-gui`, but that's a joke crate and Godot's table/form UI story is poor.

### Honest costs

- Two build systems (cargo + pnpm/bun). Manageable, but real — CI and local dev both need both toolchains.
- **Distribution workflow needs adjusting.** Current Shore release flow (CHANGELOG bump, tag, push, Gitea deploy workflow) assumes a pure-Rust workspace. Adding a TS+Tauri client means the release pipeline needs a Node/Bun toolchain step, a Tauri bundle step, and platform-matrix builds (Linux `.AppImage`/`.deb`, macOS `.app`/`.dmg`). Worth scoping as its own task alongside Phase 1 — not a blocker, but doesn't come for free.
- If SWP turns out to be awkward to speak from even the Rust backend, that's the "priority-1 fix SWP" path from the language-agnostic memory — which is the right outcome, not a workaround.

## Read-path architecture

**Decision: all-SWP. Both reads and writes go through the daemon.**

### Why

- **Remote is a first-class requirement.** The spec commits to LAN / Tailscale operation. Direct SQLite reads don't work when the DB file lives on another machine. A hybrid "direct when local, SWP when remote" design means two code paths, two bug surfaces, and two test matrices — a complexity cost that swamps any local-read speedup.
- **Schema coupling is the exact thing the language-agnostic policy warns against** (`feedback_clients_language_agnostic.md`: "Keep `shore-protocol` documented as a wire spec, not a Rust-types-only artifact"). Direct-FS reads bind the client to SQLite table shapes. Every migration becomes a client migration. SWP gives us a stable, versioned contract.
- **RAG scoring only exists daemon-side.** `memory_query_ranked`'s `score_breakdown` is computed from the RAG pipeline (vector + BM25 + lifecycle). A direct SQLite reader can't replicate that without duplicating the pipeline. So retrieval-preview must go through SWP anyway — direct-FS would only optimize the plain table view, not the whole surface.
- **Character/profile scoping lives in the daemon.** Which `memory.db` is "active" is determined by daemon state (selected character, profile dir). Replicating that resolution in the client is leak and drift.
- **LanceDB has its own concurrency rules.** Vector-side reads outside the daemon would require a second direct handle in a second format. Not worth it for a stretch feature.
- **All-SWP matches the goals.** The spec already commits to "client is a view over daemon state, never a parallel persistence layer." Direct-FS contradicts that.

### Performance expectations

- Local Unix socket: sub-millisecond round-trip. Not a bottleneck.
- LAN: single-digit ms. Fine for interactive pagination.
- Tailscale: 10–30 ms typical. Noticeable but acceptable with prefetching.

### Revisit conditions

Go back to this decision only if:

- Interactive filtering on a full-table view (≥10k entries) exceeds ~100 ms end-to-end round-trip after SWP-level optimization (server-side pagination, filter push-down, indexed sort).
- The fix should be better SWP design (batch fetch, client-side result cache keyed on `updated_at`, long-poll for invalidation) — not bypass-the-daemon.

## View specs — MVP

One workhorse screen covers both MVP workflows (triage + curation). Stretch views are described briefly at the end for forward-compatibility.

### Main view — Browse & Triage

Three regions. Structure, not pixel layout:

```
┌──────────────────────────────────────────────────────────────────────┐
│ Control bar: character │ mode toggle │ filter/query input │ pager    │
├───────────────────────────────────────┬──────────────────────────────┤
│                                       │                              │
│           Table (2/3 width)           │    Detail pane (1/3 width)   │
│                                       │                              │
├───────────────────────────────────────┴──────────────────────────────┤
│ Status bar: daemon socket │ connection state │ total count │ version │
└──────────────────────────────────────────────────────────────────────┘
```

**Control bar**
- Character selector (read-only in MVP — displays current character as resolved by daemon; switching characters is out of scope for this client).
- Mode toggle: `Filter` | `Query`.
  - `Filter`: structured chips — status (multi-select: active / protected / superseded), memory_type, source, topic_key, date range, free-text `contains`. Drives `memory_list`.
  - `Query`: single text input. Drives `memory_query_ranked`. Results replace the table rows and the `Score` column becomes visible.
- Pagination controls (prev / next / jump-to-page).

**Table**
- Columns: checkbox (for future bulk ops), ID (truncated, tooltip full), memory_type (icon), status (badge), summary_text (one-line truncation), topic_key, updated_at (relative), score (Query mode only).
- Virtualized rows — MVP target is smooth with ≥10k entries.
- Sort: click column header. In Query mode, default sort is score DESC; clicking another header re-sorts the scored result set client-side (fast, set is small).
- Row click → detail pane. No modal.
- Empty states: "No entries match. Clear filters?" / "No memory entries yet. Use `shore compact` to generate from conversation."

**Detail pane**
- Header: full ID (click-to-copy), `Edit`, `Delete`.
- Body, in order:
  - `summary_text` — main readable block
  - Metadata block: memory_type, status (as badge), confidence, source, reason, created_at, updated_at, collated_at
  - Entities: linked entity pills (name + type)
  - Relationships: `superseded_by`, `source_entry_ids`, `related_entry_ids` — each a clickable ID that selects that entry in the table (and pages to it if needed)
  - Flags: active flags on this entry (flag_type, reason, created_at). Resolve button is present but disabled in MVP (stretch).
  - Timestamps: `start_timestamp`, `end_timestamp` (episodic range).
  - In Query mode only: **"Why did this surface?"** — `score_breakdown` rendered as a labeled mini bar (vector / bm25 / lifecycle_multiplier / recency_boost / confidence_penalty).
- Empty state: "Select an entry to inspect."

**Status bar**
- Daemon socket path (or remote host for Tailscale/LAN sessions).
- Connection state — green / yellow (reconnecting) / red (offline).
- Total entries count for current filter.
- Client version.

### Edit flow

Clicking `Edit` in the detail pane flips editable fields in place (no modal):

- `summary_text` → textarea
- `status` → dropdown
- `confidence` → number input 0.0–1.0
- `topic_tags` → text input (comma-separated; upgrade to chips in stretch)

`Save` / `Cancel` buttons appear. Save flow:

1. Capture `expected_updated_at` from the currently loaded row.
2. Call `memory_update { id, ..., expected_updated_at }`.
3. On success: refresh detail, toast "Updated".
4. On `UpdateConflict`: modal showing side-by-side diff (daemon's current vs your edit). Options: `Reload` (discard local edits) or `Overwrite` (re-call with the daemon's new `expected_updated_at`, losing the background change).
5. On transport error: toast, stay in edit mode.

### Delete flow

Clicking `Delete`:

1. Modal with entry ID, summary preview, required `Reason` text input.
2. First call: `memory_delete { id, reason }` (no force — daemon enforces guards).
3. On guard-reject (image attached, or replacements not all active): modal expands to explain which guard rejected, plus `Force delete (bypass guards)` checkbox. User re-confirms → `memory_delete { id, reason, force: true }`.
4. On success: toast, detail clears, table row removed, selection moves to next row.

### Keyboard shortcuts (MVP minimum)

- `/` focus the filter/query input
- `Esc` clear selection or cancel edit
- `Cmd/Ctrl+E` edit selected entry
- `Cmd/Ctrl+D` delete selected entry (with confirm)
- `↑` / `↓` navigate rows
- `Cmd/Ctrl+Enter` submit edit form
- `Cmd/Ctrl+R` refresh table

### Edge cases explicitly covered

- **Offline / daemon unreachable.** Status bar red. Table shows "Not connected to daemon. Retry." with button.
- **Remote connection stalls mid-pagination.** Partial results shown, toast surfaces the failure, `Retry` restores the in-flight request.
- **Empty memory DB.** Table empty-state copy points at `shore compact`.
- **Character has no entries.** Same as empty.
- **Entry deleted by another client while open.** Detail pane shows "This entry was deleted" overlay with `Close` button.

### Visual direction

Chosen: **dense utility** (see `shore-memory-client-mocks/01-dense-utility.html`).

Principles to carry into implementation:

- **Dark default**, high-contrast. `#0d1115` canvas, `#58a6ff` accent, status colors (green active / amber protected / dim superseded) working hard.
- **Monospace-leaning for data**, sans for prose/chrome. IDs, timestamps, metadata grids, and topic keys are mono; summary_text and section headings are sans. The point is scannability at a glance.
- **Tight row height** (~28px) with virtualized rows. 10k entries must feel fluid.
- **Sharp rectangles, 1–2px borders, minimal radius.** No soft shadows, no rounded cards. The client is a tool, not a document.
- **Information density over breathing room.** Detail pane packs metadata, topic tags, entities, relationships, and flags without padding inflation.
- **Accent-outline selection** (cyan outline + dark-blue background fill on the selected row) — selection is unambiguous without being loud.

Mocks `02-quiet-native.html` and `03-research-notebook.html` are rejected but kept in the mocks dir for reference if a future reader wants to understand what was considered.

### Stretch views — forward compatibility

Not MVP, but called out so the main view's information-density doesn't paint the stretch work into a corner:

- **Changelog audit** (workflow 3). Timeline view of `memory_changelog` rows with filters by entry_id / operation / time range. Each row links to the affected entries. Likely a separate tab.
- **Entity-centric view** (workflow 5). Sidebar lists entities from `entity_list`; selecting one drives the main table filter to entries linked to that entity.
- **Flag queue** (workflow 7). Same shape as the main table but pre-filtered to entries with open flags. `Resolve` action in the detail pane enabled.
- **Embedding cluster view** (workflow 6). Separate tab. 2D scatter (UMAP-projected vectors, computed daemon-side and cached) rendered with deck.gl. Brushing/selection links back to the main table. Requires `memory_vector_neighbors` and a new "list all vectors with 2D projection" SWP command — add to the stretch surface list when we get there.

## Open questions

None currently outstanding. Spec is ready for prototype.
