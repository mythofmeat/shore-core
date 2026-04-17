# Shore Memory Client

A separate GUI client focused entirely on browsing, inspecting, and editing Shore's memory layer. 

- Scope ceiling: maximum
  - Table view of memory rows: inspect full text, edit text, delete.
  - RAG-scored retrieval preview: for a given query, show which memories would surface and at what score.
  - Metadata visibility: created time, last-accessed time, access count, origin.
  - Embedding-space / cluster visualization (LanceDB-backed).
  - Memory graph view (relationships, references).
  - Audit trail per memory entry.

## Other info

### Does it need remote capability too?
Yes. lan/tailscale. same as the tui

## Open questions

- Language/framework: separate "best tool for the job" evaluation.
- What's the SWP surface for memory ops? Likely needs extension — audit what's currently exposed vs. what this app would need.
- Does it write-through the daemon (which owns the SQLite/LanceDB handles) or read directly? Almost certainly write-through — daemon owns state, client is a view.
