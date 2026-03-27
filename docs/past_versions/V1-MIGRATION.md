# Shore V1 → V2 Migration Guide

This document describes how to migrate from Shore V1 (Python) to Shore V2 (Rust/TypeScript).

## Configuration Changes

### config.toml

V2 renames and restructures the top-level sections. The daemon detects
V1 sections at startup and prints migration warnings with the correct
replacement.

| V1 Section   | V2 Replacement                    | Notes                                              |
|-------------|-----------------------------------|----------------------------------------------------|
| `[server]`  | `[daemon]`                        | Keys unchanged — only the section name changed.     |
| `[char]`    | `[character]`                     | Keys unchanged — only the section name changed.     |
| `[llm]`     | `[chat.<provider>.<model>]`       | Nested structure in config.toml with provider defaults cascading. |

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

### Model definitions

V2 uses nested `[chat.<provider>.<model>]` tables in config.toml. Provider-level
defaults cascade into all models under that provider. Known providers have hardcoded
defaults (api_key_env, base_url, sdk), so minimal config is needed.

Models can live directly in config.toml, in files loaded via `include = [...]`,
or in `conf.d/*.toml` drop-in files.

| V1 Field        | V2 Equivalent                          | Notes                                        |
|-----------------|----------------------------------------|----------------------------------------------|
| `default_model` | `[defaults] model = "chat.anthropic.sonnet"` | Qualified or short name.              |
| `model`         | `model_id`                             | Identifier sent to the provider.              |

**V1 (inside config.toml [llm]):**
```toml
[llm]
default_model = "gpt-4"
```

**V2 (inline in config.toml or a separate included file):**
```toml
[chat.anthropic]
# Provider-level defaults — inherited by all models below
api_key_env = "ANTHROPIC_API_KEY"    # (also the hardcoded default)
max_tokens = 8192

[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-6"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
max_tokens = 16384                   # Overrides provider default

[chat.openai.gpt-4o]
model_id = "gpt-4o"
```

Tool models, embedding profiles, and image generation profiles use parallel
top-level sections (`[tools.*]`, `[embedding.*]`, `[image_generation.*]`).

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
shore memory --reindex
```

This extracts active and protected entries from SQLite, computes
embeddings via `shore-llm`, and populates the vector store.

## Binary Changes

| V1 Binary     | V2 Binary       | Notes                                    |
|--------------|-----------------|------------------------------------------|
| `shore` (Python) | `shore-daemon` | Persistent daemon process.              |
|              | `shore`         | Stateless CLI commands.                   |
|              | `shore-tui`     | Terminal UI with persistent connection.   |
|              | `shore-mx`      | Matrix bridge (replaces Python bridge).   |
|              | `shore-llm`     | TypeScript LLM provider proxy (Node.js).  |

## Removed Features

The V1 Python codebase has been fully retired. All functionality is
implemented in the V2 Rust/TypeScript stack. The V1 Python code has been
removed from the active repository.
