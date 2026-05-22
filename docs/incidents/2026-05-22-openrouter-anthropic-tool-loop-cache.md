# Post-mortem: OpenRouter Anthropic tool-loop cache invalidation

Status: fixed in Shore code on 2026-05-22. Use a release that includes this
fix before relying on the settings guide below.

Audience: Shore users running Anthropic models with prompt caching, especially
Anthropic models reached through OpenRouter with tools enabled.

## What happened

Anthropic prompt caching works by reusing a stable prefix of a request. In
plain terms, if Shore sends the same long beginning of a conversation again,
the provider can read that beginning from cache instead of charging for a new
cache write. A tool loop should keep that property:

1. The user asks for work that needs a tool.
2. The assistant replies with a tool call.
3. Shore runs the tool and appends the tool result.
4. The next model call should reuse the warm prefix and extend it with the
   small new tool-loop tail.

The broken behavior was step 4. During some Anthropic tool loops, Shore could
turn a warm conversation prefix into a fresh cache write instead of a cache
read. That meant users paid cache-write prices again for prompt material that
should already have been warm.

The bug was most visible for Anthropic models through OpenRouter, especially
with adaptive reasoning enabled, but one underlying cache-breakpoint mistake
was in Shore's Anthropic tool-loop handling itself.

## Impact

Affected calls were tool-loop continuations on Anthropic prompt-cached
conversations. The symptoms were:

- cache reads unexpectedly dropping during or immediately after tool use
- cache writes jumping for an already warm conversation prefix
- higher provider spend for long-running chats that use tools often
- cache anomaly warnings or forensic rows when the ledger caught the rewrite

This did not mean every Anthropic request missed cache for six months. Ordinary
cache expiry, model changes, provider changes, prompt-visible edits, tool
definition changes, and reasoning-mode shape changes can legitimately produce
cold cache writes. The failure here was that ordinary tool-loop bookkeeping
could also cause a rewrite when it should have been cache-stable.

## Root cause

There were three defects in the failing path.

### 1. Shore used the wrong tool-loop cache boundaries

The Anthropic cache breakpoint logic treated the conversation too much like a
sequence of completed user/assistant turns. A tool loop is different: the
active user prompt starts the loop, then completed `tool_result` user messages
extend it.

Shore did not consistently keep the active user boundary warm and advance the
cache boundary onto the completed `tool_result` boundary. The first tool-loop
continuation could therefore rewrite a prefix that had just been warmed.

### 2. One OpenRouter path replayed tool history in the wrong shape

Shore stores tool history internally as content blocks. For generic
OpenAI-compatible chat completions, that history is commonly projected as
assistant function calls plus separate tool messages.

For an Anthropic model reached through OpenRouter, that generic projection was
not cache-stable enough for this path. The fixed code keeps Anthropic-shaped
tool history across the OpenRouter continuation:

- assistant `tool_use` content blocks
- user `tool_result` content blocks

That keeps the growing Anthropic conversation shape consistent through the
tool loop.

### 3. Adaptive OpenRouter reasoning replay was incomplete

OpenRouter chat completions can return replayable `reasoning_details` metadata
for the next continuation. Shore needed to preserve that metadata with the
assistant tool-use phase and send it back after the tool result.

The investigation also reproduced a separate transport problem with adaptive
Anthropic requests sent through OpenRouter's native Anthropic Messages route:
the tool-use phase can arrive as a bare `tool_use` response with no replayable
continuation metadata for Shore to carry forward. The Shore fix does not claim
that upstream behavior changed. For adaptive Anthropic OpenRouter requests
configured with `sdk = "anthropic"`, Shore now routes the runtime request
through the cache-stable OpenRouter chat-completions path while keeping the
Anthropic config surface.

## Why this lasted

The tests were missing the expensive case that mattered:

- a warm prompt cache
- an actual Shore tool loop
- Anthropic through OpenRouter
- adaptive reasoning replay
- verification on the first tool-result continuation, not only on ordinary
  warm chat turns

The code also had a wrong local assumption: that the active final user message
should not itself be a cache boundary. That assumption did not match the
growing-conversation shape Shore needs for Anthropic tool loops.

Finally, `sdk` describes a client-side wire protocol choice, but OpenRouter is
still a routing layer with multiple provider-facing shapes. That distinction
was not tested rigorously enough. Users paid for that gap.

## Fix

The fix has four parts:

1. Anthropic cache breakpoint placement now tracks recent user-side
   boundaries, including active user prompts and completed tool results.
2. Anthropic models routed through OpenRouter chat completions keep
   Anthropic-shaped `tool_use` and `tool_result` history for tool-loop
   continuations.
3. OpenRouter `reasoning_details` are preserved and replayed when they are the
   continuation metadata for an adaptive reasoning turn.
4. Adaptive Anthropic OpenRouter requests configured with
   `sdk = "anthropic"` use the same cache-stable runtime transport instead of
   relying on the native OpenRouter Messages tool-use continuation.

The fix also adds regression coverage around the request shape and live cache
tests for both supported OpenRouter config surfaces:

- `scripts/cache-tests/24-tool-loop-daemon.sh` covers adaptive Anthropic
  OpenRouter use from an `sdk = "anthropic"` config.
- `scripts/cache-tests/28-tool-loop-openai-daemon.sh` covers the recommended
  OpenRouter chat-completions config.

Those live tests require real provider credentials and assert that the
tool-loop continuation reads a warm cache prefix instead of rewriting it.

## What users should do now

1. Upgrade to a Shore build that includes the 2026-05-22 cache fix.
2. Restart the daemon after upgrading.
3. Use `reasoning_effort` for current Anthropic Sonnet and Opus models. Do not
   use `budget_tokens` for the settings in this guide.
4. Set a non-empty `cache_ttl` on Anthropic models reached through OpenRouter.
5. When predictable OpenRouter cache behavior matters, pin the upstream
   provider to Anthropic instead of allowing a provider fallback to change the
   route underneath the conversation.

Changing model, provider route, cache TTL, reasoning mode, prompt-visible
files, or tool definitions can still produce an expected cold cache write.
This guide is about avoiding the tool-loop regression, not about preventing
every valid cache invalidation.

## Correct settings

### Recommended: Anthropic through OpenRouter

For a new OpenRouter Anthropic model entry, use OpenRouter's normal provider
shape and an Anthropic model id:

```toml
[defaults]
model = "sonnet-openrouter"

[providers.openrouter]
sdk = "openai"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"

[chat.openrouter.sonnet-openrouter]
model_id = "anthropic/claude-sonnet-4-6"
cache_ttl = "1h"
reasoning_effort = "high"
openrouter_provider = { order = ["Anthropic"], allow_fallbacks = false }
```

Use `high`, `medium`, or `low` here when you want a different effort level
than the example. The copied OpenRouter setup should stay effort-based; do not
replace it with a `budget_tokens` workaround.

The important pieces are:

| Setting | Why it matters |
| --- | --- |
| `model_id = "anthropic/..."` | Selects an Anthropic model through OpenRouter. |
| `cache_ttl = "1h"` | Enables the cache markers Shore needs on this OpenRouter model entry. |
| `reasoning_effort` | Uses the current Anthropic effort-style reasoning setting. |
| `sdk = "openai"` on the OpenRouter provider | Uses OpenRouter chat completions, which Shore keeps cache-stable for Anthropic tool history. |
| `openrouter_provider` pin | Keeps OpenRouter on Anthropic when predictable cache behavior matters. |

### Supported after the fix: OpenRouter with `sdk = "anthropic"`

Users who already configure OpenRouter with Anthropic SDK syntax do not need to
rewrite their config only to escape this bug after upgrading:

```toml
[defaults]
model = "sonnet-openrouter-messages"

[chat.openrouter.sonnet-openrouter-messages]
sdk = "anthropic"
model_id = "anthropic/claude-sonnet-4-6"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
cache_ttl = "1h"
reasoning_effort = "high"
openrouter_provider = { order = ["Anthropic"], allow_fallbacks = false }
```

On the fixed Shore runtime, adaptive OpenRouter Anthropic requests from this
config are routed through the cache-stable continuation path automatically.
The `sdk = "anthropic"` config remains accepted; it is not a user error.

Do not rely on this config on an older Shore build that does not include the
cache fix.

### Direct Anthropic

If you call Anthropic directly instead of using OpenRouter, the normal direct
Anthropic model entry is still the clearest setup:

```toml
[defaults]
model = "sonnet-direct"

[chat.anthropic.sonnet-direct]
model_id = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
cache_ttl = "1h"
reasoning_effort = "high"
```

### Avoid these settings for this case

| Avoid | Use instead | Reason |
| --- | --- | --- |
| `budget_tokens` for current Anthropic Sonnet or Opus settings | `reasoning_effort` | The current supported guide is effort-based. |
| `cache_ttl = ""` | `cache_ttl = "1h"` or another non-empty supported TTL | Empty TTL disables Shore cache markers for that entry. |
| Unpinned OpenRouter fallback when comparing cache cost behavior | `openrouter_provider` pinned to Anthropic | A changed upstream route is a real provider change. |
| Testing only ordinary warm chat turns | Exercise tool use too | This incident lived in the tool-result continuation. |

## How to tell whether the fix is working

At a high level, a warm tool-using conversation should look like this:

1. The first request creates cache.
2. A later warm request reads cache.
3. A message that starts a tool loop may extend the cache boundary.
4. The following `tool_loop` continuation should keep a cache read instead of
   dropping to zero and rewriting the warm prefix.

Operators who enable cache forensics can inspect cache read and cache write
counts in the Shore ledger and forensic output. A zero cache read plus a large
rewrite on the first tool-result continuation of an otherwise warm
conversation is the failure shape this incident fixes.

## Verification and prevention

The 2026-05-22 fix was verified with:

- deterministic provider and daemon tests for tool-history shape, reasoning
  metadata replay, and cache breakpoint placement
- live OpenRouter daemon tool-loop tests for both `sdk = "openai"` and
  `sdk = "anthropic"` config surfaces
- the normal repo checks: harness check, formatting, workspace tests, clippy,
  and release builds

Going forward, cache changes must keep live provider checks in scope whenever
request formatting, tool use, adaptive reasoning replay, or cache economics
change. A cache test that never crosses a tool-result continuation is not
enough coverage for this path.

## Known limits

This report does not claim that OpenRouter's native adaptive Anthropic
Messages continuation behavior changed upstream. Shore's fix is to keep its
supported OpenRouter Anthropic tool-loop path cache-stable in spite of the
behavior observed during this investigation.
