# Shore V2 — Needs Design Input

Features that can't be implemented without design decisions from the user.
These were split out of V2-TODO.md because they're "fiddly" — each one
requires human judgment about how it should work, not just coding.


## Memory Agent

- 3.12 **Memory agent — interactive REPL** (5.35) — DONE
  Natural language chat loop via `shore memory shell`. Stateful daemon sessions,
  ephemeral history, auto-accept writes (no confirmation flow).


## Embedded Matrix Server

- 10.5 **Embedded Synapse provisioning & config** — DONE
  Lives under `[connections.matrix.embedded]`. shore-matrix manages the full
  Synapse lifecycle: config generation, subprocess, admin provisioning,
  character account registration, room creation with trusted_user invitation.
  CLI: `shore matrix setup` (one-shot provisioning), `shore matrix register`
  (user account creation). Embedded state persisted at
  `$XDG_DATA_HOME/shore/synapse/embedded_state.json`.
