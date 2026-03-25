# Feature Checklist

Organized by milestone. Items within a milestone are roughly priority-ordered. Check off as implemented.

Items marked **[upstream]** require changes to the Shore server before they can be implemented.

---

## Milestone 1: Core loop

The only goal here is a working, streaming-capable connection to the daemon.

- [ ] Connect to Shore Unix socket
- [ ] Authenticate / handshake (if required)
- [ ] Fetch and display conversation history on startup
- [ ] Send a message
- [ ] Display streaming response in real time (stream_start → stream_chunk → stream_end)
- [ ] Graceful exit with Ctrl+C (clean disconnect, terminal state restored)
- [ ] Reconnection UX: non-blocking "Disconnected" banner, history stays visible, auto-reconnect

---

## Milestone 2: Input

- [ ] Single-line input with cursor movement
- [ ] Input history: Up/Down arrow to recall previous inputs
- [ ] Multiline input: Shift+Enter inserts newline, input box grows vertically
- [ ] Ctrl+G opens current input in `$EDITOR` (like Claude Code)
- [ ] Input bar separator (visual divider between conversation and input)

---

## Milestone 3: Display & rendering

- [ ] Scrollable conversation log (mouse + keyboard)
- [ ] Configurable margins / max text width on conversation log
  - [ ] Margins shrink gracefully on narrow terminals before wrapping kicks in
  - [ ] Setting persists across sessions
- [ ] Full-width input (no margins)
- [ ] Message headers: role, timestamp, model used
  - [ ] Timestamp display toggleable in settings
- [ ] Horizontal rules between messages (toggleable in settings)
- [ ] Gap indicators: dimmed margin note showing duration of long idle gaps between messages
- [ ] Colored spinner during generation
- [ ] Real-time tool use visibility during generation (e.g. `⟳ Tool: memory [full text]`)
  - [ ] Verbosity toggleable in settings
- [ ] Real-time reasoning/thinking display during generation (toggleable in settings)
- [ ] Token count display in message header (toggleable in settings)
  - [ ] Specifically: cache reads/writes for Anthropic models (cost tracking)

---

## Milestone 4: Markdown rendering

- [ ] Bold, italic, strikethrough
- [ ] Inline code
- [ ] Fenced code blocks (with syntax highlighting if feasible)
- [ ] Headers
- [ ] Ordered and unordered lists
- [ ] Blockquotes
- [ ] Links (URL shown dimmed)

---

## Milestone 5: Navigation & search

- [ ] Alt+Left / Alt+Right to swipe through response alternatives
  - [ ] Swipe count indicator (e.g. `2/4`) displayed correctly
  - [ ] Regenerate on swipe past end
- [ ] Ctrl+R full-text search overlay
  - [ ] Search input with live results
  - [ ] Results show message preview and role
  - [ ] Select result to scroll to that message
  - [ ] Escape to dismiss

---

## Milestone 6: Command picker

Opened with `/`. Flat, no deep submenus.

- [ ] `/compact` — compact conversation
- [ ] `/model` — opens model picker list
- [ ] `/character` — opens character picker list
- [ ] `/keepalive` — toggle cache keepalive (was `/cache`)
- [ ] `/retry` — retry most recently failed message
- [ ] `/regen` — regenerate last response, optionally with guidance text

---

## Milestone 7: Conversation & message management

- [ ] Chat management via picker UI
  - [ ] New conversation
  - [ ] Switch conversation (list + select)
  - [ ] List conversations
  - [ ] Current conversation info
- [ ] Inline message actions (accessible per-message, not via command)
  - [ ] Edit message
  - [ ] Delete message
  - [ ] Insert message
  - [ ] Detach message
- [ ] Fork conversation at depth N

---

## Milestone 8: Settings

A TUI-native settings panel for view/cosmetic preferences. Accessible via `/settings` or a keybind.

- [ ] Settings panel UI
- [ ] Max text width / margin size
- [ ] Timestamp display toggle
- [ ] Horizontal rules toggle
- [ ] Gap indicators toggle
- [ ] Token count display toggle
- [ ] Tool use verbosity toggle
- [ ] Reasoning/thinking display toggle
- [ ] Settings persisted to disk

---

## Milestone 9: Images

- [ ] `/attach <path>` to attach an image to the next message
- [ ] Kitty graphics protocol for inline image display (Ghostty)
  - [ ] Fallback: `[image: filename]` placeholder for unsupported terminals
- [ ] Handle `send_image` push events from daemon

---

## Milestone 10: Notifications

- [ ] `notify-send` integration for messages received while unfocused

---

## Requires upstream Shore changes **[upstream]**

These cannot be implemented until the Shore server adds support.

- [ ] **Memory agent TUI session** — currently `shore memory shell` is a Python-side loop. Needs a server-side session protocol so the TUI can drive it interactively.
- [ ] **Tool use push events** — streaming tool progress needs richer events (tool name, input, output) so the TUI can display them in real time.
- [ ] **Message IDs in history** — inline message editing requires the TUI to know each message's ID. History responses need to include them.
- [ ] **Guided regen via protocol** — the `guidance` field exists on regen requests; just needs a TUI UX for capturing it (likely a text prompt).
