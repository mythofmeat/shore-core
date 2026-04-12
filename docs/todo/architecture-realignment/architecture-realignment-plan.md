# Shore Architecture Realignment Plan

This plan is for a deeper refactor than the earlier hardening pass that focused
on specific implementation risks.
This one is about restoring Shore's original architectural intent so the codebase
becomes easier to reason about, iterate on, and extend.

## Status Snapshot

Updated 2026-04-13 after the request-correlation closeout pass.

- Workstream A is complete enough to treat as landed. `docs/ARCHITECTURE.md`,
  the mismatch checklist, and the implementation plan now describe the current
  SWP truth instead of the pre-refactor intent.
- Workstream B is complete enough to treat as landed. Shore now has explicit
  daemon-owned session state and per-session request/generation bookkeeping.
- Workstream C is complete enough to treat as landed. Main request-scoped
  responses now use per-session direct delivery with explicit multi-client
  isolation coverage, while unsolicited events still use the broadcast path.
- Workstream D is complete enough to treat as landed. Handshake payloads now
  come from real daemon state, `switch_character` mutates session state
  authoritatively without reconnect semantics, and TCP keepalive `ping` is now
  implemented instead of merely documented.
- Workstream E is complete enough to treat as landed. Shore now has a
  revisioned authoritative `History` snapshot model with shared-client stale
  snapshot rejection instead of ad hoc repair fetches.
- Workstream G is complete enough to treat as landed. The TUI no longer depends
  on hidden bootstrap or post-stream/delete repair fetches to stay correct in
  normal flows.
- Workstream F is complete enough to treat as landed. The main daemon
  orchestration surfaces now split session/request routing and state-command
  families into focused modules instead of one large handler/state file pair.
- Workstream H is complete enough to treat as landed. Shore now documents state
  ownership directly, scopes `docs/QUIRKS.md` to real external quirks, and runs
  protocol/routing guardrail suites in CI.
- The last explicitly open wire debt from the earlier passes is now closed:
  request-scoped SWP V1 server responses echo `rid`, while handshake and
  unsolicited push messages intentionally omit it.
- For the exact pickup point and phase-by-phase execution status, use
  [`architecture-realignment-implementation-plan.md`](./architecture-realignment-implementation-plan.md).

## 1. Why This Exists

Shore's implementation has drifted away from its stated goals.

The architecture document says Shore should have:

1. discrete, modular services with hard boundaries and a formalized wire protocol
2. services small enough for an LLM to fully comprehend
3. independent binaries with clear responsibilities
4. a protocol that clients can trust instead of reverse-engineering

Today, some of the hardest iteration pain is not coming from one bug or one
slow path. It is coming from architectural drift:

- the docs describe one system
- the protocol structs implement a looser system
- the server routes messages using a third mental model
- the TUI and CLI carry client-side workaround logic to compensate

If we keep fixing symptoms locally, Shore will get harder to evolve every time
we add a feature or a new client.

## 2. Restated Project Goals

These are the goals that should drive this refactor.

### Goal A: Truthful Boundaries

Every layer should have one clear job:

- transport accepts connections, frames JSON lines, applies backpressure
- protocol defines the actual wire contract
- daemon owns domain semantics
- clients render state and send intents

No layer should fake or infer state that belongs to another layer.

### Goal B: Explicit State Ownership

Every mutable state item should belong to exactly one scope:

- global daemon state
- per-character state
- per-session state
- per-request state

If the owner is unclear, clients and handlers start compensating for each other.

### Goal C: Executable Protocol, Not Aspirational Docs

The docs must describe the real wire contract, not the intended one from a
week ago. If the protocol changes, the docs, golden JSON tests, and transport
behavior tests must all move together.

### Goal D: Small, Comprehensible Modules

Shore explicitly wanted small crates and modules. The current size drift makes
architectural reasoning harder than it should be:

- `shore-daemon`: ~32k LOC
- `shore-llm-client`: ~8.5k LOC
- `shore-tui`: ~6.1k LOC
- `shore-cli`: ~5.5k LOC
- `shore-daemon/src/autonomy/manager.rs`: 2412 LOC
- `shore-daemon/src/commands/state.rs`: 2030 LOC
- `shore-daemon/src/memory/collation/mod.rs`: 1994 LOC
- `shore-daemon/src/handler/mod.rs`: 1435 LOC

This is not just a style issue. It makes protocol drift and hidden coupling
much easier to introduce.

### Goal E: New Clients Should Be Boring

A new frontend such as a Neovim client should only need to implement the
documented protocol and a UI. It should not need to rediscover undocumented
session semantics, stale-state workarounds, or global-broadcast quirks.

## 3. Core Diagnosis

These are the root problems this refactor should solve.

### Problem 1: The Protocol Contract Is Not Truthful

The architecture docs currently imply behavior that the implementation does not
actually provide.

Examples:

- request IDs are described as end-to-end correlation, but server messages do
  not actually carry `rid`
- the docs describe a flatter message shape than the actual `content_blocks`
  model now on the wire
- the docs still need to be explicit that request-scoped direct responses and
  authoritative snapshots are distinct concepts in the live implementation

This makes the docs unreliable as a foundation for implementation work.

### Problem 2: Client Identity Is Dropped Too Early

Once a client message leaves the TCP handler, Shore mostly routes it as
"message for this character" instead of "request from this client/session with
this request ID."

That is the root cause behind several downstream problems:

- direct responses are pushed through the same broadcast path as unsolicited events
- command results are not naturally scoped to the requesting client
- generation cancellation is global rather than request-aware
- concurrency and multiplexing are much harder than the docs imply

This is the single most important architectural issue to fix.

### Problem 3: State Ownership Is Blurred

Several pieces of state currently live in the wrong place or are mutated through
shared context in ways that obscure ownership.

Examples:

- active model override is daemon-global in shared command state
- effective config is swapped into shared command context for a request, then
  swapped back out afterward
- selected character partly lives in the connection, partly in the command
  layer, and partly in client-local conventions
- generation lifecycle is tracked as one global handle instead of per-session
  or per-character request state

This makes behavior feel magical instead of explicit.

### Problem 4: The Snapshot/Event Contract Needed To Become Explicit

This branch closes the worst of the ambiguity by making:

- revisioned `History` snapshots authoritative
- `NewMessage` explicitly advisory
- shared-client revision tracking responsible for stale-state rejection

The remaining job is no longer to invent the model; it is to preserve it with
tests, docs, and review guardrails so Shore does not drift back into
overlapping authorities.

### Problem 5: The Clients Still Know Too Much

The truthful-handshake gap is now closed, but clients still carry too much
compensating protocol logic relative to what the server guarantees
authoritatively.

Meanwhile clients carry compensating logic for:

- post-connect state bootstrap
- character-switch refresh behavior
- stale history defense
- response deduplication

That is backwards. The daemon should own the semantics; clients should not have
to repair them.

### Problem 6: Oversized Modules Hide Architectural Drift

This branch has now split the worst of the handler/state-command monoliths, but
oversized files still matter because they encourage orchestration, policy,
transport semantics, and domain logic to collapse back into one place.

That makes it too easy to:

- add "just one more special case"
- patch around a behavior in the client
- blur global/session/character/request concerns
- update implementation without updating docs

The size problem is not separate from the protocol problem. It is one reason
the protocol problem kept recurring, and why Workstream H still needs
guardrails to preserve the new boundaries.

## 4. North Star Architecture

This is the target shape the refactor should move toward.

### 4.1 Transport Owns Connections, Not Semantics

The TCP layer should own:

- accept loop
- framing
- backpressure and lag handling
- connection lifecycle
- client registry

It should not regress into inventing fake character lists or placeholder
history snapshots.

### 4.2 Daemon Owns Sessions

Introduce an explicit session concept.

A session should hold:

- client identity
- negotiated capabilities
- selected character
- session-local model override, if model override is intended to be session-local
- any in-flight request bookkeeping

If Shore wants some settings to be character-global instead, that should be an
intentional domain decision, not an accident of shared mutable context.

### 4.3 Requests Must Preserve Identity End-To-End

Every incoming client request should carry request metadata through the full
stack:

- client/session ID
- request ID (`rid`)
- selected character at the time of dispatch
- request kind

This metadata should survive all the way to outbound responses.

### 4.4 Direct Responses And Broadcast Events Must Be Separate Concepts

Shore needs two distinct outbound channels:

- direct responses: sent only to the requesting client/session
- broadcast or subscription events: sent to interested sessions because state
  changed independently of their request

Do not tunnel both through one global broadcast sender.

### 4.5 One Authoritative State-Sync Model

Pick a model and make it explicit.

Recommended direction:

- handshake returns truthful initial state
- requests return direct response messages tied to `rid`
- unsolicited changes are sent as explicit events
- snapshots include monotonically increasing revision numbers so clients can
  reject stale state

If snapshots remain the main authority, then event messages should be advisory
or should also carry revision information. Clients should never need blind
`log` refreshes to make the model reliable.

### 4.6 The Protocol Spec Must Be Enforced By Tests

The protocol docs should become an executable contract through:

- golden JSON tests
- handshake integration tests
- multi-client routing tests
- cancellation tests
- switch-character/session-state tests

No feature should be considered complete if the docs describe a behavior that
the tests do not verify.

## 5. Refactor Principles

These principles should constrain the work.

### Principle 1: Fix Root Ownership, Not Client Symptoms

If a client currently needs to send an extra `log` or `status` request to stay
coherent, the default assumption should be that the protocol is incomplete.

### Principle 2: Avoid Silent Compatibility Lies

If the wire contract changes materially, bump the protocol version or introduce
an explicit capability negotiation path. Do not silently keep `SWP_V1` while
changing the behavioral meaning underneath it.

### Principle 3: Separate Internal Cleanup From Wire Changes

Not every internal refactor needs a wire change. But every wire change should
be isolated, documented, and tested as such.

### Principle 4: Keep Phases Small And Validatable

Do not attempt a one-shot rewrite. Land phases that improve internal structure
while preserving working behavior, then change semantics once the new ownership
boundaries exist.

### Principle 5: Add Guardrails So Drift Does Not Reappear

This plan should end with enforcement, not just cleaned-up code:

- spec conformance tests
- architectural notes
- size-budget monitoring or at least oversized-module review gates
- explicit ownership docs for new state

## 6. Proposed Workstreams

### Workstream A: Write Down The Truth Before Changing It

Goal:

- produce a source-of-truth matrix for "documented behavior vs actual behavior"

Actions:

- audit SWP docs against protocol structs and transport behavior
- list all known mismatches
- decide case-by-case whether to fix code to match docs or fix docs to match code
- document which behaviors are intentional compatibility quirks and which are bugs

Deliverables:

- updated architecture notes
- protocol mismatch checklist
- explicit versioning policy for SWP

Why first:

- until the team agrees on what is supposed to be true, every refactor risks
  preserving the wrong behavior

### Workstream B: Introduce Explicit Session And Request Types

Goal:

- stop losing identity and ownership information at the transport boundary

Actions:

- add explicit `SessionId`, `RequestId`, and request metadata types
- make routed messages carry client/session/request identity
- define session-local state instead of smuggling it through shared command context
- stop mutating shared config state per request

Recommended end state:

- `CommandContext` becomes mostly daemon-global immutable/shared services
- per-session mutable state moves into session storage
- per-request effective config is passed as data, not temporarily swapped into
  shared handler state

Why this is foundational:

- most protocol weirdness downstream comes from not preserving identity here

### Workstream C: Split Direct Responses From Broadcast Events

Goal:

- replace the current "everything goes through broadcast" model

Actions:

- introduce an outbound response sink for direct request responses
- keep a separate event bus for unsolicited state changes
- scope subscriptions at least by session, and ideally by character where appropriate
- make command output, errors, and stream chunks direct responses by default

Optional extension:

- allow sessions to subscribe only to relevant character events rather than all
  daemon-wide traffic

Exit criteria:

- opening two clients should no longer cause one client's command responses to
  appear in the other unless explicitly intended

### Workstream D: Make Handshake And Session Mutation Truthful

Goal:

- remove placeholder handshake behavior and ambiguous character-switch semantics

Actions:

- handshake must be generated from real registry/session state
- if character selection is ambiguous, model it explicitly in the protocol
- `switch_character` must have one clear behavior:
  - either mutate session state and return the new authoritative snapshot
  - or require reconnect and not pretend otherwise
- if `ping` is part of the spec, implement it; otherwise remove it from the spec

Exit criteria:

- clients can connect and become coherent without immediate repair commands

### Workstream E: Choose And Enforce One State Synchronization Model

Goal:

- eliminate stale-state workarounds and dedupe heuristics in clients

Actions:

- decide which messages are authoritative snapshots and which are incremental events
- add revision numbers or sequence counters to authoritative state
- ensure all mutations emit state in the chosen form consistently
- remove the need for clients to blindly call `log` after stream completion or
  character switching

Recommended direction:

- revisioned snapshots plus explicit direct responses

Why:

- this is the cleanest way to preserve client simplicity while still allowing
  push updates

### Workstream F: Decompose Oversized Daemon Modules Around Real Boundaries

Goal:

- make the system small enough to reason about again

Priority targets:

- `shore-daemon/src/handler/mod.rs`
- `shore-daemon/src/commands/state.rs`
- `shore-daemon/src/autonomy/manager.rs`
- `shore-daemon/src/memory/collation/mod.rs`
- `shore-daemon/src/engine/prompt.rs`

Split by responsibility, not arbitrary file size.

Examples:

- handler: session routing, request dispatch, generation lifecycle, outbound emission
- commands/state: model commands, memory commands, diagnostics/status, config commands
- autonomy manager: scheduling, persistent state, tick execution, event logging

Guardrail:

- a split is only good if it clarifies ownership and reduces coupling

### Workstream G: Bring Client Behavior Back Into The Protocol

Goal:

- remove client-local workaround logic that exists only because the daemon is
  underspecified

Actions:

- audit TUI and CLI for recovery/refresh/dedupe behaviors
- classify each one as:
  - legitimate client UX
  - protocol gap to close in the daemon
- move protocol-gap behaviors back into daemon/session semantics

Examples to revisit:

- extra `log` and `status` fetches after connect or character switch
- stale-history recovery after stream end
- dedupe logic for overlapping `History` and `NewMessage`
- local character-switch state conventions in CLI

### Workstream H: Install Architectural Guardrails

Goal:

- prevent future drift

Actions:

- require doc updates for SWP changes
- add transport conformance tests to CI
- add a small architecture note documenting state ownership rules
- add review guidance for oversized files or mixed-responsibility modules
- treat `QUIRKS.md` as a place for unavoidable ecosystem quirks, not a dumping
  ground for architecture debt

## 7. Recommended Phase Order

This is the sequence that best balances leverage and risk.

### Phase 0: Documentation Truth Pass

- audit docs vs code
- freeze the list of mismatches
- decide intended semantics

### Phase 1: Internal Routing Metadata

- thread `client_id`, `session_id`, and `rid` through routed messages
- introduce request/session types without changing the public protocol yet

### Phase 2: Outbound Channel Separation

- create direct-response vs event channels
- stop sending request-scoped results through the broadcast path

### Phase 3: Truthful Handshake And Session Semantics

- implement real connect behavior
- clean up switch-character behavior
- implement or remove ping

### Phase 4: Revisioned State Sync

- define authoritative snapshot/event rules
- remove client repair fetches and dedupe heuristics

### Phase 5: Module Decomposition

- split oversized files along the newly clarified boundaries

### Phase 6: Cleanup And Guardrails

- remove now-dead compatibility code and workaround logic
- lock in tests and documentation

## 8. Suggested First PR

The first PR should not change everything. It should make the next PRs safe.

Recommended first PR scope:

1. add this plan and a protocol mismatch checklist
2. define explicit request/session metadata types
3. thread that metadata through `shore-daemon-server` routed messages and the
   daemon handler without changing client-visible behavior yet
4. add tests proving the metadata survives routing intact

Why this first:

- it attacks the root cause without forcing an immediate protocol migration
- it sets up direct-response routing cleanly
- it reduces the temptation to keep patching over client identity loss

## 9. Acceptance Criteria

This refactor should be considered successful only when all of the following are true.

- the docs describe the real wire behavior
- every request can be traced end-to-end by client/session/request identity
- request-scoped responses are not delivered through the global broadcast path
- handshake state is truthful and sufficient for clients to become coherent
- `switch_character` has one clear, documented semantic model
- clients no longer need repair fetches to stay consistent after normal operations
- the major daemon orchestration files are split along real ownership boundaries
- new client work can proceed from the docs and shared client crate, not from
  reverse-engineering TUI behavior

## 10. Practical Summary

Shore does not primarily need another round of tactical fixes.

It needs:

- truthful protocol semantics
- explicit state ownership
- preserved request identity
- clear separation between direct responses and unsolicited events
- smaller modules with enforceable boundaries

If we land those, future work gets easier.
If we do not, every new feature or client will keep paying architecture debt
interest.
