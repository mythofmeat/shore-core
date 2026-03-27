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


## Message Storage

- 2.9 **Persist tool calls and reasoning in messages** — MISSING
  Tool calls (name, input, output, is_error) and thinking/reasoning content are
  streamed in real-time but discarded after generation. They should be persisted
  alongside Message so that `shore log` can display them and for debugging/audit.
  **Needs decision:** Message struct expansion, storage format (inline vs sidecar),
  migration strategy for existing conversations.


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
- 10.3 **Per-tool toggles** (send_image, roll_dice, image_generation, web_search) — MISSING
  V1 had per-tool enable/disable under [behavior.tool_use].
- 10.4 **connections.tcp** (enabled, addr, allowed_hosts) — MISSING
  V1 had TCP access control. V2 has daemon.tcp_addr but no ACL.
- 10.5 **connections.matrix_embedded** — MISSING
  Embedded Synapse config (server_name, admin credentials).
- 10.6 **memory.image.enabled** — MISSING
  Toggle for image memory subsystem.
- 10.7 **Autonomy sub-toggles** (heartbeat.enabled, compaction.enabled, collation.enabled) — MISSING
  V1 had per-subsystem enabled flags. V2 only has the top-level autonomy.enabled.
- 10.8 **compaction.message_trigger / min_new_messages** — MISSING
  V1 had message-count-based compaction triggers. V2 only has idle_trigger_minutes.
- 10.9 **advanced.editor** — MISSING
  Config-level editor preference. V2 reads $VISUAL/$EDITOR env vars only.
- 10.10 **advanced.data_dir** — MISSING
  Config-level data directory override. V2 uses XDG only.
- 10.11 **advanced.max_retries / retry_backoff_seconds** — MISSING
  Config-level retry tuning. V2 has hardcoded retry logic in LLM client.
- 10.12 **debug.anthropic_cache** (log_expected_misses, preflight_check, exit_on_unexpected_miss) — MISSING
  Cache debug instrumentation flags.
