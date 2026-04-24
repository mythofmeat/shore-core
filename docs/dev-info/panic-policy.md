# Panic Policy

Runtime daemon paths should return errors, not panic.

Acceptable `unwrap()` / `expect()` buckets:

- tests
- process startup where continuing would be nonsensical
- impossible-by-construction serialization paths
- poisoned mutex recovery code that immediately repairs state

Not acceptable:

- user input handling
- provider responses
- filesystem/workspace paths
- SWP frame parsing
- memory compaction output
- tool dispatch

When touching shared runtime code, prefer `Result` with context over panic. If a panic remains in production code, it should be obvious which bucket above it belongs to.
