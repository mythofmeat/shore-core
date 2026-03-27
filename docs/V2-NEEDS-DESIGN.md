# Shore V2 — Needs Design Input

Features that can't be implemented without design decisions from the user.
These were split out of V2-TODO.md because they're "fiddly" — each one
requires human judgment about how it should work, not just coding.


## Memory Agent

- 3.12 **Memory agent — interactive REPL** (5.35) — DONE
  Natural language chat loop via `shore memory shell`. Stateful daemon sessions,
  ephemeral history, auto-accept writes (no confirmation flow).


## Provider Payload Projection

- 11.1 **Provider-specific payload projection** — MISSING
  Each LLM provider has different rules for content blocks in conversation history:
  Anthropic requires interleaved thinking with signatures on the last turn only;
  OpenAI-compatible APIs have no thinking blocks; DeepSeek has its own reasoning
  format. Currently thinking blocks are stripped from all payloads.
  **Needs decision:** Where thinking signatures come from (shore-llm? stored in
  ContentBlock?), handling of provider switches mid-conversation, per-Sdk projection
  logic in the daemon.


## Embedded Matrix Server

- 10.5 **Embedded Synapse provisioning & config** — DONE
  Lives under `[connections.matrix.embedded]`. shore-matrix manages the full
  Synapse lifecycle: config generation, subprocess, admin provisioning,
  character account registration, room creation with trusted_user invitation.
  CLI: `shore matrix setup` (one-shot provisioning), `shore matrix register`
  (user account creation). Embedded state persisted at
  `$XDG_DATA_HOME/shore/synapse/embedded_state.json`.
