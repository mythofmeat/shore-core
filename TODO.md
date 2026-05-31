# Tool Surface Simplification

Status: proposed (design note, pre-implementation)
Branch: `tool-simplification`
Author: design pass with Claude, 2026-05-31

## Goal

Shrink the model-facing tool surface and replace the bespoke allowlisted
`exec` with a real, kernel-jailed `bash` tool — the Claude Code model — without
giving up the two load-bearing invariants Shore depends on:

1. Workspace confinement (no reads/writes outside `{character}/workspace/`).
2. Deferred edit staging for prompt-visible files (`SOUL.md`, `USER.md`,
   `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`, `MEMORY.md` stay staged until
   compaction/reload).

This is a surface + security-boundary change, not a plumbing rewrite. The
existing tool registry is already flat and clean (see below); we are not
touching its shape.

## Current state (grounded in code)

All tool wiring lives in `backend/daemon/src/tools/`:

- `mod.rs`
  - `all_tools()` concatenates each module's `tool_defs()` into a `Vec<ToolDef>`
    (`ToolDef { name, description, parameters, category }`). Descriptions are
    loaded from `prompts/tools/**.md` via `include_prompt!`; JSON schemas are
    inline `json!`.
  - `available_tools(is_private, toggles)` is the only gate: private mode hides
    `search_history` and `exec`; per-tool `ToolToggles` filter the rest.
  - `dispatch_tool(name, input, ctx)` is one `match` on the tool name.
  - Dependency injection via the `ToolContext` trait; the canonical impl is
    `SharedToolContext` in `context.rs`.

There are **15 tools** today:

| Module        | Tools |
|---------------|-------|
| `workspace`   | `read`, `write`, `edit`, `list_files`, `search`, `delete`, `exec` |
| `web`         | `web_search`, `fetch_url` |
| `basic`       | `check_time`, `roll_dice`, `set_next_wake` |
| `history`     | `search_history` |
| `images`      | `generate_image` |
| `activity`    | `activity_heatmap` |

The workspace block (7 of 15) is where the bloat concentrates.

### How `exec` is sandboxed today

`handle_exec` (`workspace.rs`):

1. `shell_words::split(command)` → argv (no shell is ever spawned).
2. `is_command_allowed`: `argv[0]` must be in a 30-entry `DEFAULT_ALLOWLIST`
   (`ls, cat, rg, git, wc, pwd, sort, uniq, … cargo, rustc, npm, pnpm, yarn,
   make, cmake`) and must not contain a path separator.
3. `validate_exec_args`: every path-like argument (including `k=v` values) is
   canonicalized and must resolve inside `workspace_dir`; `..`, absolute paths,
   and `file:` URLs are rejected.
4. `Command::new(argv[0])` with `cwd = workdir || workspace_dir`.

So today's `exec` is "allowlisted argv, no shell, path-confined in userspace."
That **is** the AGENTS.md invariant ("`exec` must not invoke a shell and must
keep path-like arguments inside the character workspace") expressed in code.

## Target surface

Workspace block goes from 7 tools to 5:

| Keep            | Change | Drop |
|-----------------|--------|------|
| `read`, `write`, `edit`, `delete` | `exec` → `bash` (real shell, jailed); `search` → semantic-only | `list_files` |

- `list_files` → handled by `bash` (`ls`, `tree`, `find`).
- `search` keeps `hybrid` + `vector` modes only; the `lexical` enum value is
  dropped (lexical text search → `bash` `grep`/`rg`). The internal
  no-embedder → lexical **fallback** stays so `hybrid` still works without an
  embedding index.
- `delete` stays as a tool: it preserves trash/recovery, protected-file
  rejection, and the memory-namespace policy that raw `rm` would lose.
- `read`/`write`/`edit` stay: line-numbered output the model relies on, and the
  `ctx.defer_edit()` hook that implements staging.

Net model-facing count: 15 → 14.

## Why a real shell needs a kernel jail

Two things break the moment `command` is handed to `sh -c` instead of being
parsed into argv:

1. **Confinement.** `validate_exec_args` can no longer reason about argv
   (`cd /; cat /etc/passwd` defeats it). Confinement must move to the kernel.
2. **Deferred-edit staging.** `echo x > SOUL.md` from a shell bypasses
   `ctx.defer_edit()` and mutates a prompt-visible file directly — a violation
   of the AGENTS.md staging rule.

Both are solved by **Landlock**:

- RW ruleset scoped to `workspace_dir`.
- RO on the system paths the toolchain needs (`/usr`, `/bin`, `/lib`, … and
  cargo/rustup dirs).
- **RO on the prompt-visible / protected paths** (sourced from the
  `memory::deferred_edits` module), so a shell physically cannot write them —
  any such edit is forced back through `write`/`edit`, where staging fires.

Network egress: **allowed** (decided). `git`/`cargo`/`npm` keep working;
Landlock confines the filesystem regardless. A future seccomp profile can add a
denylist for dangerous syscalls without blocking sockets.

## Implementation phases

Each phase is independently shippable.

### Phase 0 — surface refactor (no security change)

- Remove `list_files`: its `ToolDef`, `handle_list_files`, the dispatch arm,
  and `prompts/tools/workspace/list_files.md` + `no_memory/list_files.md`.
  Keep `resolve_list_path` — `search` still calls it.
- `search`: drop `lexical` from the `mode` enum → `["hybrid", "vector"]`;
  update `prompts/tools/workspace/search.md` and `no_memory/search.md`. Keep the
  internal no-embedder fallback.
- Rename `exec` → `bash` end to end: `ToolDef.name`, dispatch arm, the
  `is_private` filter literal (`"exec"` → `"bash"`), the prompt md
  (`workspace/exec.md` → `workspace/bash.md`), and tests. Update the
  `all_tools` count test (15 → 14) and `test_available_tools_filters_private`.

This phase changes only names and which tools are exposed — behavior of the
underlying handler is unchanged (still allowlisted argv). Safe to ship alone.

### Phase 1 — real shell

- `handle_bash` runs `sh -c "<command>"` with `cwd = workspace_dir`.
- Delete `is_command_allowed`, `DEFAULT_ALLOWLIST`, `validate_exec_args`, and
  the path-arg validators — they are unenforceable against a shell and are
  replaced by the Phase 2 jail.
- **Do not ship Phase 1 without Phase 2** on Landlock-capable kernels: between
  the two, confinement is gone. (Phase 0 is the safe intermediate to ship.)

### Phase 2 — Landlock + seccomp (the boundary)

- New module `backend/daemon/src/tools/sandbox.rs`.
- Apply via a `pre_exec` hook on the child process using the `landlock` and
  `seccompiler` crates:
  - Landlock RW = `workspace_dir`; RO = system + toolchain paths; RO =
    prompt-visible/protected paths.
  - seccomp denylist for dangerous syscalls (sockets stay allowed).
- **Fail-closed fallback:** if Landlock is unavailable (kernel < 5.13 or
  disabled), do **not** run an unjailed shell. Fall back to the Phase 0
  argv+allowlist handler so the AGENTS.md invariant holds everywhere. Log which
  mode is active.

## Invariants to preserve (checklist for the PR)

- [ ] No path escape from `bash` (verify with a Landlock containment test:
      `cat /etc/passwd` fails, `cat ../../other/file` fails).
- [ ] Prompt-visible files are read-only to `bash`; editing them still requires
      `write`/`edit` and still stages (deferred-edit test).
- [ ] Private mode still hides `bash` and `search_history`.
- [ ] `delete` still trashes, still rejects protected files, still honors the
      memory namespace.
- [ ] Older-kernel fallback path keeps argv+allowlist confinement.
- [ ] `cargo test -p shore-daemon tools::` green; harness-check, fmt, clippy.

## Open questions

- seccomp denylist contents (start permissive, tighten later?).
- Whether the fallback mode should be a hard error in production configs that
  *require* a jail, rather than a silent downgrade.
- Resource limits (CPU/mem/time) on the shell — out of scope here, worth a
  follow-up (`rlimit` / cgroup).
