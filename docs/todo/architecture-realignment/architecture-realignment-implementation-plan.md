# Shore Architecture Realignment Implementation Plan

Companion to
[`architecture-realignment-plan.md`](./architecture-realignment-plan.md)
and the current
[`protocol-mismatch-checklist.md`](./protocol-mismatch-checklist.md).

This document translates the architecture realignment plan into an
implementation program that can be executed across multiple PRs without losing
behavioral control of Shore.

It is intentionally more concrete than the architecture plan:

- it names the crates and files that should move first
- it breaks the work into shippable phases
- it distinguishes internal cleanup from wire-contract changes
- it defines what to test before each phase is considered done

## Status Snapshot

Updated 2026-04-12 after the daemon-module-decomposition pass.

Current program state:

- Phase 0 is complete enough to treat as landed. `docs/ARCHITECTURE.md`, the
  mismatch checklist, and this plan now agree on the current SWP wire truth.
- Phase 1 is complete enough to treat as landed. Shore now has daemon-owned
  session state, per-session generation tracking, and explicit session-aware
  selected-character updates instead of relying on one daemon-global mutable
  path.
- Phase 2 is complete enough to treat as landed. Request-scoped command
  output, request errors, stream/tool traffic, image sends, phase updates, and
  cancellation results now route through per-session direct senders, while
  unsolicited state-change events still use broadcast until Phase 4 defines the
  authoritative revisioned sync model.
- Phase 3 is complete enough to treat as landed. Handshake payloads now use
  real daemon state, selected-character/session snapshots are truthful, TCP
  keepalive `ping` is implemented, and character switching now pushes an
  authoritative session snapshot instead of relying on reconnect or repair
  fetches.
- Phase 4 is complete enough to treat as landed. `History` is now revisioned
  and authoritative, `NewMessage` is explicitly advisory, and shared client
  code drops stale snapshots/events instead of guessing.
- Phase 5 is complete enough to treat as landed. The TUI no longer depends on
  connect-time bootstrap fetches, post-stream/delete repair fetches, or
  timestamp-based `NewMessage` dedupe to remain correct under normal flows.
- Phase 6 is complete enough to treat as landed. The highest-churn daemon
  orchestration surfaces are now split along clearer session/request/command
  boundaries instead of living in monolithic `handler/mod.rs` and
  `commands/state.rs` files.
- Phase 7 is still ahead of us in structural/process terms.

What landed on this branch:

- `shore-daemon-server` now owns a `SessionRouter` with per-session direct
  senders alongside the existing event broadcast path.
- `shore-daemon` handler state now keeps mutable session-owned values in a
  session map instead of one shared generation/cancellation slot.
- request-driven daemon responses now default to direct session delivery for:
  - command output and request-scoped errors
  - LLM stream messages
  - tool call/result events and image sends
  - request-triggered phase/status updates
  - cancellation outcomes
- multi-client regression coverage now proves the non-requesting client does
  not receive request-scoped command, stream, tool, or cancel traffic.
- `shore-daemon-server` now has a direct-delivery isolation test alongside the
  existing broadcast coverage.
- `switch_character` no longer returns reconnect-oriented command semantics;
  successful switches update session-owned selected character state
  authoritatively on the server side.
- CLI character switching now actually sends the daemon command before updating
  local state.
- `shore-daemon/src/handler/mod.rs` is now reduced to session lifecycle and
  main loop orchestration, with command dispatch, generation task execution,
  and test scaffolding split into focused submodules.
- `shore-daemon/src/commands/state.rs` is now split into command-family
  modules for status/diagnostics, model selection, config handling, and memory
  workflows, with tests moved alongside the new boundary.

Next pickup point:

1. start Phase 7 by turning the current protocol/state-ownership rules into
   durable guardrails in docs, tests, and workflow policy
2. keep any remaining UI fetches categorized as explicit UX requests rather
   than silent correctness repair logic
3. treat any future SWP wire change as a docs/types/tests/integration bundle,
   not as an implementation-only patch

## 1. Scope

This implementation plan covers every workstream in the architecture
realignment plan:

- Workstream A: write down the truth before changing it
- Workstream B: introduce explicit session and request types
- Workstream C: split direct responses from broadcast events
- Workstream D: make handshake and session mutation truthful
- Workstream E: choose and enforce one state synchronization model
- Workstream F: decompose oversized daemon modules around real boundaries
- Workstream G: bring client behavior back into the protocol
- Workstream H: install architectural guardrails

This plan assumes the "Architecture Realignment PR 1" work already landed or is
the baseline branch:

- `shore-daemon-server` already has internal `ClientId`, `SessionId`, and
  `RequestMeta`
- routed messages already preserve `rid` and selected character internally
- the public SWP wire contract is not yet updated to reflect the final target

## 2. Program Goals

The implementation should deliver the following concrete outcomes:

1. docs and protocol types describe the same system
2. every request is traceable by client, session, request, and character
3. request-scoped responses stop using the global broadcast path
4. handshake and `switch_character` semantics become truthful
5. state synchronization has one authoritative model with revisioning
6. TUI and CLI workaround logic is removed or reclassified as real UX logic
7. oversized daemon files are split around ownership boundaries
8. future protocol drift is blocked by tests and review guardrails

## 3. Affected Areas

Primary code and doc touchpoints for this program:

- `docs/ARCHITECTURE.md`
- `docs/todo/architecture-realignment/architecture-realignment-plan.md`
- `docs/todo/architecture-realignment/protocol-mismatch-checklist.md`
- `shore-protocol/src/client_msg.rs`
- `shore-protocol/src/server_msg.rs`
- `shore-protocol/src/types.rs`
- `shore-protocol/tests/golden_json.rs`
- `shore-daemon-server/src/lib.rs`
- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/handler/generation.rs`
- `shore-daemon/src/handler/persistence.rs`
- `shore-daemon/src/commands/mod.rs`
- `shore-daemon/src/commands/navigation.rs`
- `shore-daemon/src/commands/state.rs`
- `shore-client/src/connection.rs`
- `shore-client/src/stream.rs`
- `shore-cli/src/run.rs`
- `shore-cli/src/state.rs`
- `shore-tui/src/main.rs`
- `shore-daemon/tests/e2e.rs`
- `shore-daemon/tests/integration_concurrency.rs`
- `shore-daemon/tests/integration_message_integrity.rs`

Secondary touchpoints likely to change once session ownership is made explicit:

- `shore-daemon/src/lib.rs`
- `shore-daemon/src/main.rs`
- `shore-daemon/src/notifications.rs`
- `shore-daemon/src/autonomy/manager.rs`
- `shore-test-harness/src/*`

## 4. Cross-Cutting Rules

These rules apply to every phase.

### Rule 1: Preserve Working Behavior Until The Relevant Wire Phase

Internal refactors may change routing, state ownership, and module layout, but
they must not silently change the SWP contract before the protocol and client
phases are ready.

### Rule 2: Every Wire Change Must Move Four Things Together

For any user-visible protocol change, update in the same PR:

- `docs/ARCHITECTURE.md`
- `shore-protocol` structs and serde behavior
- protocol golden tests
- at least one integration test proving server and client behavior

### Rule 3: Session State Must Stop Hiding In Shared Command Context

Any mutable value that varies by requester should move out of daemon-global
shared state and into explicit session or request data.

Examples:

- selected character
- model override, if session-local
- in-flight generation bookkeeping
- client capability negotiation

### Rule 4: Remove Workarounds Only After The Server Is Authoritative

Do not delete TUI or CLI refresh logic until the corresponding daemon behavior
is both implemented and covered by tests.

### Rule 5: Prefer Small PRs With One Semantic Center

A phase may span several PRs. Each PR should have one dominant purpose:

- metadata plumbing
- response/event separation
- handshake truthfulness
- revisioned sync
- client cleanup
- module split

## 5. Delivery Structure

Recommended delivery shape:

- Phase 0: documentation truth pass
- Phase 1: session and request plumbing foundation
- Phase 2: outbound response/event separation
- Phase 3: truthful handshake and session mutation semantics
- Phase 4: revisioned authoritative state sync
- Phase 5: client cleanup against the new protocol
- Phase 6: daemon module decomposition
- Phase 7: guardrails and drift prevention

This is slightly more explicit than the architecture plan because it separates
"client cleanup" from the raw state-sync phase. In practice, this keeps the
wire work and the client-workaround deletion work from becoming one giant PR.

## 6. Detailed Implementation Plan

### Phase 0: Documentation Truth Pass

Purpose:

- establish the exact baseline behavior before making semantics changes

Primary outputs:

- updated `docs/todo/architecture-realignment/protocol-mismatch-checklist.md`
- targeted corrections in `docs/ARCHITECTURE.md`
- explicit SWP versioning policy note

Implementation steps:

1. Audit current SWP docs against:
   - `shore-protocol/src/client_msg.rs`
   - `shore-protocol/src/server_msg.rs`
   - `shore-daemon-server/src/lib.rs`
   - `shore-client/src/connection.rs`
2. For each mismatch, classify it as:
   - keep behavior, fix docs
   - keep docs, fix behavior later
   - compatibility quirk with temporary support window
3. Add a protocol status table to `docs/ARCHITECTURE.md` covering:
   - handshake payload truthfulness
   - `rid` propagation semantics
   - direct response versus event routing
   - `switch_character` behavior
   - snapshot/event authority model
   - `ping` status
4. Add a short SWP versioning rule:
   - internal-only refactors do not bump version
   - behavioral wire changes require either version bump or explicit capability
     negotiation

Tests and verification:

- extend `shore-protocol/tests/golden_json.rs` where docs describe real payload
  shapes today
- add or refresh handshake tests in `shore-daemon-server/src/lib.rs`
- ensure docs do not claim behavior that no test or code path implements

Exit criteria:

- the mismatch checklist is complete enough to drive the remaining phases
- the architecture doc clearly labels current truth versus intended future
  semantics

### Phase 1: Session And Request Plumbing Foundation

Status:

- Complete enough to treat as landed on 2026-04-12.
- Remaining future decision:
  - confirm the final intended scope of active model override when Decision 4
    in this document is made, but that does not block Phase 2 or Phase 3 work.

Purpose:

- finish the ownership cleanup needed before response routing changes

Primary targets:

- `shore-daemon-server/src/lib.rs`
- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/commands/mod.rs`
- `shore-daemon/src/commands/navigation.rs`

Implementation steps:

1. Formalize internal metadata types.
   Add or refine:
   - `ClientId`
   - `SessionId`
   - `RequestMeta`
   - a session-scoped state type, likely `SessionState`
   - a daemon-owned session registry, likely `SessionStore`
2. Move session-owned facts out of implicit/shared mutation.
   The first candidates are:
   - selected character
   - active model override if meant to be client/session local
   - in-flight request bookkeeping
3. Split daemon-global command services from per-session state.
   The likely end state is:
   - `CommandContext`: immutable or daemon-global shared services only
   - `SessionState`: mutable session-specific values
   - request-local effective config passed as data, not temporarily installed
     into shared context
4. Replace the current "single global generation handle" model.
   Introduce request-aware bookkeeping keyed at least by session, and ideally by
   request:
   - active generation task handle
   - cancellation target
   - latest selected character snapshot used for dispatch
5. Make navigation and model-switch commands explicitly consume session state
   rather than daemon-global mutable fields.

Recommended code shape:

- `shore-daemon-server`: owns connection/session registry and session IDs
- `shore-daemon`: owns semantic session state and request lifecycle
- commands and generation paths receive an explicit session reference or
  session-derived data instead of inferring it from shared globals

Tests and verification:

- server unit tests proving metadata survives routing intact
- daemon tests proving selected character and request metadata stay aligned
- tests for cancellation behavior when two sessions are connected
- regression tests ensuring session-local model changes do not leak globally

Exit criteria:

- there is one explicit place to find session state
- no command requires temporary mutation of daemon-global config to behave like
  a per-session action
- generation cancellation is no longer modeled as one daemon-global handle

### Phase 2: Outbound Response And Event Separation

Status:

- Complete enough to treat as landed on 2026-04-12.
- Already landed:
  - per-session direct response delivery in `shore-daemon-server`
  - direct routing for command output, request-scoped errors, stream/tool
    traffic, and cancellation outcomes
  - continued use of broadcast delivery only for unsolicited event-style
    messages
- Closeout added on this branch:
  - explicit two-client isolation coverage for command, stream, tool, and
    cancel behavior in `shore-daemon/tests/integration_concurrency.rs`
  - direct-send transport isolation coverage in `shore-daemon-server/src/lib.rs`
  - confirmation that request-triggered `Phase` messages, tool/image sends, and
    cancellation outcomes stay on direct session delivery
- Remaining intentional boundary:
  - `History` and `NewMessage` remain broadcast/event-style messages until
    Phase 4 defines the authoritative revisioned sync model

Purpose:

- stop using the same delivery path for direct responses and unsolicited events

Primary targets:

- `shore-daemon-server/src/lib.rs`
- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/handler/generation.rs`
- `shore-daemon/src/handler/persistence.rs`
- `shore-daemon/src/commands/mod.rs`

Implementation steps:

1. Introduce separate outbound abstractions.
   At minimum:
   - direct response sink keyed by session or request
   - event bus for unsolicited daemon state changes
2. Replace the current `broadcast::Sender<ServerMessage>`-centric API in the
   daemon handler with a more explicit emitter layer.
   Suggested internal concepts:
   - `emit_response(session_id, message)`
   - `emit_event(scope, message)`
3. Define event scopes.
   Minimum viable scope:
   - session-specific direct response
   - broadcast-to-all event
   Better end state:
   - session
   - character
   - global daemon
4. Re-route command outcomes and request-tied generation events.
   These should become direct responses by default:
   - `CommandOutput`
   - request-scoped `Error`
   - request-scoped stream messages
   - cancellation results
5. Keep unsolicited events on the event channel only.
   Candidates:
   - background autonomy messages
   - status-change notifications not caused by the requester
   - future registry or character lifecycle events
6. Add a temporary compatibility bridge if needed so old clients still function
   during transition, but keep it isolated and explicitly labeled as temporary.

Tests and verification:

- two-client routing tests in `shore-daemon-server`
- daemon integration tests proving one client's command result does not arrive
  in the other client
- stream/cancel tests with two active sessions
- lag handling tests to ensure event-bus backpressure does not break direct
  response delivery

Exit criteria:

- opening two clients no longer cross-delivers request-scoped responses
- direct-response plumbing is the default for request-driven messages
- event delivery is explicit and separately testable

### Phase 3: Truthful Handshake And Session Mutation Semantics

Status:

- Complete enough to treat as landed on 2026-04-12.
- Landed on this branch:
  - `shore-daemon-server` now sends real handshake payloads via a daemon-owned
    handshake provider instead of placeholder character/history data.
  - the initial `history` snapshot now carries authoritative
    `selected_character` data and a truthful session/config snapshot payload.
  - TCP keepalive `ping` is now emitted by the server instead of only existing
    in documentation.
  - successful `switch_character` commands now update session-owned selected
    character state and push an authoritative direct `history` snapshot for the
    new selection.
  - client connection flows can become coherent from the handshake alone; the
    TUI no longer needs a mandatory bootstrap `log`/`status` repair round-trip
    just to render the initial state.
- Remaining intentional boundary:
  - revisioned stale-state handling is still Phase 4 work; this phase makes the
    startup/session snapshot truthful but does not yet define the final
    revisioned sync contract.

Purpose:

- make connect-time state and session-changing commands authoritative

Primary targets:

- `shore-daemon-server/src/lib.rs`
- `shore-daemon/src/lib.rs`
- `shore-daemon/src/commands/navigation.rs`
- `shore-protocol/src/server_msg.rs`
- `shore-client/src/connection.rs`

Implementation steps:

1. Replace placeholder handshake generation.
   `ServerHello` and the initial state snapshot must come from real daemon
   state:
   - real character list
   - real selected character or explicit "none selected" state
   - truthful config/session snapshot fields
2. Decide and encode the session-selection model.
   Required decisions:
   - is character selection optional or mandatory at connect time
   - can a session change characters without reconnect
   - which parts of state survive character switching
3. Make `switch_character` truthful.
   Pick one model and implement it end-to-end:
   - mutate session state and return authoritative updated state
   - or require reconnect and state that clearly on the wire and in docs
   The architecture plan favors the first option.
4. Resolve the `ping` inconsistency.
   Choose one:
   - implement periodic transport `ping` in TCP mode
   - or remove `ping` from the protocol documentation and message contract
5. Extend handshake payloads only as needed to support truthful startup.
   Avoid speculative fields. Add only what clients actually need to become
   coherent without immediate repair fetches.

Recommended protocol direction:

- handshake returns one authoritative initial snapshot
- clients do not need to immediately send `log`, `status`, or `list_models`
  just to become usable

Tests and verification:

- handshake integration tests for empty and non-empty history
- handshake tests covering selected character presence or absence
- `switch_character` tests proving updated session semantics
- compatibility tests for existing CLI and TUI connection flows

Exit criteria:

- the initial handshake state is real, not placeholder
- `switch_character` has one clear semantic model
- clients can become coherent after connect without mandatory repair commands

### Phase 4: Revisioned Authoritative State Sync

Status:

- Complete enough to treat as landed on 2026-04-12.
- Landed on this branch:
  - `History` snapshots now carry a monotonic `revision`.
  - advisory `NewMessage` events now carry the same revision space and are no
    longer treated as overlapping authorities.
  - `shore-client` now owns shared stale-snapshot rejection via revision
    tracking instead of leaving that behavior to frontend-specific heuristics.
  - daemon history emitters now stamp revisioned authoritative snapshots for
    handshake, switch-character, edit/delete/reset/reload, and generation-path
    conversation mutations.
- Remaining intentional boundary:
  - request correlation on server messages is still the separate `rid` work;
    this phase only closes snapshot/event authority, not wire-level request
    echo semantics.

Purpose:

- replace the current mixed snapshot/event model with one authoritative sync
  model clients can trust

Primary targets:

- `shore-protocol/src/server_msg.rs`
- `shore-protocol/src/types.rs`
- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/handler/persistence.rs`
- `shore-client/src/stream.rs`
- `shore-tui/src/main.rs`

Implementation steps:

1. Define the authoritative state object on the wire.
   Recommended direction from the architecture plan:
   - revisioned snapshots are authoritative
   - direct responses carry request correlation
   - events are either advisory or also revisioned
2. Add revision metadata.
   Candidate locations:
   - top-level `History` snapshot
   - any server event that mutates client-visible state
   - command responses that return authoritative state
3. Decide which message families stay in the final contract.
   Make explicit rules for:
   - `History`
   - `NewMessage`
   - streaming messages
   - command outputs that mutate UI-visible state
4. Implement stale-state rejection in the shared client layer.
   `shore-client` should become responsible for:
   - tracking latest revision
   - dropping stale snapshots/events
   - surfacing revision mismatches clearly
5. Update daemon emitters so every state-changing operation consistently emits
   either:
   - a new authoritative snapshot
   - or a revisioned incremental event under the documented rules
6. Update persistence and background paths to participate in the same revision
   scheme, including autonomy-generated changes.

Wire-contract guidance:

- do not keep both `History` and `NewMessage` as overlapping authorities
- if `NewMessage` remains, it must either:
   - be purely advisory and never overwrite authoritative state
   - or carry revision/ordering information clients can trust

Tests and verification:

- protocol golden tests for revisioned payloads
- integration tests covering:
  - normal generation flow
  - regen flow
  - command-driven state changes
  - background autonomy messages
  - stale snapshot rejection
- multi-client tests proving one lagging client can recover via authoritative
  snapshots without duplicating messages

Exit criteria:

- clients do not need blind `log` refreshes after normal operations
- the authoritative snapshot/event rules are documented and test-backed
- stale state can be detected rather than guessed at

### Phase 5: Client Cleanup Against The New Protocol

Status:

- Complete enough to treat as landed on 2026-04-12.
- Landed on this branch:
  - TUI connect-time bootstrap fetches are gone; handshake state is sufficient
    for coherent startup.
  - normal stream completion and delete flows no longer trigger blind `log`
    repair fetches.
  - the old timestamp-based `NewMessage` dedupe path has been removed in favor
    of revisioned shared-client sync rules.
  - client-side protocol state management now lives more squarely in
    `shore-client` rather than bespoke TUI recovery logic.
- Remaining intentional boundary:
  - explicit user-driven commands like `status`, `log`, and `list_models`
    remain as normal UX operations; they are no longer required for hidden
    correctness repair.

Purpose:

- remove client-side repair logic that existed only because the daemon was not
  authoritative

Primary targets:

- `shore-client/src/connection.rs`
- `shore-client/src/stream.rs`
- `shore-tui/src/main.rs`
- `shore-cli/src/run.rs`
- `shore-cli/src/state.rs`

Implementation steps:

1. Remove connect-time repair fetches that are no longer required.
   Current TUI candidates include immediate `log`, `status`, and `list_models`
   requests after connect.
2. Remove stale-history recovery fetches after stream completion or cancel
   where the server now emits authoritative state directly.
3. Delete or simplify dedupe heuristics around overlapping `History` and
   `NewMessage` handling.
4. Move any remaining local behavior into one of two categories:
   - genuine UX enhancement
   - bug to fix server-side
5. Consolidate client-side protocol state management in `shore-client` where
   possible so CLI, TUI, and future bridges do not each reimplement protocol
   semantics.

Specific cleanup candidates already visible in the codebase:

- TUI post-connect bootstrap commands in `shore-tui/src/main.rs`
- TUI post-stream `log` refreshes
- TUI `NewMessage` dedupe logic tied to timestamp heuristics
- any CLI reconnect-oriented `switch_character` assumptions

Tests and verification:

- client integration tests for connect, stream, cancel, switch character, and
  reconnect paths
- TUI/CLI smoke tests through the shared client layer
- regression tests ensuring no duplicate entries after normal message flow

Exit criteria:

- CLI and TUI no longer depend on hidden daemon quirks to stay correct
- new clients can build on `shore-client` plus docs rather than reverse
  engineering TUI behavior

### Phase 6: Daemon Module Decomposition

Status:

- Complete enough to treat as landed on 2026-04-12.
- Landed on this branch:
  - `shore-daemon/src/handler/mod.rs` now focuses on session lifecycle and the
    main routed-message loop, while command dispatch and generation task logic
    live in dedicated submodules.
  - `shore-daemon/src/commands/state.rs` is replaced by a command-family module
    tree (`status`, `models`, `config`, `memory`) so state-command ownership is
    visible in the layout instead of hidden in one 2k-line file.
  - daemon unit coverage for the split boundaries moved with the new modules,
    so the refactor did not rely on a giant legacy test appendix to stay safe.
- Remaining intentional boundary:
  - some other oversized daemon internals still exist, but the main
    session/request/response orchestration surfaces called out by this plan are
    no longer monoliths.

Purpose:

- split large modules after ownership boundaries are clear

Primary targets from the architecture plan:

- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/commands/state.rs`
- `shore-daemon/src/autonomy/manager.rs`
- `shore-daemon/src/memory/collation/mod.rs`
- `shore-daemon/src/engine/prompt.rs`

Implementation steps:

1. Split `handler/mod.rs` by semantic responsibility, not line count.
   Likely submodules:
   - session routing and request lifecycle
   - direct response emission
   - event emission
   - generation orchestration
   - persistence and notification fan-out
2. Split `commands/state.rs` into command families.
   Suggested slices:
   - model commands
   - diagnostics/status commands
   - memory shell commands
   - compaction/collation commands
   - config commands
3. Split `autonomy/manager.rs` around long-lived concerns.
   Suggested slices:
   - scheduler loop
   - persistent state
   - tick execution
   - cache keepalive
   - event logging and history emission
4. Split memory collation and prompt-building modules only after the protocol
   work stops actively changing their call sites.
5. Introduce small ownership notes at module boundaries where helpful.

Non-goal:

- do not disguise semantic ambiguity as "module decomposition"
- if a file is large because responsibilities are still mixed, clarify the
  ownership boundary first and split second

Tests and verification:

- existing integration suites continue to pass
- any new submodule gets focused unit tests for its ownership boundary
- no module split should reintroduce global/session/request ambiguity

Exit criteria:

- the main daemon orchestration files are split along real architectural lines
- the code layout matches the session/request/response architecture introduced
  in earlier phases

### Phase 7: Guardrails And Drift Prevention

Purpose:

- make the realigned architecture self-reinforcing instead of aspirational

Primary targets:

- `docs/ARCHITECTURE.md`
- `shore-protocol/tests/golden_json.rs`
- `shore-daemon/tests/*`
- CI workflow files under `.gitea/workflows`

Implementation steps:

1. Add protocol conformance gates.
   Minimum:
   - golden JSON coverage for new wire types
   - handshake and routing integration tests
   - multi-client direct-response tests
2. Add a short state-ownership note.
   It should define:
   - daemon-global state
   - session state
   - character state
   - request-local state
3. Add review guidance for large mixed-responsibility files.
   This can be light-weight:
   - a note in docs
   - a PR checklist item
   - optional size-budget reporting later
4. Clarify `docs/QUIRKS.md` usage.
   Keep it for unavoidable external quirks, not protocol debt or architectural
   mismatches.
5. Add a release/checklist expectation for any future SWP change:
   - docs updated
   - golden tests updated
   - server/client integration updated

Exit criteria:

- future protocol drift requires deliberate test breakage to land
- new contributors can tell where state is supposed to live
- architectural cleanup is preserved by process, not memory

## 7. Recommended PR Breakdown

One reasonable breakdown for landing this without a rewrite:

1. PR A: expand mismatch checklist and document current protocol truth
2. PR B: introduce or finish `SessionState` / session store plumbing
3. PR C: move model override and selected-character ownership into session
4. PR D: replace global generation handle with session/request-aware tracking
5. PR E: add direct-response sink alongside existing event broadcast
6. PR F: route command results and request-scoped errors through direct responses
7. PR G: route streaming and cancellation through direct responses
8. PR H: implement truthful handshake payload generation
9. PR I: make `switch_character` mutate session state authoritatively
10. PR J: implement or remove transport `ping`
11. PR K: add revisioned authoritative sync types and tests
12. PR L: migrate TUI, CLI, and `shore-client` to the new sync model
13. PR M: remove obsolete client repair logic
14. PR N: decompose oversized modules after semantics stabilize
15. PR O: add guardrails in docs, tests, and CI

This list can compress if some phases land cleanly together, but it should not
be expanded into an unbounded branch. Keep each PR reviewable.

## 8. Test Strategy By Layer

### Protocol Layer

Use `shore-protocol/tests/golden_json.rs` for:

- handshake payloads
- revisioned snapshot payloads
- direct response payloads if new envelopes are introduced
- `switch_character` result payloads

### Server Layer

Use `shore-daemon-server/src/lib.rs` tests for:

- handshake ordering
- session metadata preservation
- direct-response versus event routing
- lag handling
- multi-client isolation

### Daemon Integration Layer

Use `shore-daemon/tests/*` for:

- generation flow with request identity
- cancellation scoped to the right request/session
- character switching semantics
- authoritative state sync after edits, deletes, and new messages
- autonomy-generated updates under the same state model

### Client Layer

Use `shore-client`, CLI, and TUI tests for:

- coherent startup from handshake alone
- no duplicate message rendering
- revision tracking and stale snapshot rejection
- no repair fetch requirement after normal operations

## 9. Major Decisions To Make Early

These decisions block the middle phases and should be made before Phase 3 grows:

### Decision 1: How SWP Evolves

Choose one:

- bump from `SWP_V1` when the behavioral contract changes
- keep `SWP_V1` and add explicit capability negotiation for new semantics

Recommended:

- use capability negotiation for additive transition periods
- use a version bump if message meanings materially change or old clients would
  misinterpret new behavior

### Decision 2: Authoritative State Form

Choose one:

- revisioned full snapshots as authority, events advisory
- revisioned incremental events plus periodic snapshots

Recommended:

- revisioned snapshots as authority
- direct responses for request-scoped results
- optional events for fast UI updates, but never as undocumented second truth

### Decision 3: `switch_character` Contract

Choose one:

- session mutation without reconnect
- reconnect-required workflow

Recommended:

- session mutation without reconnect, because it matches the goal that new
  clients should be boring and should not need reconnect conventions

### Decision 4: Scope Of Model Override

Choose one:

- daemon-global
- per-character
- per-session
- per-request only

This must be made explicit before finishing the session-state refactor.

## 10. Risks And Mitigations

### Risk A: Compatibility Drift During Transition

Mitigation:

- isolate temporary compatibility behavior behind explicit adapter code
- record each temporary lie in `protocol-mismatch-checklist.md`
- remove the adapter in the next planned phase, not "later"

### Risk B: Multi-Client Regressions

Mitigation:

- add two-client routing tests before changing delivery paths
- require multi-client integration coverage for direct responses and cancel

### Risk C: Client Cleanup Runs Ahead Of Server Truthfulness

Mitigation:

- keep repair logic until authoritative server behavior is proven
- delete workaround code only in PRs that add the replacement tests

### Risk D: Module Splitting Becomes Cosmetic

Mitigation:

- defer most decomposition until session/request/response ownership is stable
- split only when the target boundary is conceptually clear

## 11. Definition Of Done

This program is complete when all of the following are true:

- `docs/ARCHITECTURE.md` describes the real wire contract
- request identity survives end-to-end and is visible in outbound responses
- request-scoped results no longer travel through the global broadcast path
- the handshake is truthful and sufficient for coherent startup
- `switch_character` has one documented authoritative behavior
- the state-sync model is revisioned and explicit
- CLI and TUI no longer depend on repair fetches or dedupe heuristics for
  normal correctness
- the main daemon orchestration files are split along real ownership lines
- protocol conformance tests make future drift difficult to reintroduce

## 12. Suggested Immediate Next Step

The next implementation PR after the current branch should pick up in Phase 7,
not Phase 1:

1. add explicit guardrails for protocol drift, state ownership, and large
   mixed-responsibility files
2. keep future SWP changes bundled with docs, golden tests, and integration
   coverage from the start
