# Shore V2 — Human TODO

Everything the autonomous PRD runner **cannot** do for you.

## Run milestone stories with real API keys

The PRD has 5 milestone stories (US-018, US-026, US-030, US-034, US-038) that
require live API calls. The autonomous agent will write the test harnesses but
cannot actually run them with your keys. You need to:

- [ ] Run US-018 (end-to-end conversation) with Haiku
- [ ] Run US-026 (full memory system) with Haiku
- [ ] Run US-030 (autonomy) with Haiku — this one takes patience (idle timers)
- [ ] Run US-034 (CLI + TUI) manually
- [ ] Run US-038 (Matrix bridge) with your Synapse setup

## Data migration testing with real V1 data

US-039 writes the compatibility code, but you need to test it against your
actual V1 data directories:

- [ ] Point V2 daemon at your real `~/.local/share/shore/` character data
- [ ] Verify SQLite databases open without errors
- [ ] Verify conversation JSONL files load correctly
- [ ] Verify LanceDB reindexing from existing entries
- [ ] Verify models.toml and config.toml parse (or produce clear errors)

## Manual config migration

Your config isn't complicated, so just rewrite it by hand for V2. No tooling
needed.

- [ ] Write new `config.toml` following V2 schema
- [ ] Write new `models.toml` following V2 schema
- [ ] Copy/adjust character definitions to V2 directory layout

## Redo the TUI PRD

The current PRD's US-033 (TUI) is intentionally minimal — just persistent
connection + conversation view. The full TUI feature set is a follow-up.

## Social need curve tuning

The V1 constants (item #3 above) are a starting point. After the autonomy
system is running (post Phase 4), you'll need to:

- [ ] Run with real characters for a few days
- [ ] Check if heartbeat timing feels natural
- [ ] Adjust tau constants, engagement thresholds, personality multipliers
- [ ] Check if weekday-aware heatmap filtering helps vs. hurts with small data
- [ ] Tune the dormant threshold (too aggressive = annoying, too passive = dead)

This is inherently empirical — no amount of pre-planning replaces observation.

## Prompt template authoring

The PRD builds the template *system* (loading, resolution, manifest). But the
actual prompt text — especially new V2 templates — needs human authoring:

- [ ] Review/revise `post_session.md` (heartbeat probe prompt)
- [ ] Review/revise `deferred.md` (deferred follow-up prompt)
- [ ] Review/revise `social_need.md` (probabilistic reach-out prompt)
- [ ] Review/revise `compact.md` (compaction instructions)
- [ ] Review/revise `tidy.md`, `collate.md`, `normalize_entities.md` (collation)
- [ ] Decide which V1 prompts to port vs. rewrite

### V1 retirement

US-041 handles the mechanical side (clear migration messages, archive V1 code).
But you need to decide:

- [ ] When to actually cut over (after how much testing?)
- [ ] Whether to archive V1 Python code in the same repo or a separate one
- [ ] Whether to keep V1 running in parallel during transition
- [ ] When to stop the V1 daemon for good
