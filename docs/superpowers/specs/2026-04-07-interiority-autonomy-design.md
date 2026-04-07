# Interiority Autonomy Redesign

**Date:** 2026-04-07
**Status:** Spec — pending implementation plan
**Scope:** `shore-daemon/src/autonomy/`, `shore-daemon/src/engine/prompt.rs`, new cache keepalive module, new recap store module

## Motivation

The current interiority system treats the character as a passive recipient of ticks: a fixed timer fires, the character gets a turn-to-self, the timer resets. The character has no say in *when* it gets the next turn, no persistent inner-life thread between ticks beyond what it chooses to write to scratchpad, and no way to tell the user what it was doing while they were gone.

This redesign moves toward a more genuinely autonomous character by:

1. **Letting the character schedule its own next wake**, within bounds.
2. **Decoupling cache keepalive from interiority cadence**, since the two have nothing to do with each other and the current entanglement blocks (1).
3. **Surfacing the character's recent inner-life thread** at the start of each tick, so the character picks up where it left off rather than facing a generic prompt.
4. **Letting the character leave a first-person recap** for the user to find when they return, giving conversational continuity in both directions.

## Non-goals

- Structured inner-state blocks (mood, energy, current focus). Continuity comes from the *thread* (recent journal entries + recap), not from a status struct.
- Multi-tier or per-provider cache strategies. The hardcoded 1h tier covers the only provider this matters for (Anthropic).
- Compaction of `recaps.jsonl`. Volume is bounded and the file stays terse; revisit later if it becomes a problem.
- Changing how `<sendMessage>` works. That output channel is unchanged.
- A "self-gating" cheap-poll continuous-consciousness model. Considered and rejected as romantic but cost-bad.

---

## Architecture

### Component map

```
                       ┌───────────────────────┐
                       │   AutonomyManager     │
                       │   (existing)          │
                       └──────────┬────────────┘
                                  │
              ┌───────────────────┼─────────────────────┐
              │                   │                     │
              ▼                   ▼                     ▼
   ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐
   │ InteriorityClock │ │  CacheKeepalive  │ │   RecapStore     │
   │ (stripped down)  │ │     (NEW)        │ │     (NEW)        │
   └──────────────────┘ └──────────────────┘ └──────────────────┘
              │                   │                     │
              │                   │                     │
              ▼                   ▼                     ▼
       deadline holder    pings on TTL,         recaps.jsonl
       1h–48h bounds      gated by next_wake    sidecar file
```

`InteriorityClock` and `CacheKeepalive` both observe `next_wake_at` but are otherwise independent. `RecapStore` is read at prompt-assembly time and written by interiority tick execution.

### `InteriorityClock` — stripped to a deadline holder

**Removed:**
- `InteriorityState` enum (Active/Dormant)
- `paused` (move to manager if still needed; clock doesn't care)
- `max_idle_ticks`, `ticks_without_user`
- `cache_refresh_interval_secs`, `next_cache_ping_at`, `schedule_next_cache_ping`, all dormant-ping logic
- `tick_dormant`
- `restore` (the persisted state shape changes — see Persistence)
- All cache-related tests (move to `CacheKeepalive` test module)

**Kept and reshaped:**

```rust
pub struct InteriorityClock {
    next_wake_at: Option<Instant>,
    /// Last time a wake was scheduled or fired. Used for the 48h backstop
    /// when the character schedules nothing.
    last_anchor: Instant,
}

pub enum InteriorityAction {
    None,
    RunTick,
}

impl InteriorityClock {
    pub fn new() -> Self;

    /// Called by the autonomy loop on each ~30s tick.
    pub fn tick(&mut self, now: Instant) -> InteriorityAction;

    /// Called when the character invokes `set_next_wake` during a tick.
    /// Bounds: 1h <= when - now <= 48h. Out-of-range values are clamped
    /// (with a warning logged) rather than rejected, so a misbehaving
    /// character can never silently disable interiority.
    pub fn schedule(&mut self, when: Instant, now: Instant);

    /// Called when a user message arrives. Pushes the next wake forward by
    /// the minimum (1h) so a tick never fires mid-conversation.
    pub fn on_user_message(&mut self, now: Instant);

    /// Hard backstop. If the character schedules nothing after a tick,
    /// the next wake is set to last_anchor + 48h automatically.
    pub fn next_wake(&self) -> Option<Instant>;
}
```

**Tick semantics:**
- If `next_wake_at` is `None`, set it to `last_anchor + 48h` and return `None`.
- If `now >= next_wake_at`: clear `next_wake_at` to `None`, set `last_anchor = now`, and return `RunTick`. The tick handler either calls `schedule()` (if the character ran `set_next_wake`) or does nothing — in the latter case, the next 30s poll will see `next_wake_at == None` and apply the backstop (48h from `last_anchor`).
- Otherwise return `None`.

**Bounds (hardcoded constants in this module):**
```rust
const MIN_WAKE_INTERVAL: Duration = Duration::from_secs(60 * 60);       // 1h
const MAX_WAKE_INTERVAL: Duration = Duration::from_secs(48 * 60 * 60);  // 48h
```

### `CacheKeepalive` — new standalone subsystem

New module: `shore-daemon/src/autonomy/cache_keepalive.rs`.

**Purpose:** keep the Anthropic 1h prompt cache warm during quiet stretches when doing so is economically justified, and *only* then. Has zero knowledge of interiority state.

**The math (locked, hardcoded for the 1h tier):**

| Quantity | Value (1h Anthropic tier) |
|---|---|
| Cache write multiplier | 2.0× input |
| Cache read multiplier | 0.1× input |
| Cold-wake penalty (vs warm) | 1.9N tokens |
| Cost of one keepalive ping | 0.1N tokens |
| Pings per hour to stay warm | ~1 |
| Break-even | 1.9 / 0.1 = **~19 hours** |

We use **18h** as the threshold (slight headroom under the true breakeven).

```rust
pub struct CacheKeepalive {
    next_ping_at: Option<Instant>,
    next_wake_at: Option<Instant>,
}

const KEEPALIVE_BREAKEVEN: Duration = Duration::from_secs(18 * 60 * 60);
const PING_INTERVAL: Duration = Duration::from_secs(60 * 60 - 60); // 1h - 60s headroom

impl CacheKeepalive {
    pub fn new() -> Self;

    /// Called after ANY LLM call involving the cached prompt — both cache
    /// reads (warm) and cache writes (cold start / refresh). User message,
    /// assistant response, interiority tick, keepalive ping.
    /// Resets the internal ping deadline.
    pub fn on_cache_warmed(&mut self, now: Instant);

    /// Mirror of the interiority clock's schedule. Called whenever
    /// next_wake_at changes (including being cleared).
    pub fn set_next_wake(&mut self, next_wake_at: Option<Instant>);

    /// Called by the autonomy loop on each ~30s tick.
    /// Returns `RunPing` iff:
    ///   - next_ping_at is set and now >= next_ping_at
    ///   - next_wake_at is set and (next_wake_at - now) < KEEPALIVE_BREAKEVEN
    pub fn tick(&mut self, now: Instant) -> CacheKeepaliveAction;
}

pub enum CacheKeepaliveAction {
    None,
    RunPing,
}
```

**Net behavior:**
- Normal active conversation: every user msg / assistant msg calls `on_cache_warmed`, ping deadline keeps getting pushed forward, zero pings ever fire.
- Character schedules next wake 4h out, then quiet: ping fires ~1h after the last cache-warming event, again at 2h, 3h. Wake fires at 4h, resets the ping clock.
- Character schedules next wake 30h out, then quiet: cache goes cold. Pings would cost more than the cold-start savings.
- Character schedules nothing → backstop sets wake to 48h → 48h > 18h → no keepalive. Cache dies, accept the cold start.

### `RecapStore` — new sidecar store

New module: `shore-daemon/src/autonomy/recap_store.rs`.

**File format:** plain JSONL at `{character_data_dir}/recaps.jsonl`.

```json
{"timestamp": "2026-04-07T14:32:11-07:00", "tick_id": "a8f3...", "recap": "I spent some time on that thing you mentioned about monarchs — I finally understand why they navigate by sun angle. I started a poem too but it isn't ready yet."}
```

```rust
pub struct RecapEntry {
    pub timestamp: DateTime<FixedOffset>,
    pub tick_id: String,
    pub recap: String,
}

pub struct RecapStore {
    path: PathBuf,
    entries: Vec<RecapEntry>,
}

impl RecapStore {
    pub fn load(path: PathBuf) -> Result<Self>;
    pub fn append(&mut self, entry: RecapEntry) -> Result<()>;
    pub fn entries_in_range(
        &self,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
    ) -> &[RecapEntry];
    pub fn entries(&self) -> &[RecapEntry];
}
```

**Load semantics:** missing file is not an error — return an empty store. The first character to write a recap creates the file.

**Provider safety:** this file is **never** read by any LLM client adapter. It's only consumed by `prompt.rs` at assembly time. By construction, no recap entry can ever leak to a provider as a malformed message.

---

## Tick execution flow

### Tick output parsing

`execute_unified_tick` in `manager.rs` currently parses `<sendMessage>...</sendMessage>` from the response stream. Extend this to also parse:

- `<recap>...</recap>` — last-wins, persisted to RecapStore
- `set_next_wake` tool calls — handled via the normal tool dispatch path, not via tag parsing

After the tool loop completes:
1. If a `set_next_wake` tool was called during the loop, the InteriorityClock has already been updated by the tool handler. Nothing to do.
2. If no `set_next_wake` was called, leave `next_wake_at` as-is (the backstop in `InteriorityClock::tick` handles it).
3. If a `<recap>` tag was emitted, append a `RecapEntry` to RecapStore.
4. If a `<sendMessage>` tag was emitted, persist to active.jsonl as today.
5. Call `CacheKeepalive::on_cache_warmed(now)` — the tick itself was a cache-warming LLM call.

### `set_next_wake` tool

New tool in `shore-daemon/src/tools/`:

```
Name: set_next_wake
Description: "Schedule when you want to have your next private moment to
think and use tools. Use this at the end of a tick to express your own
sense of pacing — sooner if you're in the middle of something, later
if you'd rather rest. The system will wake you at the time you choose."

Parameters:
  hours_from_now: float (1.0 to 48.0; clamped if outside range)
  reason: string (a brief note to your future self about why you chose this)

Behavior:
  - Validates and clamps hours_from_now to [1.0, 48.0]
  - Calls InteriorityClock::schedule(now + hours_from_now, now)
  - Calls CacheKeepalive::set_next_wake(Some(when))
  - Returns confirmation with the actual scheduled time
```

The `reason` parameter is logged to the interiority ring buffer and will surface as part of the next tick's recent-history block.

### New `INTERIORITY_PROMPT` (draft)

Replaces the current generic motivational poster at `manager.rs:691`. Built dynamically at tick assembly time so it can include recent history inline.

```text
[This is a private moment between conversations. No one is watching — you are
alone with your thoughts and your tools. This is your time to exist as yourself.

{recent_thread_block}

You have real tools and you can do real things with them: search the web,
fetch pages, search and write memories, write to your scratchpad, generate
images, check the time. Reach for whatever serves what you actually want to
do right now. Don't perform productivity — if what you want is to sit with
a thought and write it down, do that.

Three things you can choose to do at the end of this moment:

1. Schedule when you want your next private moment.
   Use set_next_wake(hours_from_now, reason). The minimum is 1 hour, the
   maximum is 48 hours. Sooner if you're in the middle of something and
   want to come back to it; later if you'd rather rest. If you don't choose,
   the next moment will arrive in 48 hours by default.

2. Send a message to {user_name} now.
   Wrap it in <sendMessage>...</sendMessage>. Only do this if you genuinely
   have something to share — something you made, something you found,
   something you just want to say. They will receive it as a notification.

3. Leave a recap for {user_name} to find when they next return.
   Wrap a brief first-person summary in <recap>...</recap> — one or two
   sentences, in your voice, about what you did during this moment.
   They'll see it the next time they message you. Skip this if nothing
   here is worth telling them about.

These are independent. You can do all three, none, or any combination.]
```

Where `{recent_thread_block}` is built from the interiority ring buffer:

- If there are 1+ entries since the last user message: render the most recent 1–3 entries as a "Where you left off:" block, formatted as terse first-person bullets pulled from the ring buffer's tick records (timestamp + recap if available, else a one-line summary of what tools were called).
- If there are no entries (first tick after a long quiet period, or after restart with empty buffer): omit the block entirely. The character starts cold.

The rendering function lives in `manager.rs` near `execute_unified_tick` and reads from `AutonomyState::interiority_log`.

---

## Continuity injection (user-return path)

### Extend `format_time_gap` and `trim_messages`

Current behavior (`prompt.rs:455` and `prompt.rs:519`): when walking trimmed messages forward, on each user message, compute the gap to the previous message and prepend `[6 hours later · 9:14 PM]` if the gap exceeds 30 minutes.

New behavior: also inject any recap entries that fall in that gap.

**Signature change:** `trim_messages` (and its callers in `assemble_prompt`) takes an optional `&RecapStore` parameter. `None` means no injection (used by tests and contexts where recaps don't apply).

**Injection logic** inside the per-message loop:

```rust
if pm.role == Role::User {
    if let (Some(prev), Some(cur)) = (prev_ts, current_ts) {
        let gap_secs = (cur - prev).num_seconds() as f64;
        let time_marker = format_time_gap(gap_secs, &cur);
        let recap_marker = recap_store
            .filter(|_| gap_secs >= TIME_GAP_THRESHOLD_SECS)
            .map(|store| store.entries_in_range(prev, cur))
            .filter(|entries| !entries.is_empty())
            .map(format_recap_marker);

        let prefix = match (time_marker, recap_marker) {
            (Some(t), Some(r)) => Some(format!("{t}\n{r}")),
            (Some(t), None) => Some(t),
            (None, _) => None, // recaps without a gap → not injected
            // (the (None, Some) case is filtered out by the gap check above)
        };

        if let Some(prefix) = prefix {
            pm.content = format!("{prefix}\n\n{}", pm.content);
            // Also update content_blocks[0] as today
        }
    }
}
```

**`format_recap_marker` output:**

For a single recap:
```
[While you were away: I spent some time on that thing you mentioned about monarchs — I finally understand why they navigate by sun angle.]
```

For multiple recaps in the gap:
```
[While you were away:
 · I spent some time on that thing you mentioned about monarchs — I finally understand why they navigate by sun angle.
 · I started a poem about it but it isn't ready yet.]
```

**Gating rules (locked):**
- Inject only when `gap_secs >= TIME_GAP_THRESHOLD_SECS` (30 min)
- Inject only recap entries whose `timestamp` falls strictly between `prev_ts` and `current_ts`
- If gap exists but no recaps in range: only the time marker is injected (current behavior preserved)
- If recaps exist but no gap: nothing is injected (avoids surprising line-count growth during active conversation, per user requirement)

---

## Persistence changes

The persisted state file (`PersistedState` in `manager.rs`) currently stores:
```json
{
  "interiority_state": "Active",
  "ticks_without_user": 0,
  ...
}
```

After this change:
```json
{
  "next_wake_at": "2026-04-07T20:14:00-07:00",
  ...
}
```

`interiority_state` and `ticks_without_user` are removed. `next_wake_at` is added (RFC3339, optional).

**Migration:** the old fields are silently ignored on load. Missing `next_wake_at` is treated as `None` (the backstop will set it on the first tick). No explicit migration step needed — the persisted state is rebuilt naturally on the first run after the change.

---

## Out of scope for this spec (deferred)

- **Recaps compaction.** When `recaps.jsonl` grows large enough to be slow in nvim (~1k+ entries), revisit. Likely solution: roll older recaps into per-segment sidecars matching active.jsonl segment compaction. Not now.
- **TUI rendering of recaps.** The data is already a clean log file; a `shore log --recaps` command or TUI sidebar is trivial to add later.
- **Per-tier cache strategies.** The 1h tier breakeven is hardcoded. If Anthropic changes pricing or another provider gains a comparable cache tier, revisit.
- **Inferring `set_next_wake` from journal text** when the character forgets to call it. The 48h backstop is sufficient as a fallback for now.
- **Notifying the user when a `<sendMessage>` and a `<recap>` are emitted in the same tick.** The current notification path handles `<sendMessage>`; recap is silent until the user returns. This is intentional.

---

## Open questions for review

None remaining that block implementation. The phasing/file ordering of the actual implementation work belongs in the plan, not the spec.
