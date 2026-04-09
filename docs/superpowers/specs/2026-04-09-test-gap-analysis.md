# Integration Test Gap Analysis

> Generated from exhaustive git history analysis (55+ bugs across all branches).
> Reference for prioritizing future test work.

## Current Coverage

13 integration tests exist, covering ~20% of historical bugs:

| Test file | Tests | Bug classes covered |
|---|---|---|
| `integration_pipeline.rs` | 5 | Message roundtrip, persistence, streaming, tool use |
| `integration_autonomy.rs` | 3 | Phantom pings, dead timers, failed ping retry |
| `integration_recovery.rs` | 2 | History survival after crash, socket cleanup |
| `integration_providers.rs` | 3 | 429/500 retry, malformed SSE handling |

## Gap Cluster 1: Concurrency (highest priority — 0% covered)

The most dangerous bugs. Cause data corruption and waste real money.

| SHA | Bug | Root Cause |
|---|---|---|
| 2da0465 | Autonomous messages corrupt active.jsonl | Tick loop + handler both append simultaneously via raw `OpenOptions::append()` instead of engine lock |
| 36f71a9 | Interiority tick fires mid-conversation | `on_user_message` only reschedules in Dormant state; Active state leaves tick deadline untouched |
| fc1a7e6 | Generation continues after all clients disconnect | Spawned generation task has no cancellation on client drop; wastes API credits |
| 2108776 | Orphaned generation on new message | New request arrives during in-flight generation; old task keeps running and billing |
| 83a1f42 | Lagged broadcast clients silently lose messages | Broadcast `Lagged` error logged but client stays connected, missing messages indefinitely |

**Why not caught:** All current tests are sequential — send, wait, assert. None exercise concurrent tick loop + user messages, mid-stream disconnection, or overlapping generations.

**Tests needed:**
- `test_concurrent_user_message_and_autonomy_tick` — run tick loop + send user message simultaneously, verify active.jsonl integrity (no corruption, no interleaved writes)
- `test_interiority_tick_rescheduled_on_user_message` — send user message 90s into a 120s tick interval, verify tick doesn't fire mid-turn
- `test_generation_aborted_on_client_disconnect` — disconnect all clients mid-generation, verify generation task cancels (mock receives no further requests)
- `test_new_message_aborts_previous_generation` — send message while generation in-flight, verify previous task cancelled
- `test_broadcast_lag_disconnects_client` — connect slow client that doesn't read, send many messages, verify disconnect after consecutive lags

## Gap Cluster 2: Message Integrity Across Tool Loops (partially covered)

Bugs where multi-turn conversations with tools become structurally invalid.

| SHA | Bug | Root Cause |
|---|---|---|
| 7b61f12 | `trim_messages()` orphans tool_results | Trims between tool_use and tool_result; API rejects orphaned tool_result |
| 13dbe11 | Whitespace text blocks orphan tool results | LLMs emit "\n\n" blocks before tool_use, preventing merge |
| 054b658 | Message.content inconsistency | Empty for tool results, "\n\n" for intermediates, duplicated for responses |
| 0f27148 | Old thinking blocks break API | Anthropic rejects unsigned thinking blocks on non-recent turns |
| 2f50758 | Cached prompt teaches `<sendMessage>` syntax | System prompt included tag documentation in cache prefix |

**Why not caught:** Tool tests verify one tool call round-trips. No test builds a multi-turn conversation with tools and verifies the full message array stays API-valid after trim/compact.

**Tests needed:**
- `test_tool_conversation_valid_after_trim` — generate long conversation with multiple tool calls, trigger trim, verify no orphaned tool_results
- `test_whitespace_blocks_not_orphaned` — mock response with "\n\n" text + tool_use, verify message merge works
- `test_thinking_blocks_stripped_from_old_turns` — multi-turn with thinking, verify older turns have thinking removed before API call
- `test_request_body_no_sendmessage_syntax` — inspect mock's received request body, verify no `<sendMessage>` tags in system prompt

## Gap Cluster 3: Memory/Compaction State Machines (0% covered)

Bugs where first run works but second or third run fails. State machine invariants broken.

| SHA | Bug | Root Cause |
|---|---|---|
| 4194bed | Infinite compaction loop | `compaction_triggered` flag reset on every message; engine not reloaded after compaction |
| 537eb20 | Collation merge/split churn loop | Generated entries have empty `collated_at`, making them immediate candidates for next run |
| fcf65bf | Watermark reprocesses every entry | Compared against `pipeline_start` instead of entry `updated_at` |
| 1075cc4 | Blank recaps after deserialization | `content_blocks` deserialization skips `normalize()` to derive `content` field |
| 54219e6 | FTS corruption with no recovery | DELETE+INSERT can't handle pre-existing corruption; needed DROP+CREATE |
| 6cceadf | Partial compaction failure leaves orphans | No compensating delete or transaction guard on multi-step compaction |

**Why not caught:** Compaction is completely untested through the integration harness. These are multi-cycle bugs — run once works, run twice breaks.

**Tests needed:**
- `test_compaction_single_trigger_not_infinite` — trigger compaction, verify it runs once and doesn't loop
- `test_compaction_idempotent_on_second_run` — trigger compaction twice with no new messages, verify no churn
- `test_compaction_rollback_on_failure` — inject failure mid-compaction, verify all-or-nothing semantics
- `test_fts_rebuild_on_corruption` — corrupt FTS table, trigger search, verify rebuild succeeds
- `test_recap_content_not_blank_after_restart` — send messages, compact, restart, verify recaps have content

## Gap Cluster 4: Provider-Specific Protocol (partially covered)

Bugs in how requests are serialized for each provider.

| SHA | Bug | Root Cause |
|---|---|---|
| 2ae092a | Gemini tool ID collisions | Same tool called twice gets same ID; needed enumerated index |
| 2081b63 | OpenAI drops system messages with array content | `translate_messages()` only handled "assistant" and "user" array-content |
| 0b3e799 | OpenRouter routes to OpenAI | Missing `base_url` in defaults |
| addada6 | Keepalive pings strip tools → cache write not read | Ping request cloned without tool list; different hash forces cache write |
| 1ca7145 | O(n^2) SSE string slicing | Per-line string slice reallocates entire accumulated buffer |

**Why not caught:** We mock at the Anthropic SSE level. Provider-specific serialization happens inside `shore-llm-client` before the HTTP call. We don't inspect what was actually sent.

**Tests needed:**
- `test_request_body_includes_tools` — after send_and_collect, inspect mock's received request body, verify tools array present
- `test_keepalive_ping_request_matches_original` — after keepalive fires, compare ping request body to original request body (same tools, system prompt, etc.)
- Per-provider unit tests in `shore-llm-client` (not integration tests): verify serialization for Gemini tool IDs, OpenAI system messages, etc.

## Gap Cluster 5: Ledger/Usage Tracking (0% covered)

Bugs where API calls aren't properly recorded.

| SHA | Bug | Root Cause |
|---|---|---|
| 46ff6d0 | Failed calls not recorded in ledger | LedgerStream not finalized on error paths |
| 96e5111 | Schema migration fails on existing DBs | CREATE TABLE IF NOT EXISTS is no-op; old DBs can't INSERT new columns |
| 3d5174f | Cache tracker never runs for OpenRouter | Provider gated on "anthropic" string, never matches OpenRouter |

**Tests needed:**
- Add ledger assertions to existing tests: after every `send_and_collect`, verify `harness.read_ledger()` has an entry
- `test_failed_call_recorded_in_ledger` — enqueue 500, verify ledger records the failed call
- `test_ledger_migration_on_old_schema` — create old-schema DB, boot daemon, verify INSERT works

## Gap Cluster 6: Config/Startup Edge Cases (0% covered)

Pure-function bugs better served by unit tests than integration tests.

| SHA | Bug | Root Cause |
|---|---|---|
| 739f4bb | .env overridden by inherited env vars | `dotenvy::from_path()` skips vars already in env |
| 99354fd | Same model name across providers clobbers catalog | BTreeMap keyed by short name, not qualified name |
| 31f20cb | Token display: 793 shows as "793K" | `format_k` didn't append suffix itself |

**Tests needed:** Unit tests in `shore-config`, not integration tests.

## Gap Cluster 7: TUI/CLI (out of scope for integration harness)

| SHA | Bug | Root Cause |
|---|---|---|
| 23739c1 | TUI hardcodes "Assistant", quit broken, double event processing | Multiple TUI-specific issues |
| 255db5f | Duplicate NewMessage events | Engine broadcast + handler both send |
| ea34479 | Non-PNG images sent with PNG flag | Image conversion missing |
| a526d83 | UTC timestamps break time-gap markers | Should use local offset |

**Tests needed:** Terminal snapshot testing or manual testing. Not suitable for the integration harness.

## Priority Order

Based on damage potential (money lost, data corrupted, user-visible breakage):

| Priority | Cluster | Why |
|---|---|---|
| 1 | Concurrency | Data corruption + wasted API credits. Hardest to catch manually. |
| 2 | Compaction state machines | Infinite loops burn money. Corruption loses user data. |
| 3 | Message integrity / tool loops | API rejections break conversations. |
| 4 | Ledger assertions | Easy to add to existing tests. Catches silent billing bugs. |
| 5 | Request body validation | Catches cache-busting and prompt leaks. |
| 6 | Provider-specific serialization | Unit tests in shore-llm-client. |
| 7 | Config edge cases | Unit tests in shore-config. |

## Estimated Effort

| Cluster | New tests | Harness changes needed |
|---|---|---|
| Concurrency | 5 | Need `connect_second_client()`, generation abort detection, concurrent send helpers |
| Compaction | 5 | Need compaction trigger helper, multi-cycle support |
| Message integrity | 4 | Need multi-turn conversation builder, request body inspection |
| Ledger | 3 | Need `read_ledger()` helper (may already exist) |
| Request validation | 2 | Already possible via `mock_llm.received_requests()` |
