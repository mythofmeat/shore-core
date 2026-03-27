# Shore V2 — Needs Design Input

Features that can't be implemented without design decisions from the user.
These were split out of V2-TODO.md because they're "fiddly" — each one
requires human judgment about how it should work, not just coding.


## CLI Output Formatting

- 7.10 **Human-readable command output** — IN PROGRESS
  Replacing raw JSON output with formatted, colored display for each command.
  Done: `log` (chat transcript with colored headers, timestamps, image badges),
  `status` (dashboard with autonomy state, social need bar, cache info).
  Remaining: `get`, `model`, `character --info`, `memory`, `memory-changelog`,
  `config`, `edit`/`delete` confirmations, `compact`/`collate` results.
  **Design pattern:** section headers (`── Title ───`), dim labels with bright values,
  character-colored names via deterministic hash, conditional sections that hide
  when data is absent.

- 7.11 **`--json` output mode flag** — IN PROGRESS
  Once human-readable formatting is the default, add `--json` flag for scripts.
  Done: `log` (--json flag).
  Remaining: other commands as 7.10 progresses.


## Memory Agent

- 3.12 **Memory agent — interactive REPL** (5.35) — STUB
  **Needs decision:** Is this a chat with the memory agent, or a structured command
  interface? How does it differ from `shore send` with memory tools? What commands?


## Tool Use

- 4.6 **web_search** (Tavily API + synthesis) — STUB
  Returns NotImplemented. Needs Tavily integration in daemon.
  **Needs decision:** How many results to fetch, how to present synthesis,
  cost/budget controls for Tavily API calls.

- 4.8 **research_web** (multi-step deep research) — STUB
  Returns NotImplemented. Depends on 4.6.
  **Needs decision:** How many search rounds, how to orchestrate multi-step
  research, when to stop, output format.


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


## Push Notifications

- 5.44 **Push notifications** (shore notify) — MISSING
  **Needs decision:** What notification backend? (Desktop notifications, ntfy, webhook?)
  What events trigger notifications? (Autonomous messages, errors, compaction complete?)
  Does the daemon push, or does the CLI poll?


## Config Schema Gaps

Config fields that exist in V1 but have no V2 schema support yet.
Each one needs a design call about whether/how to port it.

- 10.1 **defaults.cli_target_character** — MISSING
  Default character to load on startup.
- 10.2 **defaults.display_name** — MISSING
  User's display name in conversations.
- 10.5 **connections.matrix_embedded** — MISSING
  Embedded Synapse config (server_name, admin credentials).
- 10.12 **debug.anthropic_cache** (log_expected_misses, preflight_check, exit_on_unexpected_miss) — MISSING
  Cache debug instrumentation flags.
