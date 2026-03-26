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

- 7.11 **`--json` output mode flag** — MISSING
  Once human-readable formatting is the default, add `--json` flag for scripts.
  (Blocked on 7.10.)


## Conversation Management

- 5.12 **Fork conversation** (fork last N messages) — MISSING
  **Needs decision:** What gets forked — just messages, or also memory/compaction state?
  New character instance or shared? New conversation file? What about attachments?


## Memory Agent

- 3.12 **Memory agent — interactive REPL** (5.35) — STUB
  **Needs decision:** Is this a chat with the memory agent, or a structured command
  interface? How does it differ from `shore send` with memory tools? What commands?


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
