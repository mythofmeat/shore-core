# Shore V2 — Quirks & Gotchas

Unexpected behavior, kludges, and idiosyncrasies that aren't obvious from reading the code. If you assumed something would work one way and it didn't, document it here.

Scope guardrails:

- Put external/provider/platform oddities here.
- Do not use this file for protocol debt, architecture mismatches, or planned
  cleanup work.
- If the issue changes Shore's intended wire contract or state ownership, put
  it in `docs/ARCHITECTURE.md` or a focused design document instead.

## Provider Integrations

- **OpenRouter defaults to `Sdk::Openai`** but can be overridden to `Sdk::Anthropic` per model (e.g. `sdk = "anthropic"` for Claude models). The `base_url` in hardcoded defaults routes requests to OpenRouter's API. If the base_url is missing or wrong, requests go to OpenAI instead — silently.

- **OpenRouter inconsistently forwards thinking signatures.** When proxying Claude's extended thinking, OpenRouter sometimes strips or fails to relay `signature_delta` SSE events. This means thinking blocks stored via OpenRouter may have `signature: null` even when the upstream model produced one. Shore handles this gracefully (signatures are `Option<String>`), but if Anthropic ever strictly requires signatures on cached thinking blocks in subsequent turns, this could break multi-turn thinking continuity through OpenRouter.

- **LLMs emit whitespace-only Text blocks before thinking/tool_use.** Claude (via OpenRouter) sometimes produces a `{"type": "text", "text": "\n\n"}` block before the thinking and tool_use blocks in a tool-loop response. The tool-loop merge must treat whitespace-only Text blocks as non-substantive — otherwise the merge predicate fails and tool results get orphaned.

- **OpenRouter drops ~22-33% of prompt cache hits without provider pinning.** OpenRouter load-balances across multiple Anthropic backends with independent prompt caches — each backend switch is a full miss. This is independent of API format, headers, delay, or breakpoint strategy. **Fix: `openrouter_provider = { order = ["Anthropic"] }` in model config.** Confirmed 0% miss rate across 6 independent runs (0/30 non-cold turns). Pinning to Google/Vertex does NOT help (still 20%). Only Anthropic's direct infrastructure provides stable per-key caching. See `docs/PROMPT_CACHING.md` for the full experiment matrix.

## Anthropic API

- **Thinking blocks don't need client-side stripping.** The Anthropic API strips `thinking` and `redacted_thinking` blocks from prior assistant turns internally. Sending them intact does not affect the cache key — confirmed via live testing with adaptive thinking across multi-turn conversations including tool use. Pre-stripping on the client side is unnecessary and was removed.

- **Interiority replaces heartbeat — config migration is breaking.** The `[behavior.autonomy]` section no longer accepts `personality`, `max_unanswered`, `max_deferral_hours`, or `[behavior.autonomy.heartbeat]`. Due to `deny_unknown_fields`, old config files will fail to parse. The persisted state file (`autonomy.json`) version bumped from 1→2; old heartbeat fields are silently ignored on load. The wire protocol command `heartbeat_log` is kept as-is to avoid breaking existing CLI versions — it returns `InteriorityEvent` data under the old name.

- **Anthropic prompt cache has minimum token thresholds.** Caching silently doesn't activate if the cached prefix is shorter than the model's minimum. Opus 4.6: 4096 tokens, Sonnet 4.6: 2048 tokens, Haiku 4.5: 4096 tokens. The API returns `cache_creation_tokens=0` and `cache_read_tokens=0` with no error. This is easy to miss in test configurations with short prompts — cache verification is meaningless if input tokens don't exceed the threshold.

- **Anthropic prompt cache has ~5s propagation delay.** After a cache write, identical requests within ~2s may miss. Requests after ~5s reliably hit. This is relevant for tool loops where the continuation call fires within ~1s of the initial call — the cache from the initial call won't be available yet, but this is expected and not a bug.

- **Sliding message breakpoints require a pinned system anchor.** Using `cache_depth_turns` alone (without any `cache_pinned_position`) causes intermittent full prefix rewrites — `cache_read_tokens=0` with a full `cache_creation_tokens` despite byte-identical prefix content. This affects all depths except depth=0. Adding any pinned system breakpoint (`[0]` or `[-1]`) completely fixes it. This is undocumented; SillyTavern works because it always includes system-level breakpoints. See `docs/PROMPT_CACHING.md` for full test results.

## Image Handling

- **Anthropic 1h cache TTL pricing differs from OpenRouter's reported 5m prices.** OpenRouter's `/api/v1/models` endpoint reports cache_write prices for the 5-minute TTL. Shore uses the 1-hour TTL (configured via `cache_ttl = "1h"`), where cache_write costs are 2x input price (5-minute price is 1.25x input). The PricingEngine hardcodes this multiplier (`ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER = 1.6`). If Anthropic changes the relationship between TTL tiers, this multiplier needs updating.

## OpenRouter Pricing

- **OpenRouter `/api/v1/models/{id}` returns 404 for everything.** The per-model endpoint is dead (confirmed 2026-04-06). The only working endpoint is `/api/v1/models` which returns the full catalog. `PricingEngine::fetch_pricing` was rewritten to fetch the full catalog, scan for the target model, and bulk-cache all pricing data in one pass.

- **Anthropic model IDs use dots for minor versions on OpenRouter.** Shore stores model names as `claude-opus-4-6` (from Anthropic's API) but OpenRouter's catalog uses `claude-opus-4.6`. The `normalize_anthropic_model()` function converts the last `digit-digit` hyphen to a dot. This is fragile — if Anthropic releases a model with a hyphenated suffix that isn't a version number, it could be incorrectly normalized.

- **Anthropic 1h cache TTL pricing differs from OpenRouter's reported 5m prices.** OpenRouter reports cache_write prices for the 5-minute TTL. Shore uses 1-hour TTL, where cache_write costs are 2x input price (5-minute price is 1.25x input). Hardcoded as `ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER = 1.6`.

## Image Handling

- **Autonomy path lacks image cache warm-up.** The `rebuild_request_from_disk` call in `autonomy/manager.rs` is synchronous, so it cannot call `warm_image_cache()` (which is async). Images sent during autonomous turns still get resized and cached via the synchronous `cached_resize()` fallback inside `build_content`, but the first encounter of each image pays the full resize cost inline rather than pre-warming in parallel. This only matters if the character has images in its conversation history when an autonomous turn fires.

- **Retry may return over-limit images for transparent PNGs.** The resize pipeline retries once with more aggressive parameters if the first attempt exceeds the byte limit. For transparent images (which must stay PNG), if the retried result still exceeds the limit, it is sent anyway with a warn log. This is a deliberate trade-off: sending a slightly-too-large image that might work is better than dropping it entirely. The API may reject it, but the alternative (converting to JPEG and losing transparency) was deemed worse.

- **User messages always have `content_blocks` populated.** This means the `build_content(text, images)` fallback in the LLM message builder is dead code for user messages — it only fires when `content_blocks` is empty. Prior to the fix, `m.images` was silently dropped for all user messages because the `content_blocks` branch didn't encode them. Image encoding must happen in both branches.

## Desktop / Tray (Linux)

- **Tauri's Linux tray can't separate left-click-activate from context-menu.** libayatana-appindicator (what `tauri::tray` links against) has two compounding constraints on Linux:
    1. **No menu → no icon.** A `TrayIconBuilder` without `.menu(&menu)` silently fails to register with the shell on most DEs.
    2. **Menu attached → menu pops on *any* click.** SNI itself exposes `Activate` (left) and `ContextMenu` (right) as distinct DBus calls, but the appindicator bridge Tauri uses collapses them. `show_menu_on_left_click(false)` is documented as "Linux: Unsupported" and is ignored.

    Resolution: on Linux, shore-gui bypasses Tauri's tray entirely and registers its own SNI item via the `ksni` crate, which talks to the StatusNotifierWatcher directly and routes `Activate` to `Tray::activate` and `ContextMenu` to `Tray::menu`. This restores proper left-click-toggle-window + right-click-menu behavior. The Tauri tray path is retained for macOS/Windows where its click routing is clean. Reference impls: `shore-gui/src-tauri/src/tray_linux.rs` (ksni, Linux), `shore-gui/src-tauri/src/lib.rs::build_tray` (Tauri, non-Linux).

- **`libayatana-appindicator is deprecated` runtime warning is cosmetic.** Emitted by the C library when Tauri (via `tray-icon` crate) links against `libayatana-appindicator3` rather than the newer `-glib` variant. The older library still works and the warning has no behavioral impact. No longer hit on Linux now that shore-gui uses ksni directly.

## Streaming / Client Rendering

- **A single assistant turn can emit multiple `StreamEnd` events.** During tool use, the daemon streams `StreamStart → StreamEnd(finish_reason="tool_use") → ToolCall → ToolResult → StreamStart → … → StreamEnd(finish_reason="end_turn")`. Each intermediate `StreamEnd` carries its own `StreamMetadata` covering only that phase. A client that treats every `StreamEnd` as "assistant entry complete" will push a premature empty/partial block, render a duplicate character header on the next phase, and split per-turn stats across rows. Clients must buffer text and metadata across phases and only commit a single conversation entry when `finish_reason != "tool_use"`, summing tokens and `total_ms` while preserving `ttft_ms` from the first phase. Reference impl: `shore-tui/src/main.rs::handle_server_message`.

- **On the final phase, `History` reaches the client *before* `StreamEnd`.** `engine.append_message` (inside `persist_and_notify`) broadcasts a `History` snapshot unconditionally, and `handler/task.rs` intentionally defers the final `StreamEnd` until after persistence (see the Build Toolchain quirk below for why). A client that (a) rebuilds its view from `History` *and* (b) also pushes a new assistant entry on `StreamEnd` will double-render every reply in-memory — the duplicate vanishes on reconnect because the persisted history has only one copy. Clients must treat the final `StreamEnd` as a metadata-attachment event (tokens, timing, model), not an entry-creation event, unless no prior `History` has landed. Reference impl: `shore-tui/src/main.rs::handle_server_message` (StreamEnd branch for `finish_reason != "tool_use"`); regression test: `scenario_history_then_stream_end_no_duplicate`.

## Matrix / Embedded Homeserver

- **`embedded_state.json` + per-character `provision.json` can outlive the homeserver RocksDB.** They live under the shore data dir, the database lives under `matrix-server/database/`. Wiping the database (manually or via a conduwuit/continuwuity/tuwunel format bump) doesn't touch the state files — so on the next start, shore-matrix happily reuses admin + character access tokens the new DB has never heard of. Every follow-on call 401s: `set_display_name` fails, `matrix-sdk` crypto recovery errors, the sync loop dies with `M_UNKNOWN_TOKEN`, no rooms are visible, DMs get no response. URL equality (`state.homeserver_url == homeserver_url`) is **not** a liveness proof. The fix is to probe with `/_matrix/client/v3/account/whoami` before trusting any saved token; see `shore_matrix::provision::{check_token, wipe_embedded_state_and_characters, wipe_character_state}`. On a 401 against the admin token, every character's `provision.json` + `crypto_store` must be wiped — transitive invalidation — because their device_ids belonged to the dead DB; reusing the crypto store across device rotation corrupts olm sessions.

- **tuwunel rejects `database_backend = "rocksdb"` as an unknown key.** The conduwuit-family config format diverged: conduwuit/continuwuity accept it, tuwunel logs `Config parameter "database_backend" is unknown to tuwunel, ignoring`. Since RocksDB is the only supported backend across all three, we omit the key entirely rather than fork the config per binary. See `shore_matrix::homeserver::HomeserverConfig::generate_config`.

## Process Management

- **`drop(tokio::process::Child)` does NOT detach the child.** The comment on the shore-mcp auto-spawn path used to claim dropping the Child handle detached the spawned daemon. It doesn't — by default `kill_on_drop` is `false` so no signal is sent, but the child still inherits our process group and controlling terminal. If our pgroup receives SIGTERM (MCP client teardown, pkill -f) or SIGHUP (terminal close with huponexit), the daemon dies with us and leaves a stale `instances.json` entry. Real detachment requires `setsid()` in a `pre_exec` hook so the child becomes its own session leader with no controlling tty. See `shore-mcp/src/profile.rs::spawn_and_attach_test_daemon`; regression test in `shore-mcp/tests/autospawn_detach.rs`.

## Build Toolchain

- **`StreamEnd` for the final phase is emitted after persistence — not when the LLM stream ends.** `StreamConsumer::consume` (`shore-llm-client/src/stream.rs`) does NOT emit `StreamEnd` itself; the orchestrator emits it. For intermediate `tool_use` phases, `run_tool_loop` emits `StreamEnd(tool_use)` immediately so clients can render tool calls. For the final phase, `handler/task.rs` emits `StreamEnd(end_turn)` only after `persist_and_notify` completes. This closes a race where a client (e.g. shore-mcp) issuing a back-to-back `send` + `memory_compact` would see compact snapshot stale engine state — the just-streamed assistant message hadn't been appended yet — and write an empty `active.jsonl`, only for the pending persist to land afterwards as a one-line orphan. With this ordering, "client received `StreamEnd`" implies "the message is durable," and any follow-up command sees consistent state.

- **`find_turn_split` needs an explicit `keep_turns == 0` guard.** The natural
  reading of the loop ("walk right-to-left, return when `turns_seen >= keep_turns`")
  silently misbehaves at zero: `turns_seen` is incremented *before* the
  comparison, so the first user message encountered satisfies `1 >= 0` and
  gets treated as retained. Without the explicit `if keep_turns == 0 { return
  messages.len(); }` guard at the top of the function, `compact 0` would
  retain one user turn instead of zero. See
  `shore-daemon/src/memory/compaction/mod.rs::find_turn_split`.

- **`matrix-sdk` 0.16.0 does not compile on rustc 1.94+ without a `recursion_limit` bump.** The default 128 is exceeded computing the layout of `matrix_sdk::Client::sync()`'s async fn body — query depth increases by 130 during that computation. Upstream 0.16.0 and `main` both ship *without* `#![recursion_limit]` on the `matrix-sdk` crate root; the fix is a one-line attribute bump that upstream hasn't merged (see matrix-org/matrix-rust-sdk#6254, draft PR #6449). We carry a pinned git fork via `[patch.crates-io]` at `http://localhost:3000/eshen/matrix-rust-sdk.git` rev `8285d1ca5da1f18227ba4eddaeef9bf579a55de6`. **Caveat: `cargo update -p matrix-sdk` silently breaks the build by re-resolving to published 0.16.0** — do not run it. The patch must stay pinned until upstream ships a fix. Drop the patch and the pin together; don't half-revert.

## Testing via shore-mcp (live LLM verification)

The fastest way to verify daemon behavior with a real LLM is driving `shore-mcp` over JSON-RPC stdio. The MCP server auto-spawns its own test daemon, but when you need a **fresh test daemon with a specific config** (e.g., testing new tools), you must spawn the daemon manually and point MCP at it.

### Envvars

API keys live in `~/.config/shore/.env` (not committed). Source them before testing:

```bash
set -a; source ~/.config/shore/.env; set +a
```

Key envvars:
- `OPENROUTER_SHORE_PRIMARY` — main test key (Haiku/sonnet for `send`)
- `OPENROUTER_SHORE_TOOL` — key for tool-use testing (separate for cost tracking)
- `OPENROUTER_SHORE_EMBEDDING` — key for embedding tests
- `OPENROUTER_SHORE_IMAGES` — key for image generation tests
- Provider-specific keys (`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, etc.) for direct-provider tests

### Procedure

```bash
# 1. Source envvars
set -a; source ~/.config/shore/.env; set +a

# 2. Build the debug binary
cargo build -p shore-mcp

# 3. Use the default test profile (auto-spawns its own daemon on demand)
#    No manual daemon spawn needed for basic verification.
rm -f /tmp/mcp_stdin
mkfifo /tmp/mcp_stdin

./target/debug/shore-mcp > /tmp/mcp_out.jsonl 2>/tmp/shore-mcp.log < /tmp/mcp_stdin &
MCP_PID=$!

exec 3>/tmp/mcp_stdin

# 4. Send JSON-RPC frames
#    initialize
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}' >&3

#    list tools (verify new tools are advertised)
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' >&3

#    send a message that exercises the tool loop
echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send","arguments":{"text":"Write a memory about liking tea"}}}' >&3

#    memory_compact to verify compaction + markdown writes
echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory_compact","arguments":{}}}' >&3

# 5. Wait for responses, then clean up
sleep 30
exec 3>&-
kill $MCP_PID 2>/dev/null
```

### Inspecting results

```bash
# MCP JSON-RPC output (one line per frame)
cat /tmp/mcp_out.jsonl | jq .

# MCP server logs (includes daemon stderr if auto-spawned)
cat /tmp/shore-mcp.log

# Test profile data dir (markdown files land here)
ls ~/.local/share/shore-mcp-test/TestChar/memories/

# If the daemon got into a bad state, kill and let it respawn:
pkill -f 'shore-daemon.*shore-mcp-test'
```

### Critical gotchas

- **FIFO is mandatory.** `shore-mcp` exits when stdin closes. A heredoc (`<<'EOF'`) closes stdin after the last line, killing the server before the LLM response arrives. Use a named pipe (`mkfifo`) and keep the write fd open.
- **Environment variables must be exported to the daemon.** The daemon reads `OPENROUTER_API_KEY` (or provider-specific keys) at runtime. If you set it only for the MCP client, the daemon won't see it and will fail with "API key environment variable not set."
- **`--attach-main --allow-main-writes` on a test daemon.** These flags tell MCP "this is my main profile, allow mutations." It doesn't actually have to be the user's real profile — it's just the permission model. Without `--allow-main-writes`, mutation tools like `send` are refused.
- **`--daemon-addr` bypasses auto-spawn.** Without it, MCP tries to discover `shore-mcp-test` in the registry. If you spawned your own daemon, you must pass its address explicitly.
- **Default mode uses a persistent test profile.** `~/.local/share/shore-mcp-test/` survives across runs. Use `--ephemeral` for a tempdir that tears down on exit.
