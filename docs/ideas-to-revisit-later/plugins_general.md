# Plugin Ideas

Status: idea, not current implementation.

A future plugin system should add tools or integrations without changing Shore's core invariants:

- daemon remains authoritative
- memory remains markdown-first
- prompt-cache boundaries stay explicit
- plugin tools obey private-mode and memory gates
- plugin state is inspectable or clearly isolated

Good first plugin candidates are integrations that enrich character life without owning core state: media libraries, reading logs, music metadata, bookmarks, or local project notes.
