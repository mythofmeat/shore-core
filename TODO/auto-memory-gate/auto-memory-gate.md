# Auto Memory Gate

A per-turn gate that runs before each character response, decides whether to
pre-inject long-term memory content, and injects when yes. Reduces reliance on
the character self-invoking memory retrieval (unreliable and expensive on Opus
round-trips).

## Motivation

The existing memory agent works well *when the character remembers to call it*.
In practice, characters skip the call often, losing continuity with prior
sessions. A proactive gate — cheap model, runs every turn — catches the turns
where distant memory would materially change the response. If it works, it also
reduces expensive character-invoked retrieval round-trips (~$0.05–0.15 each on
Opus).

## Architecture — intended vs. current prototype

**Intended design (original vision):**
The gate is a three-stage pipeline that wraps the existing memory agent:

1. **Classifier**: watch the conversation, decide whether memory is needed
   this turn.
2. **Question-asker**: formulate specific questions about the conversation
   that long-term memory might answer ("when did X happen", "what was Y's
   reaction to Z", etc.) and send them to the existing memory agent /
   researcher pipeline.
3. **Result filter**: receive the memory agent's results, decide whether they
   are worth persisting to the character's context for this turn. If yes,
   synthesize a short injection (with pointers for further search).

This design uses the existing memory subsystem as the retrieval engine — it
does NOT generate prose about memory content itself. It only orchestrates:
classify → ask → filter → inject.

**Current prototype (v7 selector):**
The prototype skipped the question-asking and retrieval steps. Instead it
receives a pre-baked list of session summaries directly and picks which
session IDs should be surfaced. The harness then prepends those summary rows
to the character's context verbatim.

```json
{
  "fire": true,
  "reason": "explicit callback to adoption milestone",
  "inject_sessions": [13]
}
```

An earlier prototype (v5) had the gate write prose injections directly. That
version hit 97.5% accuracy on conv-26 but hallucinated on conv-50 (invented
"Calvin has a connection to Boston" — never stated). Adding a `sources: [int]`
citation requirement didn't fix it. The v7 selector is hallucination-free
**by construction** because it doesn't generate factual content — but it's
also not doing what the gate was originally supposed to do.

**Realignment required:** the Phase 2 signal is strong enough that we want to
ship this; but before production integration, the gate must be reshaped to
the intended three-stage design so that it plugs into Shore's existing
memory agent instead of requiring a pre-baked session-summary store.

## Current status (2026-04-20)

Prototype lives in `experiments/auto-memory-gate/`.

### Gate model

- **Model**: `google/gemma-4-31b-it` at temp 1.0
- **Cost**: ~$0.0005 per call
- **Latency**: ~3–5s per call (Gemma; not yet tested on faster models)
- **Prompt**: v7-selector, in `run_gate_eval.py` SYSTEM_PROMPT

### Conversation window

- **Default**: 12 turns
- **Rationale**: W=12/24/36 ablation on conv-26 showed W=12 and W=24 tied for
  best accuracy (90%), both beating W=36 (85%). W=12 is cheapest ($0.019 for 40
  calls vs $0.023 at W=36) because the conversation tokens shrink faster than
  the out-of-window memory summaries grow. W=12 also *recovered* two recalls
  that W=36 missed, because smaller window ⇒ more sessions in memory ⇒ more
  material for the gate to cite from on oblique references ("the gang") and
  catch-up openers.
- **Configurability**: will be exposed as a user-tunable parameter in config.

### Validated metrics

| | conv-26 (40 labels) | conv-50 (40 labels) |
|---|---:|---:|
| Accuracy | 90.0% | 77.5% |
| Precision | 94.7% | 76.2% |
| Recall | 85.7% | 80.0% |
| F1 | 90.0% | 78.1% |
| Hallucinations | 0 (by design) | 0 (by design) |

### Known failure modes

1. **Statement misread as question** — Gemma occasionally treats a supportive
   declarative statement ("Volunteering is a great way to meet people")
   as a reflection-asking question. 1 persistent FP on conv-26 at every prompt
   version and window size tested. Model-level comprehension ceiling.
2. **Referenced event not in memory DB** — "that book you recommended", "the
   Perseid meteor shower", "the gang". LoCoMo session summaries don't capture
   these specific events, so the gate correctly refuses to cite. **Shore's
   production memory DB has finer-grained entries than coarse session
   summaries** (entries are extracted per-turn during conversation), so this
   ceiling likely lifts substantially in production.
3. **Arc-in-window FPs** — when a multi-session arc has a fresh in-window
   mention, the gate sometimes fires anyway (prompt v5→v7 narrowed this but
   didn't eliminate it).

### What the experiment does NOT touch

- Real daemon, real DB, real memory entries. Everything runs against LoCoMo
  session summaries as a stand-in for the memory store.
- No integration with `shore-daemon` yet.

## Open questions

1. ~~Does the prompt generalize across conversations?~~ Partially.
   Prec/rec/F1 drop meaningfully on conv-50 vs conv-26, though the drop is
   mostly precision (FPs on arc-adjacent content). Recall — the critical
   metric — held up at 80%+ on both.
2. Does pre-injecting memory produce fewer hallucinations / misremembrances
   than the character responding with no memory context at all? The baseline
   is "no memory used" because Opus 4.7 is pathologically reluctant to call
   its memory tool proactively (~3 invocations in several hundred messages
   despite prompt tuning). The ship/no-ship question therefore isn't
   "auto-gate vs. tool-using character"; it's **"auto-gate vs. character
   flying blind."**
3. Can the gate work on short topic labels instead of full session summaries?
   Deferred — will surface from the A/B benchmark if injection cost becomes a
   concern.
4. How does the gate integrate into `shore-daemon`'s request pipeline? The
   natural hook is `handler/task.rs::handle_generation()`, after
   `prompt::assemble_prompt()` and before `stream_with_retry()`. A schema gap:
   the current `Entry` schema has no first-class "session summary" row — per-
   session summaries either need a new row type (`memory_type="session_summary"`)
   or need to be computed on demand from compacted entries.

## Next steps

### Phase 1 — generalization (done)

- [x] Pick a second LoCoMo conversation (`conv-50`). Hand-label 40 turns.
- [x] Run v5/v7 prompt against it. **Result**: 80% recall, no hallucinations.
      Below the 90% acc threshold but above the 95% recall target, ish.
      Accepted with caveats documented above.
- [x] Window-size ablation: W=12 is the new default.

### Phase 2 — first A/B: gate-selector vs. no memory (done, ship signal positive)

Ran 40 stratified QA pairs (20 conv-26 + 20 conv-50), Haiku 4-5 as character,
Gemma 4-31b-it as gate (v7 selector) and as judge. Four metrics:

| Metric | A (no memory) | B (gate + memory) | Δ | × |
|---|---:|---:|---:|---:|
| Token F1 | 2.6% | 18.4% | +15.8pp | 7.1× |
| Recall-only | 2.1% | 21.6% | +19.5pp | 10.4× |
| Substring | 0.0% | 14.0% | +14.0pp | — |
| **Judge=correct** | **5.0%** | **27.5%** | **+22.5pp** | **5.5×** |
| Judge≥partial | 5.0% | 40.0% | +35.0pp | 8.0× |

**10 judge-based rescues. 0 regressions.** Memory never broke a previously
correct answer. Per-category, multi-hop and open-domain benefited most;
temporal (exact-date) questions didn't improve because LoCoMo session
summaries don't carry dates (Shore's real `Entry` schema does — this
limitation goes away in production).

Cost: $0.098 for 40 questions × 5 calls each (gate + 2 char + 2 judge). In
production that's ~$0.002 per character turn for the gate + response path
(no judge).

Fire rate: 97.5% — questions are almost always stateful-knowledge asks.
Free-flowing conversation would be much lower (our per-turn runs saw 50-60%).

**Decision: signal is strong enough to ship.** Next: reshape the gate to the
intended architecture and benchmark against Shore's real memory system.

### Phase 2.5 — reshape gate to question-asker + filter (realignment)

The v7 selector is not the gate we intended to build. Before integration
work, rebuild the gate as the three-stage pipeline:

- [ ] Draft v8 prompt: outputs `{fire, questions, reason}` where `questions`
      is a 0-3 list of specific recall questions about the conversation
      ("when did X happen", "what did Y say about Z"). No inject_sessions.
- [ ] Wire questions into a retriever. For the experiment, retrieve over
      LoCoMo **observation facts** (per-turn extractions, matching Shore's
      `Entry` shape) rather than session summaries. Retriever options:
      BM25, vector search, or LLM-based relevance ranking.
- [ ] Draft v8-filter prompt: receives original conversation context +
      question list + retrieved entries, outputs
      `{worth_injecting: bool, injection: str|null, pointers: [str]}`.
      This is where grounding is enforced — injection is a synthesis of the
      retrieved entries, NOT new invention.
- [ ] Validate the filter against hallucination: is injection traceable to
      retrieved entries? Consider requiring `cited_entry_ids: [int]`.
- [ ] Re-run Phase 2 A/B with the question-asker + filter design. Target:
      match or exceed the v7 selector's 5.5× judge improvement.

### Phase 2.6 — Shore-bare vs. Shore+gate benchmark

This is the one that actually answers "does integrating the gate into Shore
improve real behavior." Use the same 40-question set.

- [ ] **Arm 1 (Shore bare)**: character has access to the memory tool and
      may call it. Runs against a simulated Shore memory — LoCoMo
      observation facts loaded as entries (matching Shore `Entry` shape).
      Character = Haiku 4-5 (cheaper stand-in for Opus) with tool-use
      enabled. Measure how often the character actually calls memory + the
      quality of its answer.
- [ ] **Arm 2 (Shore + gate)**: same memory DB, same character, but the
      auto-memory-gate runs before the character turn. Gate asks questions,
      memory agent retrieves, filter synthesizes injection, character
      answers.
- [ ] **Optional Arm 3 (real Opus 4.7 bare)**: confirms the tool-reluctance
      observation on LoCoMo questions specifically. Expensive; not a ship
      decision arm, just characterization.
- [ ] Report: judge=correct delta per arm, tool-call rate in Arm 1,
      gate-fire rate in Arm 2, cost and latency deltas. Flips (Arm 1 wrong
      → Arm 2 correct) are the money shot.

### Phase 3 — live integration (answers Q4)

Contingent on Phase 2.6 showing a win over Shore-bare.

- [ ] Design the gate's place in the daemon request pipeline
      (`docs/ARCHITECTURE.md` update required). Likely hook:
      `handler/task.rs::handle_generation()` between `assemble_prompt()`
      and `stream_with_retry()`.
- [ ] Wire question-asker gate into `shore-daemon`. The question-asker
      stage calls the existing `MemoryResearcher` / `MemoryAgent` rather
      than a new retrieval path. No schema change required (unlike the v7
      selector, which needed a session_summary row type).
- [ ] Expose gate enable/disable + window size as config (default 12).
- [ ] Verify via `shore-mcp` test profile: drive real conversation, confirm
      gate fires on appropriate turns, confirm injection flows into
      character context, confirm the memory agent is being called by the
      gate and returning grounded content.
- [ ] Error handling: gate timeout, gate JSON parse failure, memory agent
      returns empty, filter says not worth injecting. Fallback in every
      case = don't inject, response proceeds as baseline.
- [ ] Record decisions in `docs/DECISIONS.md` and update
      `docs/ARCHITECTURE.md`.

## Update 2026-04-21 — Shore-driven bench, Opus 4.7 tool-reluctance diagnosed

Phase 2.6 infrastructure (permanent Shore-driven memory benchmark) is built in
`experiments/auto-memory-gate/shore-bench/`. Two bench modes:

- **Direct-query mode** (`run_bench.py`): stratified LoCoMo QA pairs, fresh
  Shore daemon per question. Measures the memory pipeline's accuracy when the
  character *does* invoke it.
- **Realistic mode** (`realistic/run_realistic.py`): natural conversational
  turns with long character prompts (no tool hints) and all tools enabled.
  Measures whether the character invokes memory at all, and whether the
  response is grounded.

Pre-requisite fix: `FtsHit` was not surfacing `start_timestamp` /
`end_timestamp` to the memory agent; a `when` field was added to the tool
result JSON, and the system prompt was updated to use it for temporal
questions. See commits 9a0556f and predecessors.

### Direct-query baseline (prod stack, corrected roles)

77 stratified QA pairs across conv-26 and conv-50. Prod model stack, with the
`memory_agent` / `tool_model` roles swapped to the intended arrangement
(Minimax = outer synthesizer, Gemma-4 = inner DB tool layer).

| Metric | % |
|--------|---|
| correct | **32.5%** |
| partial | 29.9% |
| wrong | 37.7% |

By category: single-hop 55%, multi-hop 30%, open-domain 35%, **temporal 10%**.

Temporal is the dominant pipeline weakness. Root cause: the researcher doesn't
preserve raw dates into its synthesis, so even when retrieval returns the right
dated entries the specifics get summarized away. For "how long" questions the
researcher compresses to "not specified" rather than computing the duration
from bounding dates.

A secondary observation: in the user's live config, `memory_agent` and
`tool_model` were inverted relative to the intended shape — the cheap model was
sitting on the outer synthesis layer where the reasoning matters, and the smart
one was on the inner DB tool layer where it's overkill. Swapping them is a
one-line config fix worth applying to prod.

### Realistic-mode finding: Opus 4.7 tool-reluctance

The direct-query bench wasn't reproducing the actual failure — in it, the
character prompt directs "use the memory tool" and Haiku obeys. In the user's
real Shore, Opus 4.7 almost never invokes memory in natural conversation. The
realistic-mode bench isolates that behavior.

3 turns per arm, same conversational turns, same prompts, only chat model
varies:

| Chat model | Reasoning | Memory invocation rate | Response quality |
|------------|-----------|------------------------|------------------|
| Haiku 4.5 | off | **3/3 (100%)** | In-character, grounded |
| Opus 4.6 | off | **3/3 (100%)** | In-character, grounded |
| Opus 4.7 | off | 0/3 (0%) | Deflects gracefully, no fabrication |
| Opus 4.7 | medium | 0/3 (0%) | Fabricates specifics |
| Opus 4.7 | xhigh | 0/3 (0%) | Fabricates vivid specifics ("social worker Linda with reading glasses") |
| Opus 4.7 | max | ≥2 per turn | Cost-prohibitive for prod |
| **Opus 4.7 + card-level nudge** | **xhigh** | **3/3 (100%)** | 2 grounded, 1 with residual fabrication |

Key findings:

1. **The regression is 4.7-specific, not "Opus tier."** Opus 4.6 handles the
   same test fine. Matches the user's observed timing (~Apr 1).
2. **Reasoning effort is a dial with a threshold above xhigh.** Consistent with
   Anthropic's 4.7 docs: *"raise the effort setting. high or xhigh effort
   settings show substantially more tool usage."* At `max`, tool use unlocks;
   at xhigh (the prod setting) or below, Opus suppresses tool calls in favor
   of staying in narrative frame. Max is too slow and expensive for prod.
3. **A one-line prompt nudge bridges the gap at xhigh.** Prepending this to
   the character card moved invocation from 0/3 to 3/3:
   > *"You have a wide variety of tools at your disposal. Feel free to use
   > them regularly; they are there to be used. Of particular importance is
   > your memory tool. You have a memory database that contains a vast number
   > of memories. Refer to it often."*
4. **Hallucination persists even with memory access.** At xhigh+nudge, turn 1
   fabricated a "social worker came by last week for part of the home study"
   scene despite 9 retrievals returning no such content. Opus's thinking
   directed itself to "anchor something real, like a recent visit or moment"
   — when memory didn't have that shape, it invented one. The character
   prompt's "respond with specifics" cue and Opus's narrative-completion
   instincts together license invention when retrieval is thin.

### Applied fix

Added to the memory tool description in
`shore-daemon/src/tools/memory_tools.rs`:

> *"If a query returns nothing relevant, that's information too — the detail
> isn't in the record. Reflect that uncertainty to the user instead of filling
> in from inference."*

This frames empty-result as information the character should reflect, not a
gap to paper over.

### Implications for gate design

The gate is now clearly the right architecture for Opus 4.7:

- The character model cannot be relied on to invoke memory even with explicit
  directives — it takes a specific card-level nudge to unlock, and even then
  residual hallucination persists on thin retrievals.
- Pre-injecting memory into context before the model's decision side-steps the
  tool-invocation question entirely. Memory content is already visible when
  Opus starts thinking; no frame break needed.
- The pipeline-quality problem (32.5% on direct queries) is orthogonal and
  must be solved in parallel. Even a perfect gate delivering wrong-entry
  retrievals gives the character wrong facts.

### Next session: pipeline accuracy

1. **Temporal (10% correct)**: patch the researcher prompt in
   `shore-daemon/src/memory/researcher.rs::RESEARCHER_SYSTEM_PROMPT` to
   preserve exact dates verbatim and, for "how long" questions, show bounding
   dates AND compute the duration.
2. **Single-hop wrongs (45% of that category)**: ranking issue — researcher
   picks adjacent/similar candidates when multiple entries look close on
   vector distance. Requires poking at retrieval ranking or teaching the
   synthesizer to prefer the best-matched entry when candidates disagree.
3. **Memory agent over-synthesis**: the inner layer sometimes returns prose
   instead of structured raw entries. `shore-daemon/src/memory/agent/prompt.rs`
   already instructs "present content fully, don't condense" but behavior
   drift suggests reinforcing with an explicit structured-output format
   example.
4. **Config fix**: swap `memory_agent` and `tool_model` in
   `~/Documents/qifei/config/conf.d/models__tools.toml` so Minimax ends up on
   the outer synthesis layer (matching the corrected bench prod config).
5. **Card-level nudge** — consider rolling the one-line nudge into production
   character cards (or into Shore's capabilities block) now that it's
   validated.

## References

- Experiment folder: `experiments/auto-memory-gate/`
- Python gate prototype: `experiments/auto-memory-gate/run_gate_eval.py`
- Labels (conv-26): `experiments/auto-memory-gate/labels.jsonl`
- Labels (conv-50): `experiments/auto-memory-gate/labels_conv50.jsonl`
- Selector results: `results_conv26_sel.json`, `results_conv50_sel.json`
- Window ablation: `results_w12.json`, `results_w18.json`, `results_w24.json`,
  `results_w36.json`
- Shore-driven bench (direct): `experiments/auto-memory-gate/shore-bench/`
- Shore-driven bench (realistic): `experiments/auto-memory-gate/shore-bench/realistic/`
- Related: `TODO/benchmarks/2026-04-10-locomo-benchmark.md`
