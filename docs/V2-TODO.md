# Shore V2 — Remaining Work

Features that still need implementation or wiring to reach V1 parity.

Status key:
- STUB = code exists but returns placeholder/error
- MISSING = no code at all
- UNKNOWN = needs verification


## Priority 1: shore-llm Endpoints

These depend on shore-llm implementing the endpoints.

- 3.15 **Embedding endpoint** — STUB
  shore-llm /v1/embed returns 501. Needed for RAG vector search.

- 4.5 **generate_image** — STUB
  shore-llm /v1/image/generate returns 501.


## Priority 2: Tool Use

- 4.6 **web_search** (Tavily API + synthesis) — STUB
  Returns NotImplemented. Needs Tavily integration in daemon.

- ~~4.7 **fetch_url** (readable text extraction)~~ — DONE (reqwest + HTML stripping)

- 4.8 **research_web** (multi-step deep research) — STUB
  Returns NotImplemented. Depends on 4.6.

- 2.7 **Activity heatmap engine** — STUB
  Tool returns placeholder JSON. Heatmap data collection not implemented.

- 2.9 **Persist tool calls and reasoning in messages** — MISSING
  Tool calls (name, input, output, is_error) and thinking/reasoning content are
  streamed in real-time but discarded after generation. They should be persisted
  alongside Message so that `shore log` can display them and for debugging/audit.
  Best practice for most LLM SDKs. Blocked by: Message struct expansion, storage
  format decision, migration for existing conversations.


## Priority 3: CLI Features

### Messaging
- 5.2 Send with image attachment (-i flag) — MISSING
- ~~5.7 Log follow mode (-f/--follow)~~ — DONE
- ~~5.8 Log format options (--json/--content)~~ — DONE

### Conversation Management
- 5.13 Search conversations (full-text) — MISSING
- ~~5.14 Conversation info~~ — REMOVED (redundant with `shore status`)

### Message CRUD
- ~~5.18 Get message by index~~ — DONE (`shore get <ref>`)
- 5.19 Insert message at position — MISSING
- 5.20 Detach attachment — MISSING

### Character Management
- ~~5.24 Create character (scaffold directory)~~ — DONE (`shore character --new <name>`)

### Model Management
- ~~5.28 Reset to default~~ — DONE (`shore model --reset`)

### Memory CLI
- 5.32 Memory reindex — MISSING
- 5.33 Memory import (files → entries) — MISSING
- ~~5.34 Memory ask (one-shot agent)~~ — DONE (`shore memory "query"` runs one-shot agent)
- ~~5.36 Memory changelog~~ — DONE (`shore memory-changelog`)

### Configuration
- ~~5.38 Config show (all sections)~~ — DONE (`shore config` returns full config)
- ~~5.39 Config check (validation)~~ — DONE (`shore config --check`)
- 5.40 Config reset (clear overrides) — MISSING
- 5.41 **Config set (runtime)** — MISSING
  `shore config <key> <value>` should apply runtime config changes to the running daemon.
  CLI already accepts the value arg but the daemon ignores it. Should subsume
  `autonomy pause/resume` (removed as standalone command).


## Priority 4: Memory & Autonomy Extras

- ~~2.8 **Autonomy pause/resume**~~ — REMOVED (will be via `shore config set` once 5.41 is implemented)

- 3.5 **Consolidation** (write-time dedup via LLM) — UNKNOWN
  Needs verification — may be handled by memory agent create/supersede flow.


## Priority 5: Rendering & UX

- ~~7.2 Inline terminal images, Kitty/Ghostty (APC protocol)~~ — DONE (images.rs)
- ~~7.3 Inline terminal images, iTerm2 (OSC 1337)~~ — DONE (images.rs)
- ~~7.4 $SHORE_IMAGES override~~ — DONE (images.rs)
- 7.5 Rich markdown rendering — UNKNOWN (V2 renders streamed text, quality unverified)
- 7.6 Verbose spinner (token counts, cache hits, timing) — MISSING

### CLI Output Formatting

- ~~7.12 **NO_COLOR / `--no-color` support**~~ — DONE
- ~~7.13 **Phase indicator before first token**~~ — DONE
- ~~7.14 **Tool result truncation**~~ — DONE (500 char limit)
- ~~7.15 **Stream metadata abbreviation**~~ — DONE (strips date suffix)


## Priority 6: Resilience & Observability

- 8.5 **shore-llm lifecycle robustness** — MISSING
  When shore-llm is running externally (no `[services.llm] command`), the daemon
  gives no feedback if the socket is missing — `send` just fails with a connection
  error. Improvements: (1) check llm.sock reachability at startup and warn,
  (2) clearer error message ("shore-llm is not running" vs raw socket error),
  (3) consider auto-discovery of externally-started shore-llm processes.

- 8.1 In-memory ring buffers (API calls, tools, errors) — MISSING
- 8.2 API payload logging (api_payloads.jsonl) — MISSING
- 8.3 Cache debug guards — MISSING (config has cache_invalidation_warnings bool)
- ~~8.4 Status sections (filtered view)~~ — DONE (`shore status --section <name>`)


## Priority 7: Other CLI

- 5.44 Push notifications (shore notify) — MISSING
- 5.45 Failed message list — MISSING
- 5.46 Failed message retry — MISSING
- 5.47 Failed message clear — MISSING
- 5.48 Cache suppress — MISSING
- 5.49 Cache unsuppress — MISSING
- 5.50 Images list (CLI-level browsing) — MISSING
- 5.51 Images import — MISSING
- 5.52 Images describe (vision model) — MISSING
