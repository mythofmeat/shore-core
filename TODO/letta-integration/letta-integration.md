# Letta Integration

Replace Shore's custom memory layer (SQLite + FTS + Lance + researcher LLM +
memory agent LLM) with [Letta](https://www.letta.com/) as the retrieval and
state-management backend. Work lives on branch `letta-integration`.

## Why we're doing this

Documented in `project_letta_memory_replacement.md`, summarized here:

- Shore's LoCoMo accuracy plateaued at 32.5%; iteration to 25% correct on
  the temporal category was expensive and the gap to mem0's 91% / Letta's
  comparable numbers was not closing.
- The Whiskers parity test (5 difficulty levels of the dead-cat scenario)
  showed mem0 passes 5/5 without *any* explicit supersession logic — the
  extraction LLM at ingest handles state transitions that Shore's
  supersession model was specifically designed to fix. Letta uses the same
  extract-at-ingest pattern.
- Shore's 2-LLM query path costs $0.01+ per read. Letta front-loads LLM cost
  at ingest and keeps retrieval cheap — a large delta for read-heavy usage.
- Letta's agent-driven memory (tools on the agent; the agent decides when to
  read/write) is closer to Shore's original vision than mem0's pure-retrieval
  API.

## Architecture (target)

```
┌─────────────┐   SWP    ┌───────────────────┐  HTTP/unix  ┌────────────┐
│ shore-daemon│ ────────▶│ shore-daemon      │ ──────────▶ │ letta-py   │
│ (Rust)      │          │ memory bridge     │             │ sidecar    │
└─────────────┘          │ (crate)           │             │ (Python)   │
                         └───────────────────┘             └────────────┘
                                                                  │
                                                           ┌──────▼──────┐
                                                           │  Letta lib  │
                                                           │  + storage  │
                                                           └─────────────┘
```

- **Python sidecar**: long-running process. Owns the Letta agent + storage.
  Speaks to shore-daemon over a local unix socket (or localhost HTTP if
  simpler for initial wiring). Started/supervised by shore-daemon.
- **Rust bridge**: replaces the `memory/` module. Exposes the same SWP-level
  shape (memory read/write tools) but delegates to the sidecar. No
  researcher, no memory_agent LLM, no SQLite/Lance — all retired.
- **Storage**: Letta's own (SQLite by default). No Shore-side memory DB.

### Why a sidecar instead of PyO3

Letta is a non-trivial Python library with heavy deps (sqlalchemy, a
vectorstore, embedding models, LLM clients). Embedding that via PyO3 pulls
the GIL and a Python runtime into shore-daemon — ugly and fragile across
Python versions (Arch ships 3.14; mem0/fastembed still segfaults on 3.14, so
we already need a specific venv — see `reference_python_314_segfault.md`).
A sidecar keeps the Python runtime contained, isolates crashes, and lets us
swap Letta for something else later without touching Rust.

### Wire shape (initial)

Letta exposes an OpenAPI-style HTTP API. Easiest path: run the Letta server
locally and have Shore be a thin client. Defer custom protocol work; only
bring it in-house if Letta's surface is wrong for us.

## Phases

### Phase 0 — parity check (gate: does Letta actually pass Whiskers?)

**DONE 2026-04-21.** Letta passes 5/5 on the Whiskers scenarios using
`openrouter/anthropic/claude-haiku-4.5` + `openai/text-embedding-3-small`.
Results in `experiments/memory-framework-eval/results_letta.json`.

- [x] Install Letta 0.16.7 in a parallel venv
      (`experiments/memory-framework-eval/.venv-letta`, Python 3.12).
- [x] Stand up a contained Letta stack: `start_letta_stack.py` spawns
      pgserver (embedded Postgres, pgvector included) plus the Letta REST
      server bound to 127.0.0.1:8283. Self-contained — no sudo, no
      system-level deps.
- [x] Write `run_letta.py` mirroring `run_mem0.py`: same `scenarios.py`,
      same judge format, same response-synthesis pattern (in Letta's case
      the agent itself synthesizes the response in the final turn).
- [x] Run all 5 levels. 5/5 PASS.
- [x] Commit test + results + stack launcher.

Gotchas worth remembering (move to QUIRKS during Phase 2):
- Letta 0.16.7 requires Postgres — its server's db.py hardcodes asyncpg
  even when `DatabaseChoice.SQLITE` is reported. The SQLite path is only
  wired for ORM column-type dispatch, not connection setup.
- The pip wheel doesn't ship alembic migrations; `Base.metadata.create_all`
  misses the runtime sequence for `messages.sequence_id`. We install it
  manually after create_all (`CREATE SEQUENCE ... OWNED BY`).
- pgserver ships pgvector bundled; no system postgres install needed.

### Phase 1 — sidecar sketch (standalone, no Shore)

- [ ] Pick transport: start with Letta's own HTTP server if that's the
      sanctioned path; otherwise a tiny FastAPI wrapper. Decide after reading
      Letta's docs.
- [ ] Decide agent model wiring: we want the sidecar to use the same
      OpenRouter key Shore uses. Figure out how to point Letta's LLM config
      at OpenRouter (or run it with a local Anthropic/OpenAI-compat shim).
- [ ] Run the sidecar by hand, push a few messages through it with `curl`,
      confirm it round-trips memory.

### Phase 2 — Rust bridge crate

- [ ] New crate `shore-memory-letta` (or similar). Owns the HTTP client,
      the sidecar process lifecycle (spawn, health-check, shutdown), and
      the shape that replaces `shore-daemon/src/memory/mod.rs`.
- [ ] Map existing SWP memory tool calls onto Letta API calls. Preserve
      tool names so clients don't break.
- [ ] Writes: ingest the user/character turns as Letta "messages" so the
      extraction LLM does its thing.
- [ ] Reads: replace the researcher call with a Letta search/context call.
      Preserve the response shape expected by the memory agent's callers,
      or delete the memory agent entirely if Letta's surface is enough.

### Phase 3 — retire Shore's memory code

Only once Phase 2 is wired and green end-to-end.

- [ ] Delete `shore-daemon/src/memory/{agent,agent_llm,collation,
      collation_impls,compaction,compaction_impls,db,rag,researcher,
      search,vectorstore}.rs`. Keep only the thin bridge.
- [ ] Drop the memory DB migration code; no SQLite/Lance anymore.
- [ ] Remove `memory_agent` / `tool_model` prompt files.
- [ ] Update CLI/TUI surfaces that name "researcher" / "memory agent" —
      these are now Letta internals.
- [ ] Update `docs/ARCHITECTURE.md` with the new memory diagram.
- [ ] Update `docs/DECISIONS.md` with the "why Letta, why now" entry.
- [ ] Update `docs/QUIRKS.md` with the Python sidecar gotchas (3.12 venv,
      sidecar lifecycle, OpenRouter wiring).

### Phase 4 — data migration

- [ ] Export Shore's existing memory DB to Letta-ingestable form (messages
      with timestamps, probably).
- [ ] One-shot migration script: read old DB, push into Letta sidecar via
      the same ingest path the daemon uses in prod. Do *not* try to
      preserve the internal structure — let Letta's extraction re-derive it.
- [ ] Run the migration on a test profile first; diff retrieval behavior
      on a known conversation before blessing it for the main profile.

### Phase 5 — live verification

- [ ] `cargo test --workspace`
- [ ] `cargo test --test e2e -- --ignored`
- [ ] `./scripts/live-tests/live-test.sh`
- [ ] Drive through shore-mcp against a real LLM on the test profile.
- [ ] Re-run Shore's own LoCoMo bench (the one in
      `experiments/auto-memory-gate/shore-bench/`) through the Letta-backed
      daemon. Compare to the 32.5% / 25%-temporal baseline.

## Non-goals (for this branch)

- Keeping Shore's memory code as a fallback. It's going.
- Preserving the `memory_agent` / `researcher` naming internally; rename
  or delete as appropriate once the Rust bridge is in place (see the
  already-planned rename note in `project_memory_agent_rename_planned.md`
  — that work lapses if the layer is deleted, which is the expected
  outcome).
- Supersession, collation, compaction as Shore-owned concepts. Letta's
  extraction handles the same work at ingest.

## Open questions

- **Local vs hosted Letta**: is there a meaningful difference in behavior?
  For now we run local. Revisit only if local is structurally worse.
- **Character prompts / system prompt**: Letta treats the agent's system
  prompt as first-class. Where does Shore's character prompt live in this
  world — the Letta agent's system prompt, or still Shore-side with Letta
  treated as a retrieval-only service? Suspect the former is more
  idiomatic; decide after Phase 1.
- **Multi-character**: Shore supports multiple characters per profile.
  Letta agents are 1:1 with a "user". Probably one Letta agent per Shore
  character. Confirm during Phase 1.

## Related memory / docs

- `project_letta_memory_replacement.md` — why we're doing this
- `reference_python_314_segfault.md` — use Python 3.12 venv
- `experiments/memory-framework-eval/` — Whiskers test + results for mem0;
  Letta results land here too
- `experiments/auto-memory-gate/shore-bench/` — LoCoMo harness for post-
  integration verification
