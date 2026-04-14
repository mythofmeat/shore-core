# Interiority Wrap-Up on Iteration Cap

## Problem

Interiority ticks use a hardcoded `min(max_iterations, 6)` tool loop cap. The
character consistently hits this cap with `finish_reason=tool_use`, meaning it
always wants more tools. The loop terminates without the model ever producing a
final text response containing `<recap>` or `<sendMessage>` tags. Result: zero
recaps written since April 8.

## Approach

Don't change the interiority prompt. Don't strip tools. Instead: make the cap
configurable, and when the model hits it, inject a wrap-up system message asking
for a recap.

## Changes

### 1. Use `InteriorityConfig.max_tool_rounds` in `execute_unified_tick`

- `shore-daemon/src/autonomy/manager.rs:1347`: Replace
  `std::cmp::min(lc.app.behavior.tool_use.max_iterations, 6)` with
  `lc.app.behavior.autonomy.interiority.max_tool_rounds`
- Config field already exists on `InteriorityConfig` with default 12

### 2. Wrap-up LLM call when iteration cap is hit

After the tool loop, check: did we exit with `finish_reason == "tool_use"` on
the last iteration AND `recap_text.is_none()`? If so:

- Append the pending tool results to `request.messages` as normal (assistant
  message with tool calls + user message with tool results)
- Append a system message (transformed to user role for Anthropic): "You've used
  all your tool rounds for this private moment. Please write a <recap> of what
  you were doing and thinking so you can pick it up next time — this is
  required. You can also send a message to the user with <sendMessage> if you
  have something to share."
- Make one more `generate()` call with `CallType::ToolLoop`
- Extract `<recap>` and `<sendMessage>` from the response as normal
- The wrap-up call keeps the same prefix (system + tools unchanged) — no cache
  invalidation

### 3. Log the wrap-up

- Log when the wrap-up call fires (`"Interiority: iteration cap hit, requesting wrap-up recap"`)
- Log whether the wrap-up produced a recap
- Log as a warning if wrap-up also failed to produce a recap (`"Interiority: wrap-up call produced no recap"`)

### 4. Update example config

- Add `[behavior.autonomy.interiority]` section to `examples/config.toml` with
  `max_tool_rounds` documented

## Files touched

- `shore-daemon/src/autonomy/manager.rs` (steps 1-3)
- `examples/config.toml` (step 4)

## What we're NOT doing

- Not changing the interiority prompt
- Not stripping tools from the wrap-up call (preserves cache prefix)
- Not adding continuation state between ticks
- Not changing the RecapStore or interiority clock
