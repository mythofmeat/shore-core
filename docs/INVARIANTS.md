# Invariants

The *goals* of Shore features and architectural decisions — what they MUST do, what they MUST NOT do.

Invariants are ongoing constraints on correctness and intent, distinct from:

- [FEATURES.md](FEATURES.md) — what the feature *does* (behavior, surface, config).
- [DECISIONS.md](DECISIONS.md) — why a choice was made at a point in time (historical).
- [ARCHITECTURE.md](ARCHITECTURE.md) — structural shape of the system.
- [QUIRKS.md](QUIRKS.md) — unavoidable external oddities we work around.

If a change would violate an invariant, either the change is wrong or the invariant needs to be explicitly revised here first. Features outlive implementations; invariants are the contract they owe no matter how they're built.

---

## Template

```markdown
### <feature name>

**Goal:** <what this must accomplish — one sentence, behavioral, not implementation>

**Must:**
- <constraint>

**Must not:**
- <constraint>

**Notes:** <rationale, edge cases, or tensions with other invariants>
```

Keep each entry short. If it runs past ~15 lines, the invariant is probably really two invariants.

---

## Open

_Empty. Add new invariant drafts here as future features are introduced or existing ones need revisiting._

## Invariants

### Memory: markdown is the durable store

**Goal:** Long-term memory is inspectable, editable, and recoverable as ordinary markdown files, not opaque database rows.

**Must:**
- Normal runtime reads and writes use `{character}/memories/**/*.md`.
- Memory tools must be able to list, read, search, and overwrite markdown files directly.
- Disabling memory must disable all `memory*` tools and block workspace access to the `memories/...` namespace.
- Private conversations must not expose memory tools or memory files.

**Must not:**
- New runtime memory features must not depend on the legacy SQLite/vector memory agent.

**Notes:** The legacy SQLite/vector modules are retained for compatibility, migration, tests, and old benchmarks only.

---

### Memory: compaction maintains markdown

**Goal:** Compaction turns transient conversation into durable markdown memory without losing important continuity.

**Must:**
- Compaction must not silently drop non-trivial content from the hot log.
- Compaction should see a bounded snapshot of existing markdown memory before writing, so it can merge or update files instead of blindly creating duplicates.
- Compaction should prefer idle time, but must still be allowed between turns when context or turn-count thresholds require it.
- Recent conversation turns must remain verbatim according to `keep_recent_turns`.

**Must not:**
- Compaction must not require a separate collation pass for correctness.

**Notes:** The old collation config is compatibility-only; memory maintenance now happens through markdown edits during normal tool use and compaction.

---

### Memory interaction pipeline

**Goal:** The character can use memory as files when it needs precision, and as an assisted markdown query when it needs synthesis.

**Must:**
- Direct memory file tools expose clear paths and markdown content.
- The natural-language `memory` tool answers from markdown files only.
- Writes to memory should prefer updating existing files over creating near-duplicate files.
- Memory access must be consistently gated across memory tools, workspace file tools, and private-mode tool lists.

---

### Scratchpad vs. memory

**Goal:** Two distinct surfaces serving different needs:

- **Scratchpad** — a literal workspace. Entirely self-authored by the character, used during interiority sessions for ideas and projects-in-progress. It is the character's own creative space, not a second memory store.
- **Memory** — the character's metaphorical brain. The substrate for continuity, coherence, and factual remembrance across compactions, without inflating the context window and degrading performance.

Memory is *who the character is* over time; scratchpad is *what the character is working on*.

**Must:**
- The scratchpad is character-authored — the user does not edit it directly. It is the character's, not a shared filesystem.
- Scratchpad content is not auto-injected into the prompt; it enters context only when the character explicitly reads it.

### Autonomy: active vs. dormant phases

**Goal:** Cost-saving and ethical. A character that keeps making background API calls for an absent user is burning money on nobody, and a user returning to a pile of "where are you?" messages from the character feels awful. Dormancy caps both: bounded autonomous activity while the user is away, re-enabled on return.

**Must:**
- Dormant = no autonomous LLM calls. Interiority ticks stop. The character wakes only when the user sends a message.
- Dormant → active is triggered by user engagement, not by a timer.
- The character itself cannot choose to go dormant.

**Notes:** A force-dormant command exists, but it's a debug affordance, not a user feature.

---

### Interiority tick recap

**Goal:** Every interiority tick ends with a short written recap of what the character thought about and what it plans to follow up on, so the next tick starts with narrative continuity instead of cold-starting. Without recaps, each tick is an island — the character reflects, decides nothing persists, and the next tick has to rebuild context from scratch. Recaps are the thread that makes the character's private life feel like an ongoing inner life rather than a series of disconnected prompts.

**Must:**
- Every tick produces a recap, whether or not it produced a user-facing message.
- The next tick can see what the previous tick thought (via the recap in its input context).

---

### Minimum interiority latency floor

**Goal:** Prevent ticks from firing immediately after the character just said something. Right after a reply there's nothing new to think about — a tick seconds later would rehash or invent. The floor enforces breathing room so ticks occur when there's genuine space for the character to have done something "offscreen," which is both cheaper (no gratuitous ticks) and more believable (no obsessive rumination seconds after the turn). It also guards against API call spamming.

**Must:**
- No interiority tick fires within the floor duration after the last assistant message.

---

### Max tool rounds per tick

**Goal:** A safety ceiling on any single interiority tick. Without it, a character that starts a deep investigation can spin up arbitrarily many tool rounds before producing anything useful — burning cost, blocking the next tick, and collapsing the tick model into "always thinking." The cap forces every tick to wrap up after a bounded amount of work, with any continuation happening in the next tick.

**Must:**
- No tick exceeds the cap on tool rounds.
- A tick that hits the cap must still produce a recap — the cap triggers a forced wrap-up, not an abort.

---

### Dormancy triggers (unreplied-tick count and idle-time)

**Goal:** Two redundant metrics for the same invariant — bound how much autonomous activity the character performs against an unresponsive user. Whichever trips first wins. The tick-count path catches "character is being chatty at someone who isn't replying"; the wall-clock path catches "user has just been silently away." Together they cap both the pestering-on-return problem and the burning-money-on-nobody problem described in the active/dormant invariant.

**Must:**
- Both `dormant_after_interiority_turns` and `dormant_after_idle_time` are evaluated; crossing either sends the character dormant.

---

### Image tool family (send / recall / list / remember / generate)

**Goal:** Images are a first-class artifact the character interacts with much like memory entries — it can archive them, list them, look back at them, show them, or create new ones. The five tools partition by *direction and purpose* so the character can choose cheap operations (list, recall) over expensive ones (generate) and keep a clean mental model of "is this for me to see or for the user to see."

**Per-tool responsibilities:**
- `send_image` — outbound: deliver an image from storage into the user's view as part of the reply.
- `recall_image` — inbound to character: load an image back into the character's own context so it can *see* it again (introspection, not delivery).
- `list_images` — catalog access: enumerate or search image memories without loading any.
- `remember_image` — archive: save an image into memory with a contextual description. Used for both user-shared images and character-generated ones the character wants to keep.
- `generate_image` — create a new image.

**Must:**
- `send_image` puts the image in the user's view; `recall_image` puts it in the character's own context. Neither subsumes the other.

---

### Tool-use loop budget (user turns)

**Goal:** The counterpart to the tick tool-round cap, applied to user-turn tool-use loops. Without a ceiling, a confused or greedy tool-use pattern can run indefinitely while the user is waiting. The cap bounds latency, cost, and the character's tendency to over-research a simple request.

**Must:**
- No single user turn exceeds the tool-use iteration cap.
- Hitting the cap forces a final response — the character must produce a reply, not abort.

---

### Editing past messages

**Goal:** The user's escape hatch for everything that goes wrong in an LLM conversation. A bad generation poisons future turns; a typo made the character misunderstand; a sensitive detail slipped in that you don't want to live in the log forever. Without edit access, the only recovery is "start over," which throws away the good turns too. With it, the user can surgically fix what went wrong and keep going.

**Must:**
- The user can edit and delete any message in the log (both user and assistant turns), by reference (ID or negative index).

**Notes:** Edits to the conversation log do *not* propagate into already-compacted memory entries. This is a known limitation, not an intentional design choice — retroactive propagation would be desirable but may be impractical or impossible.

---

### Regen guidance

**Goal:** A one-shot nudge on a regeneration ("be more concise," "try a different angle") that does not become part of the conversation canon. Editing the last user message to include the nudge would work but would bake noise into the log — content the user never actually said, eventually compacted into memory. Guidance stays ephemeral so the conversation log stays clean.

**Must:**
- Guidance affects only the immediate regeneration.
- Guidance does not persist in the conversation log and does not modify the original user message.

---

### Daemon/client split

**Goal:** One long-lived daemon owns every piece of authoritative character state — active character, conversation log, memory, autonomy scheduling, in-flight generations. Clients are pure views that attach, observe, and issue commands. This lets CLI, TUI, Matrix bridge, a future tray/notification driver, and any other client-shaped thing coexist and see the same live truth. A user can send from the CLI, watch the reply stream into the TUI, and have Matrix deliver it to their phone — all backed by one daemon. The daemon also outlives any individual client: quitting the TUI doesn't stop the character; ticks keep scheduling; the scratchpad stays put.

**Must:**
- All authoritative character state lives in the daemon.
- Clients must be interchangeable: closing or swapping one must not affect character state or what other clients see.
- The daemon's lifecycle is independent of any client — it can run with zero clients attached.

**Notes:** Client-local state (scroll position, unsent drafts, UI preferences) is fine — the boundary is between *character* state (daemon) and *presentation* state (client), along the lines one would expect from such an architecture.

---

### Client surfaces: CLI, TUI, bridges, GUIs

**Goal:** Distinct client shapes exist to cover distinct modes of contact with a character. The CLI is the canonical feature-complete surface — scriptable, composable, the reference that other clients conceptually flow out of. The TUI is the interactive live session surface — persistent connection, streaming, full editing affordances. Bridges like Matrix exist to deliver the character to places the user already is (Matrix specifically is a convenience to avoid building a native mobile app). GUIs fill the tray/notification niche — letting the character reach the user through OS-level affordances without the user opening a window.

**Must:**
- Exactly one CLI exists. It should be as feature-complete as reasonably possible; everything else conceptually flows out of it.
- Other client shapes (TUI, GUI, bridge) may multiply. Alternative TUIs — in different languages, with different strengths — are explicitly fine, including feature-identical ones.
- Every client, no matter how many, must go through the daemon; no client forks authoritative state.

---

### Matrix bridge and the embedded homeserver

**Goal:** Matrix exists as a convenience for mobile access — its reason for being is to avoid building a native mobile app. There is no deep integration with Matrix the protocol intended; it could as easily have been Telegram, and was chosen only because Telegram was already used for something else. The embedded homeserver (currently conduwuit / continuwuity / tuwunel) is the preferred path because it's simpler and suits the author's needs; external Matrix homeservers are also a supported path.

**Must:**
- Turning on Matrix must not require the user to independently run or manage a homeserver — the embedded one is the one-command default.
- Bringing your own homeserver must remain a supported option for users who already have one.

**Notes:** FEATURES.md still references Synapse — that's stale and should be corrected when convenient.

---

### Prompt cache preservation

**Goal:** Not breaking the Anthropic prompt cache is an extremely high-priority, system-level concern. The cache should stay warm and unbroken for as long as the user wants it, with invalidation only occurring in response to user actions that *obviously* should invalidate it — changing character definitions, changing system prompts, or editing old messages. Everything else the system does must be designed to leave the cacheable prefix stable.

**Must:**
- The cacheable prefix of the chat model's prompt must not change as a side effect of internal operations (memory writes, compaction, tick scheduling, tool-use loop bookkeeping, etc.).
- Cache invalidation is only acceptable in response to explicit user-driven changes: edits to `character.md`, `user.md`, `system.md`, or past messages.

**Notes:** `cache_ttl` is configurable per model, but for a long-running daemon 1h is objectively correct — 5m is economically worse. The knob exists for completeness, not because it's a meaningful tuning dimension in practice.

---

### Cache forensics

**Goal:** When cache behavior is suspected of being wrong, detailed per-request evidence must be available on demand — cache preservation (previous entry) is costly enough that "we think misses are happening" needs to be investigable at the request level, not just from aggregate diagnostics.

**Must:**
- When enabled, every cache-relevant request is recorded with enough detail to attribute hits / misses / creates.
- Disabled by default; normal operation produces no forensics file.

**Notes:** Not a designed-in feature with intent — a reactive debug instrument built to solve a money-draining problem. The invariant is the capability ("must be investigable when needed"), not this specific implementation shape.

---

### Remote access and security posture

**Goal:** Shore delegates security to the overlay network (Tailscale, ZeroTier, WireGuard, VPN) rather than building authentication into the protocol itself. The rationale is a trust-in-implementation concern: rolling your own security is easy to get wrong, and Tailscale/ZeroTier already solve this problem well. The user must actively acknowledge the implications of remote binding before the daemon will do it; beyond that, the responsibility is theirs — caveat emptor.

**Must:**
- Binding to a non-loopback address requires `unsafe_allow_remote_access = true` — the daemon refuses to start without it.
- `allowed_hosts` is a source-IP filter for accident prevention only; it must never be described to the user as authentication.

**Notes:** Authentication in the Shore protocol itself is not forbidden — it's simply not implemented, held back by an implementation-quality concern rather than a design principle. If added, it must be done right.

---

### Diagnostics and logging

**Goal:** A full suite of diagnostics is inevitable for a program this complex with this many moving parts. The more capable the logging, the easier bugfixing and problem-solving becomes — this is an aspirational direction, not a fixed feature surface. Comprehensive observability trumps minimalism here.

**Must:**
- Anything load-bearing in the system must be observable for debugging — gaps in diagnostics are bugs to be closed over time, not permanent design boundaries.

**Notes:** Easy redaction (hiding secrets, user content on demand) is a quality-of-life courtesy, not a hard rule. Existing diagnostic tools (`shore-diagnostics`, `shore status --diagnostics`, cache forensics, ledger) may not yet live up to this standard, but they should.

---

### Absurd and experimental clients are welcome

**Goal:** The client/server split is an open playground. Part of the appeal of having the daemon own all authoritative state is that *any* client shape is fair game — a Godot-based GUI with ambience and effects, a neovim plugin, whatever. Experimental and absurd client implementations are welcome, not merely tolerated. There is no single privileged "the GUI" slot being defended.

**Must:**
- No client is canonical in a way that prevents others from existing. Multiple GUIs and alternative clients can coexist.

**Notes:** `shore-gui-godot` is the Godot-based chat GUI — inherently absurd (chat UI in a game engine) and built as a recursive-self-improvement exercise orchestrated with fish scripts during free time / free tokens. It may move out of this repo when it's closer to complete. `shore-gui` (Tauri + React + TypeScript) is the canonical rich-UI client and lands independently.
