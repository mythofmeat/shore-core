# Prompt Caching Notes

Prompt-cache preservation is a load-bearing Shore concern. Unexpected Anthropic cache invalidation wastes real money and should be treated as a serious regression.

## Current Cache-Stability Model

Protected prompt content is split into two layers:

- editable workspace files under `characters/<Character>/workspace/`
- prompt-active snapshot files under `$XDG_DATA_HOME/shore/<Character>/active_prompt/`

Normal request assembly reads from `active_prompt/`.

Protected files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

`HEARTBEAT.md` is heartbeat-only. Normal chat does not inject it.

## Activation Boundary

Workspace edits to protected files queue `deferred_edits.jsonl`. They become active only when compaction/reload refreshes `active_prompt/`.

This makes prompt prefix changes visible and attributable:

- compaction happened
- a protected prompt file was activated
- the cache boundary was expected

## Prompt Inputs

Normal chat prompt inputs:

- active `SOUL.md`
- active `USER.md`
- active `AGENTS.md`
- active `TOOLS.md`
- active snapshot `active_prompt/MEMORY.md`, refreshed from `workspace/MEMORY.md` at compaction/reload
- conversation messages
- stable capability/tool guidance

Heartbeat prompt inputs:

- same active protected files
- active `HEARTBEAT.md`
- heartbeat runtime affordances (`HEARTBEAT_OK`, `set_next_wake`, `<sendMessage>`)

## Thinking Blocks

Prior completed-turn thinking is preserved by default through `[memory.thinking].preserve_prior_turns = true` (set to `false` to strip and save tokens — safe for Anthropic Claude 4.x but ignored for DeepSeek/Moonshot thinking-mode, which require prior `reasoning_content`). In-progress tool loops always preserve thinking blocks.

## Things That Should Not Bust Cache

- writing ordinary markdown memory files
- appending compaction/dreaming memory notes outside `MEMORY.md`
- ordinary workspace edits outside protected prompt files
- tool loop bookkeeping
- activity tracking
- image cache warmups

## Things That May Bust Cache

- compaction/reload
- activating staged protected edits
- activating a rewritten `workspace/MEMORY.md` snapshot
- editing old conversation messages
- changing active model/provider/cache settings
- changing tool definitions or prompt templates in code

## Verification

Useful checks:

```sh
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon engine::prompt
cargo test --workspace
```

For real cache economics, use a live Anthropic/OpenRouter-Anthropic model and inspect the ledger/cache tracker.
