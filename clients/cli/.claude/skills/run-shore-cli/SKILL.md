---
name: run-shore-cli
description: Build, test, and preview the shore CLI's terminal output rendering. Use when asked to run shore-cli, see/preview/screenshot how its transcript or streaming output looks, check colors/separators/thinking rendering, or verify a change to clients/cli/src/output without standing up a daemon.
---

`shore-cli` is the `shore` command-line client for the Silvershore daemon.
The interesting, frequently-changed surface is the **output renderer**
(`src/output/`): the transcript shown by `shore log` / `shore get` and the
live token stream. The catch — the real CLI needs a running daemon and a
thinking-capable model to produce that output, and the crate is **binary-only
(no `lib` target)**, so the `output` module can't be reached from an
`examples/` file or any external driver.

The agent path here drives the **real renderers** through `#[ignore]`-d preview
tests and dumps the colorized bytes to your terminal:
**`.claude/skills/run-shore-cli/preview.sh`**.

All paths below are relative to `clients/cli/` (the unit). The driver also
works from the repo root.

## Prerequisites

The Rust workspace toolchain — nothing else (no `apt-get` packages were needed;
it builds and tests clean on a stock Linux toolchain).

```bash
cargo --version   # 1.95.0 used here; any recent stable works
```

## Build

No separate build step for the preview path — `cargo test` compiles what it
needs. To build the actual binary:

```bash
cargo build -p shore-cli            # produces target/debug/shore
```

## Run (agent path) — preview the rendering

```bash
.claude/skills/run-shore-cli/preview.sh           # both previews, colorized
.claude/skills/run-shore-cli/preview.sh log       # just shore log / shore get
.claude/skills/run-shore-cli/preview.sh stream    # just live token streaming
```

Each preview renders a representative assistant turn with **interleaved
thinking** (thinking → text → tool call → redacted_thinking → thinking → text)
through the real `render_message_content` (log) and `print_chunk_to` (stream)
functions, with color ON, and prints the raw bytes so your terminal colorizes
them. Thinking is dim grey with a `│` left-gutter bar (word-wrapped to the
terminal width so the bar runs down every wrapped row), tool labels are yellow,
a blank line of breathing room straddles each thinking section, and
`redacted_thinking` blocks are hidden. The preview thinking blocks are long on
purpose so you can see the wrapping; resize your terminal and re-run to confirm
the wrap follows the width.

Expected shape (log path):

```
<dim>  │ Let me reason about this first.
  │ The user asked about X, so I should check Y.</dim>

Here's the first part of my answer.
<yellow>[tool: read_file]</yellow>
  path: src/main.rs
<dim>[result]</dim>
  fn main() { ... }

<dim>  │ Now that I've read the file, I can refine my answer.</dim>

And here's the refined conclusion.
```

## Direct invocation — the technique, and adding a preview

The preview tests live next to their assertion counterparts:

- `src/output/transcript.rs` → `render_preview_log` (drives `render_message_content`)
- `src/output/styling.rs` → `render_preview_stream` (drives `print_chunk_to`)

To preview a different message/stream, copy one of those tests, change the
`content_blocks` / chunk sequence, and run it directly:

```bash
cargo test -p shore-cli render_preview_log \
  -- --ignored --nocapture --test-threads=1
```

The recipe for any in-crate visual preview:

1. In the relevant `#[cfg(test)] mod tests`, `set_color_enabled(true)`.
2. Render into a `Vec<u8>` via the real (private) renderer.
3. `set_color_enabled(false)` again, then write the buffer straight to
   `io::stdout()` so the ANSI escapes survive.
4. Mark it `#[ignore]` so it stays out of normal `cargo test`, and run it with
   `--ignored --nocapture --test-threads=1`.

## Test (assertions)

The behavior is pinned by ordinary (non-ignored) tests in the same modules
(`interleaved_thinking_gutter_both_directions`,
`streaming_interleaved_thinking_gutter_both_directions`, `redacted_thinking_is_hidden`, …):

```bash
cargo test -p shore-cli output::      # 57 pass, 2 ignored (the previews)
```

## Run (human path) — the real CLI

The actual client needs a daemon and a model; it is not driveable headless here
for rendering previews. For reference only:

```bash
cargo run -p shore-cli -- log          # talks to a running daemon over its socket
cargo run -p shore-cli -- log --plain  # plain variant: prefixes thinking with [thinking]
```

Use the preview path above instead when you only need to see how output looks.

## Gotchas

- **`--test-threads=1` is mandatory for previews.** `COLOR_ENABLED` and the
  streaming `CHUNK_STATE` are process globals; parallel tests race on them and
  you'll get bleed-through (a stray gutter or color from another test) or a
  garbled buffer. The driver already sets this.
- **Color must be toggled back off after rendering.** The global stays set for
  the rest of the process; the preview tests flip it back to `false` so they
  don't tint other tests sharing the process.
- **`#[ignore]`, not deletion.** Previews live permanently in the test modules
  but are skipped by default — that's why a normal `output::` run reports
  `2 ignored`. Don't "clean them up."
- **Two render paths, different rules.** The colored transcript
  (`render_message_content`) gutter-bars thinking with a dim `│` and blank-line
  breathing room; the `--plain` path (`print_log_plain`) instead prefixes each
  thinking line with `[thinking]` and uses no box-drawing. Both hide
  `redacted_thinking`. Preview the one your change touches.
- **No `lib` target.** You cannot add an `examples/preview.rs` that imports
  `output::…` — the symbols aren't exported. The in-crate test is the only
  handle on these private renderers.

## Troubleshooting

- **Driver prints nothing.** The `sed` range only emits lines between
  `----- … -----` and `----- end -----`. If a compile error occurred, it went to
  stderr — rerun the raw command (`cargo test -p shore-cli render_preview --
  --ignored --nocapture --test-threads=1`) to see it.
- **Colors don't show, you see raw `^[[38;5;8m`.** Your viewer is escaping the
  bytes (e.g. piping through `cat -v`). Run the driver directly in a terminal.
- **`0 ignored` / preview didn't run.** You dropped `--ignored`, or the test
  name filter didn't match. Use `render_preview` (both), `render_preview_log`,
  or `render_preview_stream`.
