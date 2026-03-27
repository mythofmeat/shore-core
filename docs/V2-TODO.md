# Shore V2 — Remaining Work

Features that still need implementation or wiring to reach V1 parity.

Status key:
- STUB = code exists but returns placeholder/error
- MISSING = no code at all
- UNKNOWN = needs verification


## Priority 1: shore-llm Endpoints

These depend on shore-llm implementing the endpoints.

- ~~3.15 **Embedding endpoint**~~ — DONE (openai.embed() + daemon LlmClient.embed() + RealVectorIndexer)

- 4.5 **generate_image** — STUB
  shore-llm /v1/image/generate returns 501.


## Priority 2: Tool Use

- ~~4.6 **web_search**~~ — moved to V2-NEEDS-DESIGN.md

- ~~4.7 **fetch_url** (readable text extraction)~~ — DONE (reqwest + HTML stripping)

- ~~4.8 **research_web**~~ — moved to V2-NEEDS-DESIGN.md (depends on 4.6)

- 2.7 **Activity heatmap engine** — STUB
  Tool returns placeholder JSON. Heatmap data collection not implemented.

- ~~2.9 **Persist tool calls and reasoning in messages**~~ — moved to V2-NEEDS-DESIGN.md

- ~~2.10 **Heartbeat action execution**~~ — DONE (probe, deferred, social need LLM calls wired in autonomy/manager.rs)

- ~~2.11 **Cache keepalive ping execution**~~ — DONE (max_tokens=1 ping, cache_read_tokens feedback, cache warning push)


## Priority 3: CLI Features

### Messaging
- ~~5.2 Send with image attachment (-i flag)~~ — DONE (`shore send -i <path>`, multi-image support)
- ~~5.7 Log follow mode (-f/--follow)~~ — DONE
- ~~5.8 Log format options (--json/--content)~~ — DONE

### Conversation Management
- ~~5.13 Search conversations (full-text)~~ — moved to V2-NEEDS-DESIGN.md
- ~~5.14 Conversation info~~ — REMOVED (redundant with `shore status`)

### Message CRUD
- ~~5.18 Get message by index~~ — DONE (`shore get <ref>`)
- ~~5.19 Insert message at position~~ — REMOVED (never used)
- ~~5.20 Detach attachment~~ — REMOVED (never used)

### Character Management
- ~~5.24 Create character (scaffold directory)~~ — DONE (`shore character --new <name>`)

### Model Management
- ~~5.28 Reset to default~~ — DONE (`shore model --reset`)

### Memory CLI
- ~~5.32 Memory reindex~~ — DONE (`shore memory --reindex` rebuilds FTS + vector indexes)
- ~~5.33 Memory import (files → entries)~~ — REMOVED (write a standalone script instead)
- ~~5.34 Memory ask (one-shot agent)~~ — DONE (`shore memory "query"` runs one-shot agent)
- ~~5.36 Memory changelog~~ — DONE (`shore memory-changelog`)

### Configuration
- ~~5.38 Config show (all sections)~~ — DONE (`shore config` returns full config)
- ~~5.39 Config check (validation)~~ — DONE (`shore config --check`)
- ~~5.40 Config reset (clear overrides)~~ — DONE (`shore config --reset` reloads from disk)
- ~~5.41 **Config set (runtime)**~~ — DONE (`shore config <key> <value>` with focused whitelist: defaults.model, defaults.stream, autonomy.enabled, cache_keepalive.enabled)


## Priority 4: Memory & Autonomy Extras

- ~~2.8 **Autonomy pause/resume**~~ — REMOVED (subsumed by `shore config autonomy.enabled`, 5.41)

- ~~3.5 **Consolidation** (write-time dedup via LLM)~~ — DONE (handled by collation merge phase + agent create/supersede flow)


## Priority 5: Rendering & UX

- ~~7.2 Inline terminal images, Kitty/Ghostty (APC protocol)~~ — DONE (images.rs)
- ~~7.3 Inline terminal images, iTerm2 (OSC 1337)~~ — DONE (images.rs)
- ~~7.4 $SHORE_IMAGES override~~ — DONE (images.rs)
- ~~7.5 Rich markdown rendering~~ — DONE (custom parser in shore-tui/src/markdown.rs: bold, italic, code, headings, blockquotes — not full CommonMark but sufficient for chat)
- 7.6 Verbose spinner (token counts, cache hits, timing) — MISSING

### CLI Output Formatting

- ~~7.12 **NO_COLOR / `--no-color` support**~~ — DONE
- ~~7.13 **Phase indicator before first token**~~ — DONE
- ~~7.14 **Tool result truncation**~~ — DONE (500 char limit)
- ~~7.15 **Stream metadata abbreviation**~~ — DONE (strips date suffix)


## Priority 6: Resilience & Observability

- ~~8.5 **shore-llm lifecycle robustness**~~ — DONE (startup socket check with warning, actionable error messages by error kind)

- 8.1 In-memory ring buffers (API calls, tools, errors) — MISSING
- ~~8.2 API payload logging (api_payloads.jsonl)~~ — DONE (`advanced.api_payload_logging` config, redacts API keys)
- ~~8.3 Cache debug guards~~ — DONE (5-layer guard in stream.rs `check_cache_invalidation()`, CacheWarning push, 5 tests)
- ~~8.4 Status sections (filtered view)~~ — DONE (`shore status --section <name>`)


## Priority 7: Other CLI

- ~~5.44 Push notifications (shore notify)~~ — moved to V2-NEEDS-DESIGN.md
- ~~5.45 Failed message list~~ — moved to V2-NEEDS-DESIGN.md
- ~~5.46 Failed message retry~~ — moved to V2-NEEDS-DESIGN.md
- ~~5.47 Failed message clear~~ — moved to V2-NEEDS-DESIGN.md
- ~~5.48 Cache suppress~~ — REMOVED (subsumed by `shore config set`, 5.41)
- ~~5.49 Cache unsuppress~~ — REMOVED (subsumed by `shore config set`, 5.41)
- ~~5.50 Images list (CLI-level browsing)~~ — REMOVED (superseded by in-context image tools)
- ~~5.51 Images import~~ — REMOVED (superseded by in-context image tools)
- ~~5.52 Images describe (vision model)~~ — REMOVED (superseded by in-context image tools)

### Verification
- ~~**In-context image description**~~ — DONE (handler.rs builds Anthropic content arrays with base64-encoded images, media type detection by extension)
