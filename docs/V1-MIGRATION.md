# Shore V1 → V2 Migration Guide

This document describes how to migrate from Shore V1 (Python) to Shore V2 (Rust/TypeScript).

## Configuration Changes

### config.toml

V2 renames and restructures the top-level sections. The daemon detects
V1 sections at startup and prints migration warnings with the correct
replacement.

| V1 Section   | V2 Replacement             | Notes                                              |
|-------------|----------------------------|----------------------------------------------------|
| `[server]`  | `[daemon]`                 | Keys unchanged — only the section name changed.     |
| `[char]`    | `[character]`              | Keys unchanged — only the section name changed.     |
| `[llm]`     | Removed — use `models.toml` | Model definitions moved to a separate file.         |

**V1 config.toml:**
```toml
[server]
socket_path = "/tmp/shore.sock"

[char]
name = "Shore"

[llm]
default_model = "gpt-4"
```

**V2 config.toml:**
```toml
[daemon]
socket_path = "/tmp/shore.sock"

[character]
name = "Shore"
```

### models.toml (new in V2)

Model definitions are now in a dedicated file. The `model` field was
renamed to `model_id`.

| V1 Field   | V2 Field    | Notes                           |
|-----------|------------|----------------------------------|
| `model`   | `model_id` | Identifier sent to the provider. |

**V1 (inside config.toml [llm]):**
```toml
[llm]
default_model = "gpt-4"
```

**V2 models.toml:**
```toml
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"

[[models]]
name = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
```

## Data Directory

Character data lives under `$XDG_DATA_HOME/shore/<character>/` (typically
`~/.local/share/shore/<character>/`).

| V1 Path                           | V2 Path                           | Notes                              |
|-----------------------------------|-----------------------------------|------------------------------------|
| `conversations/<id>/manifest.json`| Same                              | V2 adds optional `private` field.  |
| `conversations/<id>/messages.jsonl`| Same                             | V2 adds optional fields per message.|
| `memory/memory.db`                | Same                              | Schema unchanged; V2 adds indices. |
| —                                 | `memory/vectorstore/`             | New in V2 — LanceDB vector index.  |

The daemon reads V1 conversation files, JSONL messages, and manifests
transparently. Missing fields (e.g. `private`, `msg_id`) are filled with
safe defaults.

## Reindexing

V1 stored memory entries in SQLite only. V2 adds a LanceDB vector store
for semantic search. On first V2 startup with existing V1 data, run:

```
shore reindex
```

This extracts active and protected entries from SQLite, computes
embeddings via `shore-llm`, and populates the vector store.

## Binary Changes

| V1 Binary     | V2 Binary       | Notes                                    |
|--------------|-----------------|------------------------------------------|
| `shore` (Python) | `shore-daemon` | Persistent daemon process.              |
|              | `shore-cli`     | Stateless CLI commands.                   |
|              | `shore-tui`     | Terminal UI with persistent connection.   |
|              | `shore-matrix`  | Matrix bridge (replaces Python bridge).   |
|              | `shore-llm`     | TypeScript LLM provider proxy (Node.js).  |

## Removed Features

The V1 Python codebase has been fully retired. All functionality is
implemented in the V2 Rust/TypeScript stack. The V1 Python code has been
removed from the active repository.
