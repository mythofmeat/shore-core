# Shore Operational Hardening Plan

This plan is a companion to
[`architecture-realignment-plan.md`](./architecture-realignment-plan.md).

The architecture realignment plan is about restoring truthful boundaries,
session ownership, and protocol coherence.

This document covers the adjacent issues that also make Shore harder to run,
debug, and trust, but are not themselves the main protocol/session problem.

## Status Snapshot

Updated 2026-04-13 after the runtime-invalidation pass.

- Workstream A is complete enough to treat as landed. Shore is now documented
  as localhost-only by default, non-loopback daemon binds require explicit
  `unsafe_allow_remote_access` opt-in, and daemon startup rejects accidental
  remote exposure while warning that `allowed_hosts` is only an IP allowlist.
- Workstream B is complete enough to treat as landed. `shore-client` now
  applies the same bounded newline framing limit as the server, using a shared
  `MAX_WIRE_MESSAGE_SIZE` constant and explicit oversized-frame errors during
  both handshake and steady-state reads.
- Workstream C is complete enough to treat as landed. Registry writes now use
  a stable sidecar lock file plus atomic replace, corrupt registry JSON is
  preserved and surfaced instead of being treated as empty state, discovery
  distinguishes missing registries from corrupt ones, and registry metadata now
  uses RFC3339 timestamps.
- Workstream D is complete enough to treat as landed. `config_reset` is now the
  explicit runtime invalidation boundary: it reloads the active config dir,
  clears session runtime overrides and memory-shell sessions, rescans
  characters, drops merged per-character config cache, drops cached memory DB
  and vector-store handles, and removes engines only for deleted characters.
  The runtime refresh model is now documented and covered by daemon tests.
- The next pickup point is Workstream E: clean up daemon startup surface.

## 1. Why This Is Separate

These issues matter, but they should not be allowed to derail the core
architecture work by expanding it into an unbounded cleanup project.

They deserve their own plan because they are mostly about:

- operational safety
- remote access assumptions
- client/server robustness
- portability and corruption handling
- runtime invalidation behavior
- startup ergonomics

These are important enough to track explicitly, but distinct enough that they
should not be mixed into the first session/protocol refactor phases.

## 2. Problems This Plan Covers

### Problem A: Remote Access Has A Thin Security Story

Shore documents remote access through `client.toml` and examples like
Tailscale, but the daemon-side control surface is still mostly "bind a TCP
port and optionally restrict peer IPs with `allowed_hosts`."

That may be acceptable if Shore is intentionally a trusted-network-only daemon.
But if that is the intended model, it should be explicit and enforced.
If it is not the intended model, the current setup is underspecified and too
easy to misread as "remote-ready."

### Problem B: Client Transport Hardening Lags Behind Server Hardening

The server already bounds inbound message size, but the client still reads
server messages with an unbounded line read. That means Shore protects the
daemon from a malicious client better than it protects a client from a
malicious or buggy server.

This asymmetry should be removed.

### Problem C: Discovery And Registry Handling Are Too Linux-Specific And Too Forgiving

The instance-discovery story currently depends on `/proc` PID checks for liveness.
That may be fine for a Linux-first system, but it should be deliberate.

Separately, malformed registry JSON is sometimes treated as an empty/default
state rather than as a real integrity problem. That can silently erase useful
diagnostics and make debugging harder.

### Problem D: Runtime Invalidation Is Not A First-Class Model

Shore caches:

- discovered character names
- per-character merged configs
- opened `MemoryDB` handles
- opened `VectorStore` handles

There are targeted invalidation paths, but not yet a coherent runtime model for
what should happen when the filesystem or config changes underneath a running
daemon.

This increases the odds of stale state and "why didn't it pick up my change?"
confusion.

### Problem E: The Daemon Startup Surface Is Narrow And Brittle

The daemon entrypoint still does manual argument parsing for `--config` and
relies on several implicit conventions for startup behavior.

This is survivable now, but it becomes a drag once startup options grow or when
we need better diagnostics for production operators.

## 3. Goals

### Goal 1: Make Shore's Remote Security Model Explicit

Nobody should need to infer from source code whether Shore is:

- localhost-only by design
- trusted-overlay-only
- or intended for authenticated remote use

The supported deployment model should be documented and reflected in defaults.

### Goal 2: Symmetric Transport Safety

Both sides of the connection should defend themselves against oversized or
malformed traffic.

### Goal 3: Honest Discovery And Better Failure Modes

Instance discovery should fail loudly and diagnosably when registry state is
corrupt, rather than silently flattening bad state into "nothing found."

### Goal 4: Coherent Runtime Refresh Behavior

When config, character directories, or cached resources change, Shore should
have a documented policy for:

- what is reloaded automatically
- what is reloaded only on explicit command
- what requires reconnect or restart

### Goal 5: Predictable Startup And Operator UX

The daemon's startup behavior should be easy to extend, easy to document, and
clear when things go wrong.

## 4. Proposed Workstreams

### Workstream A: Define The Remote Security Model

Goal:

- make Shore's remote access assumptions explicit in docs, defaults, and config

Actions:

- decide whether Shore's supported model is:
  - localhost-only by default with no auth
  - trusted-network-only with explicit warning language
  - or authenticated remote access
- document the chosen model clearly in `README.md` and `ARCHITECTURE.md`
- audit daemon config naming so unsafe remote exposure is obviously intentional
- consider adding an explicit "unsafe remote access" switch if the current
  `allowed_hosts` story remains the only access control
- decide whether future auth/TLS support is planned, deferred, or explicitly
  out of scope

Exit criteria:

- the deployment model is obvious from the docs
- users cannot easily mistake "IP allowlist only" for full remote security

### Workstream B: Harden Client-Side Framing

Goal:

- give `shore-client` the same basic framing protections the server already has

Actions:

- add bounded line reading for server messages in `shore-client`
- centralize the max wire message size in one shared place if practical
- add client-side tests for oversized server responses
- ensure error reporting is explicit when a server violates framing limits

Non-goal:

- do not redesign the whole protocol here; this is just framing symmetry

Exit criteria:

- client reads are bounded
- oversized server lines fail deterministically instead of growing without limit

### Workstream C: Harden Registry And Discovery Behavior

Goal:

- make instance discovery more diagnosable, less Linux-ambient, and less
  tolerant of silent corruption

Actions:

- replace raw `/proc/{pid}` assumptions with a small platform abstraction, or
  explicitly document Linux-only support if that is the intentional position
- stop collapsing malformed registry JSON into an empty/default state without
  recording that corruption occurred
- consider writing corrupt registry payloads aside for diagnosis instead of
  overwriting them silently
- standardize timestamp format in registry metadata
- review file-locking and atomic-write behavior for the instance registry

Why timestamp format belongs here:

- `started_at` is registry metadata, but the daemon currently writes an
  epoch-ish custom string rather than a broadly useful interoperable timestamp

Exit criteria:

- registry corruption is diagnosable
- platform assumptions are explicit
- registry metadata is consistently formatted

### Workstream D: Define Runtime Invalidation Rules

Goal:

- make cache and refresh behavior explicit instead of incidental

Actions:

- write down what should happen when:
  - a new character directory appears
  - a character directory is deleted
  - a per-character config changes
  - the global config changes
  - a memory DB or vector store is rebuilt externally
- decide which caches are session-scoped, process-scoped, and character-scoped
- decide whether explicit commands like `config_reset` should also invalidate:
  - character discovery cache
  - per-character merged config cache
  - DB/vector-store handle caches
- add tests for the chosen invalidation semantics

Important constraint:

- this work should align with the session/protocol refactor rather than fighting it

Exit criteria:

- Shore has a documented runtime refresh model
- invalidation behavior is intentional and tested

### Workstream E: Clean Up Daemon Startup Surface

Goal:

- make startup behavior easier to extend and safer to operate

Actions:

- replace manual `std::env::args()` parsing in `shore-daemon` with a proper CLI parser
- define a single source of truth for startup configuration precedence
- improve startup diagnostics for bind errors, invalid config paths, and unsafe
  remote exposure
- decide which runtime toggles belong as CLI flags versus env vars versus config

Exit criteria:

- daemon startup behavior is predictable and extensible
- operator-facing errors are clearer

### Workstream F: Add A Small Operability Review Pass

Goal:

- catch the small things that do not justify a full architecture phase but still
  hurt daily iteration

Actions:

- review daemon and client log messages for operator usefulness
- audit startup and shutdown cleanup behavior around the instance registry
- check whether "always-on cache forensics" should remain always-on or become a
  deliberate operator option
- review docs for "implicitly Linux-only" assumptions

This workstream is intentionally smaller and should happen after the more
important items above are clearer.

## 5. Recommended Sequence

### Phase 0: Security Model Decision

- decide what remote usage Shore officially supports
- update docs first so users are not guessing

### Phase 1: Client Framing Hardening

- bound client reads
- add framing symmetry tests

### Phase 2: Registry And Discovery Hardening

- make corruption handling explicit
- standardize metadata formats
- clarify portability assumptions

### Phase 3: Runtime Invalidation Model

- define cache ownership and invalidation behavior
- align with architecture realignment work

### Phase 4: Startup Surface Cleanup

- move daemon startup parsing to a proper CLI layer
- improve operator diagnostics

### Phase 5: Operability Sweep

- close out smaller logging/docs/cleanup follow-ups

## 6. Suggested First PR

Recommended first PR scope:

1. add this plan
2. document Shore's current remote security model explicitly
3. add bounded reads to `shore-client`
4. add tests for oversized server responses

Why this first:

- it delivers immediate safety value
- it does not block the larger architecture realignment
- it avoids leaving the client as the weaker side of the transport boundary

## 7. Acceptance Criteria

This follow-up work should be considered successful when:

- Shore's remote access story is explicit and hard to misinterpret
- client framing has the same basic safety properties as server framing
- registry corruption and discovery failures are diagnosable
- runtime invalidation behavior is documented and tested
- daemon startup parsing and diagnostics are no longer ad hoc

## 8. Practical Summary

The big architecture plan fixes how Shore thinks.

This follow-up plan fixes how Shore behaves operationally when:

- users expose it on a network
- daemons and clients encounter malformed state
- runtime resources change underneath a live process
- operators need startup and discovery behavior to be predictable

These are second-order issues compared to the protocol/session refactor, but
they are still real friction and should be handled deliberately rather than as
future cleanup folklore.
