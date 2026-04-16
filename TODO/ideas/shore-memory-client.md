# Shore Memory Client

A separate GUI client focused entirely on browsing, inspecting, and editing Shore's memory layer. Distinct from the chat GUI — different primary verb (curate/inspect vs converse), different mental mode, different UI density.

## Why separate, not a tab in the chat GUI

- Chat client stays focused on chat. Adding a full memory dashboard doubles its surface and fights the keyboard-first design.
- Memory ops is power-user tooling — doesn't need to be the daily driver, needs to be good when you reach for it.
- Fits Shore's design philosophy: small clients with hard boundaries, "multiple clients is the point."
- Future composability: a standalone memory viewer can be reached for from other debugging/audit contexts (autonomy debugging, behavior audits, Matrix bridge diagnostics) in ways a chat-embedded one can't.

## Scope ceiling: maximum

- Table view of memory rows: inspect full text, edit text, delete.
- RAG-scored retrieval preview: for a given query, show which memories would surface and at what score.
- Metadata visibility: created time, last-accessed time, access count, origin.
- Embedding-space / cluster visualization (LanceDB-backed).
- Memory graph view (relationships, references).
- Audit trail per memory entry.

## Relationship to chat GUI

- Separate binary, separate window, separate install (eventually).
- Both are clients against the same `shore-daemon`; both speak SWP.
- Chat GUI keeps a lightweight inline "memory query" that shows small results with an "open in Memory app" button — best of both worlds.

## Open questions

- Language/framework: probably also TS+Tauri for consistency, but no reason it must match the chat GUI. Separate "best tool for the job" evaluation.
- Does it need remote capability too, or is memory ops always from the same machine as the daemon? (Probably remote too, same Tailscale model.)
- What's the SWP surface for memory ops? Likely needs extension — audit what's currently exposed vs. what this app would need.
- Does it write-through the daemon (which owns the SQLite/LanceDB handles) or read directly? Almost certainly write-through — daemon owns state, client is a view.
