# Unify Cache Anomaly Detection on CacheTracker

**Date:** 2026-04-10
**Status:** Approved

## Problem

Two independent systems detect cache anomalies:

1. **`check_cache_invalidation`** in `shore-llm-client/src/stream.rs` — inline during stream consumption. Fires when `cache_creation > 0 && cache_read == 0 && has_seen_cache_read`. Pushes `ServerMessage::CacheWarning` to SWP clients and logs `error!()`. Gated by `[advanced] cache_invalidation_warnings`.

2. **`CacheTracker`** in `shore-ledger/src/cache_tracker.rs` — state machine in the ledger client. Tracks Cold/Warm transitions with TTL expiry, model change, thinking toggle, compaction. Three anomaly types: `UnexpectedRead`, `UnexpectedWrite`, `KeepaliveMiss`. Fires `error!()` + `notify-send --urgency=critical`.

Problems:
- **Duplicate detection** with different logic and different thresholds for the same underlying events.
- **`UnexpectedRead` is a false positive factory.** It fires whenever the tracker is Cold but the provider cache is warm (daemon restart, stale reconstruct, OpenRouter routing). A "surprise cache hit" is never a problem.
- **`--urgency=critical`** for all anomalies, including the false-positive `UnexpectedRead`, makes desktop notifications unusable.

## Design

### Remove: Stream-level detection

Delete `check_cache_invalidation()` and `CacheContext` from `shore-llm-client/src/stream.rs`. Remove all `CacheContext` construction sites in the daemon handler, generation, and tool modules.

The `[advanced] cache_invalidation_warnings` config key becomes unused. Remove it from the config struct and mark it ignored in the example config.

### Remove: `Anomaly::UnexpectedRead`

Delete the `UnexpectedRead` variant from `CacheTracker`. When the tracker is Cold and observes `cache_read > 0`, transition to Warm with no anomaly. This is the tracker correcting its stale internal state, not detecting a problem.

### Keep: `UnexpectedWrite` and `KeepaliveMiss`

Both remain at `error!()` log level — they indicate real problems with cost impact:
- `UnexpectedWrite`: cache dropped when it shouldn't have (prefix changed, provider eviction).
- `KeepaliveMiss`: keepalive system failed to bridge a TTL gap, causing a cold start.

### Modify: Desktop notification urgency

In `cache_forensics::notify_anomaly`, change `--urgency=critical` to `--urgency=normal`.

### No wire-protocol change

`ServerMessage::CacheWarning` stays in the SWP protocol definition. It is simply no longer pushed from the stream layer. This avoids a breaking protocol change; clients that handle it (TUI status bar) will just never receive it during normal operation.

## Files Changed

| File | Change |
|---|---|
| `shore-llm-client/src/stream.rs` | Remove `CacheContext`, `check_cache_invalidation()`, and related test helpers |
| `shore-llm-client/src/cache_forensics.rs` | `--urgency=critical` → `--urgency=normal` |
| `shore-ledger/src/cache_tracker.rs` | Remove `UnexpectedRead` variant, update Cold→Warm transition, update tests |
| `shore-ledger/src/client.rs` | Remove `UnexpectedRead` match arm from anomaly string mapping |
| `shore-daemon/src/handler/mod.rs` | Remove `CacheContext` imports/construction, `has_seen_cache_read` field |
| `shore-daemon/src/handler/generation.rs` | Remove `CacheContext` construction, `has_seen_cache_read` update |
| `shore-daemon/src/engine/tools.rs` | Remove `CacheContext` usage |
| `shore-daemon/src/main.rs` | Remove `has_seen_cache_read` Arc creation |
| `shore-config/src/app.rs` | Remove `cache_invalidation_warnings` field |
| `shore-config/src/lib.rs` | Remove `cache_invalidation_warnings` from parsing/tests |
| `shore-daemon/tests/e2e.rs` | Remove `has_seen_cache_read` from test setup |
| `shore-test-harness/src/harness.rs` | Remove `has_seen_cache_read` from harness setup |
| `examples/config.toml` | Remove/comment `cache_invalidation_warnings` |
