# Shore V2 — Needs Design Input

Features that can't be implemented without design decisions from the user.
These were split out of V2-TODO.md because they're "fiddly" — each one
requires human judgment about how it should work, not just coding.


## Memory Agent

- 3.12 **Memory agent — interactive REPL** (5.35) — STUB
  **Needs decision:** Is this a chat with the memory agent, or a structured command
  interface? How does it differ from `shore send` with memory tools? What commands?


## Provider Payload Projection

- 11.1 **Provider-specific payload projection** — MISSING
  Each LLM provider has different rules for content blocks in conversation history:
  Anthropic requires interleaved thinking with signatures on the last turn only;
  OpenAI-compatible APIs have no thinking blocks; DeepSeek has its own reasoning
  format. Currently thinking blocks are stripped from all payloads.
  **Needs decision:** Where thinking signatures come from (shore-llm? stored in
  ContentBlock?), handling of provider switches mid-conversation, per-Sdk projection
  logic in the daemon.


## Conversation Management (cont.)

- 5.13 **Search conversations** (full-text) — MISSING
  **Needs decision:** Search across characters or within current? What gets returned
  (message snippets, conversation IDs)? Output format? Does this use existing FTS5
  or a separate index?


## Embedded Matrix Server

- 10.5 **Embedded Synapse provisioning & config** — MISSING
  Full embedded Synapse setup: server_name, admin credentials, port binding,
  federation toggles. This is a standalone feature workstream, not a config gap.
  **Needs decision:** Scope of daemon-managed Synapse lifecycle, config surface,
  whether it lives under `[connections]` or gets its own top-level section.
