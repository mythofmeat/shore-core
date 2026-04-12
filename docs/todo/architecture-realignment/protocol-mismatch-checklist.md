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

## Still Open

### 1. Placeholder Handshake State

- Current code behavior:
  - `shore-daemon-server` sends a placeholder `hello` character list and an
    empty `history` snapshot during handshake.
- Current docs claim:
  - Handshake returns meaningful initial state that clients can trust.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phase 3: truthful handshake
    and session semantics.

### 2. No Server `rid` Echo

- Current code behavior:
  - client requests carry `rid`, but server response messages do not echo it on
    the wire.
- Current docs claim:
  - request IDs are end-to-end correlation identifiers.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phases 2-4.

### 3. Startup And Session-State Truth Are Still Incomplete

- Current code behavior:
  - `switch_character` now updates session-owned selected character state after
    daemon success and no longer returns reconnect-oriented command semantics.
  - handshake payloads are still placeholder startup state rather than one
    truthful authoritative session snapshot.
- Current docs claim:
  - connect-time and session-mutation behavior should be authoritative and
    self-consistent.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phase 3: truthful handshake
    and session semantics, then Phase 4: revisioned state sync.

### 4. Snapshot And Event Sync Are Still Mixed

- Current code behavior:
  - Shore still mixes `History` snapshots, `NewMessage` events, stream events,
    and client-side refresh behavior.
- Current docs claim:
  - the protocol should have one authoritative state synchronization model.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phase 4: revisioned state
    sync.

## Resolved Or Materially Narrowed On This Branch

### 5. Direct Responses No Longer Use Broadcast Delivery For Main Request Paths

- Current code behavior:
  - command results, request-scoped errors, stream/tool traffic, and
    cancellation outcomes now route through per-session direct senders.
  - unsolicited events still use the broadcast/event path.
- What remains:
  - Phase 2 still needs a final outbound-path audit and stronger two-client
    coverage, but this is no longer the primary protocol mismatch it was at the
    start of the program.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phase 2: outbound channel
    separation closeout.

### 6. `switch_character` Is No Longer Reconnect-Oriented At Command Level

- Current code behavior:
  - the command now returns session-mutation semantics instead of
    `reconnect_required: true`.
  - the CLI now sends the daemon command before updating local state.
- What remains:
  - handshake/startup truth and the final authoritative state model are still
    open, so Phase 3 and Phase 4 are not done just because the command result
    shape is fixed.
- Later owner:
  - `docs/todo/1-architecture-realignment-plan.md`, Phase 3 and Phase 4.
