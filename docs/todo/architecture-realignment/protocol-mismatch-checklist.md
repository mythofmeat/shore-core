# Protocol Mismatch Checklist

This checklist records protocol and architecture mismatches that remain
intentional after Architecture Realignment PR 1.

The purpose of the first PR is to preserve client/session/request identity
internally without changing the public SWP contract yet.

Updated 2026-04-12:

- the checklist now distinguishes still-open mismatches from items that were
  resolved or materially narrowed by the first implementation pass
- the execution tracker for future work is
  [`architecture-realignment-implementation-plan.md`](./architecture-realignment-implementation-plan.md)
- the Phase 2 closeout pass added explicit two-client isolation coverage for
  command, stream, tool, and cancel traffic
- the truthful-handshake closeout pass replaced placeholder startup data,
  implemented TCP `ping`, and made `switch_character` push an authoritative
  session snapshot

## Still Open

### 1. No Server `rid` Echo

- Current code behavior:
  - client requests carry `rid`, but server response messages do not echo it on
    the wire.
- Current docs claim:
  - request IDs are end-to-end correlation identifiers.
- Later owner:
  - `docs/todo/architecture-realignment/architecture-realignment-plan.md`, Phases 2-4.

### 2. Snapshot And Event Sync Are Still Mixed

- Current code behavior:
  - Shore still mixes `History` snapshots, `NewMessage` events, stream events,
    and client-side refresh behavior.
- Current docs claim:
  - the protocol should have one authoritative state synchronization model.
- Later owner:
  - `docs/todo/architecture-realignment/architecture-realignment-plan.md`, Phase 4: revisioned state
    sync.

## Resolved Or Materially Narrowed On This Branch

### 3. Handshake State Is No Longer Placeholder Data

- Current code behavior:
  - `shore-daemon-server` now loads the handshake from daemon-owned state:
    real character discovery, truthful selected-character resolution, real
    conversation history, and a minimal truthful session/config snapshot.
  - TCP keepalive `ping` is now emitted by the server.
- What remains:
  - handshake truth is no longer the open mismatch.
  - the remaining follow-on work is Phase 4 revision semantics, not placeholder
    startup data.
- Later owner:
  - `docs/todo/architecture-realignment/architecture-realignment-plan.md`, Phase 4: revisioned
    state sync.

### 4. Direct Responses No Longer Use Broadcast Delivery For Main Request Paths

- Current code behavior:
  - command results, request-scoped errors, stream/tool traffic, and
    cancellation outcomes now route through per-session direct senders.
  - multi-client tests now assert isolation for command, stream, tool, and
    cancel flows, and `shore-daemon-server` has a transport-level direct-send
    isolation test.
  - unsolicited events still use the broadcast/event path.
- What remains:
  - this is no longer a Phase 2 routing gap.
  - the remaining open question is Phase 4 semantics: `History` and
    `NewMessage` are still broadcast/event-style updates rather than a final
    revisioned authoritative sync model.
- Later owner:
  - `docs/todo/architecture-realignment/architecture-realignment-plan.md`, Phase 4: revisioned
    state sync.

### 5. `switch_character` Is No Longer Reconnect-Oriented Or Repair-Fetch Driven

- Current code behavior:
  - the command now returns session-mutation semantics instead of
    `reconnect_required: true`.
  - successful switches update session-owned selected character state and push
    an authoritative direct `history` snapshot for the new character.
  - the CLI now sends the daemon command before updating local state.
- What remains:
  - the remaining work is the Phase 4 revisioned sync contract, not reconnect
    semantics or placeholder startup repair flows.
- Later owner:
  - `docs/todo/architecture-realignment/architecture-realignment-plan.md`, Phase 4.
