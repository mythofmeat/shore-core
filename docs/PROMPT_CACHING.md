# Anthropic Prompt Caching: Findings & Configuration

Empirical results from testing prompt caching behavior through OpenRouter
against Anthropic Claude models (April 2026).

## TL;DR

**Sliding message breakpoints alone are unreliable.** You must include at
least one pinned system-prompt breakpoint for cache stability. Without it,
the API intermittently performs full prefix rewrites despite byte-identical
content.

Recommended config:

```toml
cache_depth_turns      = [1, 2]
cache_pinned_position  = [0]   # or [-1] if you have a recap block
cache_ttl              = "1h"
```

## Background

Anthropic's prompt caching is prefix-based. You annotate content blocks with
`cache_control: {type: "ephemeral"}` and the API caches everything from the
start of the request up to that annotation. Up to 4 breakpoints per request.
Minimum cacheable prefix: 1024 tokens.

- **Cache write**: 25% surcharge over base input pricing
- **Cache read**: 90% discount over base input pricing
- Break-even: any prefix reused more than once within the TTL window

Shore exposes two configuration axes:

| Field | Type | Meaning |
|---|---|---|
| `cache_depth_turns` | `Vec<u32>` | Sliding breakpoints relative to conversation end. `[2]` = breakpoint before the 2nd-to-last user turn. |
| `cache_pinned_position` | `Vec<i32>` | Static breakpoints in the system prompt. `0` = last system block, `-1` = second-to-last, positive = Nth user turn. |

## Test Results

All tests: 10 sequential messages, unique nonce per run (cold cache),
through OpenRouter to `anthropic/claude-sonnet-4-6`.

| Test | depth | pinned | thinking | Result |
|---|---|---|---|---|
| 01  | `[2]` | — | high | **FAIL** — full rewrite at turn 7-9 |
| 01b | `[0]` | — | high | **PASS** — 10/10 |
| 01c | `[1]` | — | high | **FAIL** — full rewrite at turn 2 |
| 01d | `[1,2]` | — | high | **FAIL** — full rewrite at turn 6 |
| 02  | — | `[0]` | high | not run (rate limited) |
| 03  | `[2]` | `[0]` | high | not run |
| 04  | `[2]` | — | off | not run |
| 05  | `[2]` | `[0]` | off | not run |
| 06  | `[1,2]` | `[0]` | high | **PASS** — 10/10, 32-tok incremental writes |
| 07  | `[1,2]` | `[-1]` | high | **PASS** — 10/10, 32-tok incremental writes (with recap) |

### Key observations

1. **depth=0 always passes.** This places the breakpoint on the last
   assistant message (immediately before the final user message). The
   breakpoint moves every turn, but the prefix up to it is always cached.
   This is the only sliding-only config that works.

2. **depth>0 without a pinned anchor fails intermittently.** depth=1,
   depth=2, and depth=[1,2] all produce full prefix rewrites (cache_r=0,
   cache_w=full) at seemingly random turns despite the content before the
   breakpoint being byte-identical between calls. Body dumps confirmed this.

3. **Adding any pinned system breakpoint fixes it.** Both `pinned=[0]`
   (last system block) and `pinned=[-1]` (second-to-last, useful when a
   recap block exists) stabilize the sliding breakpoints completely.

4. **This is undocumented behavior.** Anthropic's documentation does not
   mention that sliding-only breakpoints on message content are unreliable
   without a system-level anchor. SillyTavern's implementation always
   includes system-level breakpoints (last system block + last tool
   definition), which is why their approach works.

## How breakpoints map to the API payload

Shore's `assemble_prompt` produces system blocks in this order:

| Index | Block | Notes |
|---|---|---|
| 0 | Rendered system.md template | Always present |
| 1 | `<capabilities>` | If tools enabled |
| 2 | `<char>` character definition | If present |
| 3 | `<user>` user definition | If present |
| 4 | `<char_recap>` | If recap.md exists and not private |

`cache_pinned_position` resolves as:
- `0` → last system block (e.g., recap if it exists, otherwise char def)
- `-1` → second-to-last system block
- `-N` → Nth from end of system blocks

`cache_depth_turns` resolves by counting user messages (excluding
tool_result) backward from the end of the conversation:
- `0` → breakpoint on the last assistant message (right before final user msg)
- `1` → one user turn further back
- `2` → two user turns further back

The `cache_control` annotation is placed on the **last content block** of
the target message or system block.

## SillyTavern's approach (for reference)

SillyTavern (`src/prompt-converters.js`) places breakpoints at:
1. Last system block (system prompt anchor)
2. Last tool definition
3. Message at `cachingAtDepth` role switches from the end
4. Message at `cachingAtDepth + 2` role switches from the end

Their "role switches" count both user and assistant messages; Shore's
`cache_depth_turns` counts only user messages. So SillyTavern's depth D
and D+2 roughly maps to Shore's depth D/2 and D/2+1, which is why
`cache_depth_turns = [1, 2]` is the equivalent configuration.

## Test harness

Self-contained test scripts live in `scripts/cache-tests/`. Each test:

- Creates a fresh temp directory with isolated config, data, and socket
- Generates a unique 32-char nonce to guarantee a cold cache
- Runs 10 sequential messages
- Fails immediately if any cache write exceeds 50% of the cold-start write
- Preserves the temp dir on failure for debugging
- Cleans up on success

Run individual tests: `bash scripts/cache-tests/07-pinned-neg1-with-recap.sh`
Run all: `bash scripts/cache-tests/run-all.sh`
