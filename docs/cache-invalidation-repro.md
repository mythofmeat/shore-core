# Cache Invalidation After Tool Use — Reproduction Steps

## Environment

- **Provider:** Anthropic (`claude-sonnet-4-6`) via OpenRouter → Google Vertex
- **Config:** `cache_ttl = "1h"`, `reasoning_effort = "adaptive"`, `cache_control_depth = 2`
- **Character:** `test` with randomized UUID in system prompt (prevents OpenRouter cross-session cache reuse from masking real invalidation)
- **Body dumps:** `build_body()` writes every request to `/tmp/shore_body_XXXX.json` (sequential)

## Setup

1. Randomize the test character's system prompt UUID:
   ```bash
   sed -i "s/Session ID: .*/Session ID: $(uuidgen)/" \
     ~/.config/shore/characters/test/character.md
   ```

2. Build and run the daemon:
   ```bash
   cargo build --workspace --release
   ./target/release/shore-daemon
   ```

3. Clean old body dumps:
   ```bash
   rm -f /tmp/shore_body_*.json
   ```

4. Connect via CLI or TUI to the `test` character using the `sonnet` model.

## Control Test (no tool use) — PASSES

Send 5 messages with **7-second delays** between each (to account for ~5s cache propagation delay observed on Vertex):

```
Message 1: "Tell me a fun fact about octopuses"
  → wait 7s
Message 2: "Tell me another fun fact about a different animal"
  → wait 7s
Message 3: "Now tell me about deep sea creatures"
  → wait 7s  
Message 4: "What about bioluminescence?"
  → wait 7s
Message 5: "Summarize what we've discussed"
```

**Expected result:** Messages 3+ show `cache_read > 0` in the stream logs. Message 5 confirmed: `cache_r:2548, cache_w:16`.

**This proves caching works for normal multi-turn conversation.**

## Tool Use Test — FAILS

Continue from the control test (or start fresh with the same setup):

```
Message 6: "What time is it right now?"
```

This triggers the `check_time` tool. The tool loop produces two API calls:
- **Body 0006:** 13 messages (messages 0-10 from before + assistant tool_use + user tool_result). This is the tool loop continuation call.
- The initial call that returns `finish_reason: "tool_use"` is Body 0005.

After tool use completes, wait 7 seconds, then:

```
Message 7: "Thanks for the time. What were we talking about before?"
```

**Observed result:**
- Body 0006 (tool loop continuation): `cache_w:2580` (expected — new content)
- Body 0007 (follow-up message): `cache_r:0, cache_w:2596` — **COMPLETE CACHE MISS**

The entire prefix is re-cached from scratch despite being byte-identical to the previous request's prefix (verified by comparing body dump JSON with cache_control stripped).

## What Has Been Verified

1. **Prefix content is identical.** Bodies 0006 and 0007 share messages 0-12 byte-for-byte (after stripping cache_control annotations). System prompts are identical. Tool definitions are identical.

2. **cache_control is NOT part of the cache key.** Proven via direct curl tests: removing a cache_control annotation from an earlier breakpoint does not invalidate a later breakpoint's cache.

3. **No thinking blocks are present.** Adaptive thinking chose not to think for these simple questions. All 8 body dumps contain only `text` and `tool_use`/`tool_result` content blocks — zero `thinking` or `redacted_thinking` blocks.

4. **The `has_existing_markers` path works correctly.** On tool loop continuations (Body 0006), `build_body()` detects existing cache_control markers and skips re-placement, preserving the exact positions from Body 0005.

5. **Propagation delay is not the issue.** 7 seconds between messages is well above the observed ~5s propagation window. The control test proves this delay is sufficient.

## What Has NOT Been Tested

1. **Interleaved thinking during tool use.** Adaptive thinking never triggered for the test questions used. The user's primary hypothesis is that thinking blocks during tool use (which are cached with the tool_result message, then stripped by the API when a non-tool-result user message follows) cause invalidation. Need to force thinking — either use harder questions, opus (which thinks more aggressively), or test whether the presence of thinking blocks in the tool loop call vs. their absence in the follow-up call is the issue.

2. **Whether the tool loop's intermediate API call itself is the cause.** The tool loop makes a call (Body 0006) within ~1 second of Body 0005. This rapid succession might create a conflicting cache entry that evicts or interferes with the one from Body 0005. The follow-up call (Body 0007) then misses because the cache state is corrupted.

3. **Whether OpenRouter's cache routing differs between tool_use and non-tool_use requests.** We've ruled out blaming providers in general, but we haven't tested whether the exact same request sent directly to Anthropic (bypassing OpenRouter) also exhibits this behavior.

## Key Code Paths

- **Request body construction:** `shore-llm-client/src/providers/anthropic.rs` → `build_body()` (line 278)
- **Cache breakpoint placement:** `apply_cache_control()` (line 190) — strips existing CC, normalizes strings→arrays, places single breakpoint at depth 2
- **Tool loop:** `shore-daemon/src/engine/tools.rs` → `run_tool_loop()` (line 74) — appends assistant tool_use + user tool_result messages, calls LLM again
- **Cache invalidation detection:** `shore-llm-client/src/stream.rs` → `check_cache_invalidation()` (line 265)
- **Existing markers check:** `messages_have_cache_control()` — if true, `build_body()` skips `apply_cache_control()` and clones messages as-is

## Open Questions

1. Why does a byte-identical prefix miss the cache after a tool use turn but hit after a normal turn?
2. Is Anthropic's cache keying affected by something outside the serialized body (e.g., whether the previous request in the session used tools)?
3. Does the rapid succession of API calls during tool loops (Bodies 0005 → 0006 within ~1s) cause cache eviction or corruption?
4. Would sending thinking blocks in the tool loop call but not in the follow-up cause a prefix mismatch from the API's perspective (even though they're "stripped internally")?
