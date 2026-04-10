# Inline Streaming Thinking & Tool Use Display

## Problem

During message generation, the TUI shows a bordered "thinking panel" between the conversation and input areas. This panel:

1. Is visually distinct from all other generation info — it looks like a popup, not part of the conversation
2. Causes layout shifts when it appears/disappears (the layout constraint jumps between `Length(6)` and `Length(0)`)
3. Merges all thinking into a single blob (`app.stream.thinking: String`), destroying interleaving with text content
4. Has its own toggle (`thinking_collapsed` / Tab key) separate from the history toggle (`show_thinking` / t key)

## Design

### Core Change

Replace the popup thinking panel with inline rendering that matches post-stream display, and replace the flat string buffers with a block list that preserves interleaving.

### Data Model

Replace `StreamState.text: String` + `StreamState.thinking: String` + `StreamState.thinking_collapsed: bool` with a single block list:

```rust
/// A segment of streaming content, preserving interleaving order.
#[derive(Clone, Debug)]
pub enum StreamBlock {
    Thinking(String),
    Text(String),
}

pub struct StreamState {
    pub active: bool,
    pub regen: bool,
    pub blocks: Vec<StreamBlock>,  // replaces text + thinking + thinking_collapsed
    pub phase: String,
    pub tool_name: Option<String>,
}
```

### StreamChunk Processing (main.rs)

On each incoming `StreamChunk`, check if the last block matches the incoming `content_type`. If so, append to it. Otherwise, push a new block:

```rust
ServerMessage::StreamChunk(chunk) => {
    let is_thinking = chunk.content_type == "thinking";
    match (is_thinking, app.stream.blocks.last_mut()) {
        (true, Some(StreamBlock::Thinking(ref mut s))) => s.push_str(&chunk.text),
        (false, Some(StreamBlock::Text(ref mut s))) => s.push_str(&chunk.text),
        (true, _) => app.stream.blocks.push(StreamBlock::Thinking(chunk.text)),
        (false, _) => app.stream.blocks.push(StreamBlock::Text(chunk.text)),
    }
    app.stream.phase = if is_thinking { "thinking" } else { "responding" }.into();
    // auto_scroll as before
}
```

### StreamEnd Processing (main.rs)

Replace `app.stream.text.clear()` + `app.stream.thinking.clear()` + `app.stream.thinking_collapsed = false` with `app.stream.blocks.clear()`.

In the tool_use branch (tool loop continuation), same: `app.stream.blocks.clear()`.

In `StreamState::reset()`, replace the three field clears with `self.blocks.clear()`.

### Rendering (ui.rs)

#### Layout: Remove thinking panel

`draw()` layout goes from 3 constraints back to 2:

```rust
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Min(3),               // conversation
        Constraint::Length(input_height),  // input
    ])
    .split(size);
```

Remove the `if thinking_height > 0 { draw_thinking(...) }` call. Delete `draw_thinking` entirely.

#### Conversation: Remove trailing-thinking skip

Remove the `entry_count` logic that skips trailing `Thinking` entries during streaming (lines 361-371). Always render all entries. The existing `flush_thinking` already gates on `show_thinking`, so no duplicate display.

#### `render_streaming_state`: Inline block rendering

Rewrite to iterate `stream.blocks` and render each segment inline:

1. Render assistant name header (as now)
2. For each block in `stream.blocks`:
   - `StreamBlock::Thinking(s)`: Render with `◆ thinking` header + bar-indented content, same style as `flush_thinking`. Gated by `app.show_thinking`.
   - `StreamBlock::Text(s)`: Render as pre-wrapped markdown + indented, same as current streaming text.
3. Always render compact spinner at the end:
   - `"thinking ···"` when phase is `"thinking"`
   - `"▶ tool_name ···"` when `stream.tool_name` is set
   - `"waiting for tool ···"` when phase is `"tool_use"` but no tool_name
   - `"···"` otherwise

The spinner uses the existing DarkGray italic style, indented 2 spaces.

### Toggle Behavior

| Key | Current | New |
|-----|---------|-----|
| `Tab` (normal) | Toggle `thinking_collapsed` (popup only) | Remove binding (or repurpose) |
| `Tab` (insert) | Toggle `thinking_collapsed` (popup only) | Remove binding |
| `t` | Toggle `show_thinking` (history only) | Toggle `show_thinking` (history AND streaming) |
| `T` | Toggle `show_tools` (history only) | No change needed (tools already render via entries) |

### Preferences

The `thinking_collapsed` field is removed from `StreamState`. The `show_thinking` preference (already persisted in `tui_prefs.json`) now controls all thinking visibility uniformly.

## Files Changed

| File | Changes |
|------|---------|
| `shore-tui/src/app.rs` | Add `StreamBlock` enum. Replace `text`/`thinking`/`thinking_collapsed` with `blocks: Vec<StreamBlock>` in `StreamState`. Update `reset()`. |
| `shore-tui/src/ui.rs` | Remove `draw_thinking`. Remove thinking panel from `draw()` layout. Remove trailing-thinking skip. Rewrite `render_streaming_state` for inline blocks + always-on spinner. |
| `shore-tui/src/main.rs` | Update `StreamChunk` handler for append-or-push. Update `StreamEnd` handler to clear blocks. |
| `shore-tui/src/input.rs` | Remove `Tab` → `thinking_collapsed` toggle in normal and insert modes. |

## Not Changed

- Post-stream rendering (`flush_thinking`, `flush_tools`) — already correct
- Tool call/result handling — already pushed as entries during streaming
- `ConversationEntry` enum — unchanged
- Wire protocol — unchanged
