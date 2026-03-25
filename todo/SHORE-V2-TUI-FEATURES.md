# Shore TUI — Feature Backlog

Features beyond what US-033 delivers (persistent connection, basic conversation
view, streaming, status bar, inline images, basic keybinds). Use this to create
the TUI PRD after Phase 2.

---

## Color Palette

Custom palette — muted, cold, comfortable for long sessions:

- **Background:** very dark blue-gray (not pure black, not sterile)
- **Primary text:** silvery, slightly muted and cool (tarnished silver / morning fog)
- **Accent:** dusty muted lavender (bruise-before-it-fades purple, not bright)
- **Secondary accent:** desaturated teal / ocean-glass green
- **Error/alert:** dull amber or burnt orange (not red)
- **Vibe:** slightly cold, overcast, muted. Like looking out a window on an
  overcast day. Nothing neon, nothing warm, nothing that screams "GAMER" or
  "HACKER."

---

## Input & Editing

| Feature | Description |
|---------|-------------|
| Input history recall | Up/Down arrows browse previously sent messages; Down past end restores draft |
| Open in $EDITOR | Ctrl+G suspends TUI, opens $EDITOR for composition, resumes with content; falls back to vi |
| Mouse scroll | Scroll wheel moves conversation 3 lines; mouse events in input area don't scroll conversation |

## Message Display

| Feature | Description |
|---------|-------------|
| Model name in header | Show which model produced each assistant message (toggleable, default off), dimmed after timestamp |
| Toggleable timestamps | Toggle timestamp display in message headers (default on), persists to settings |
| Horizontal rules | Optional dimmed `─` separator between messages instead of blank line (default off) |
| Gap indicators | Dimmed "— 3 hours later —" when idle gap > 30 min; human-readable minutes/hours/days (default on) |
| Token count display | Optional `↑123 ↓456 ♻789` in header showing input/output/cache tokens (default off) |
| Configurable margins | `conv_margin` setting (default 4), shrinks gracefully on terminals <60 cols, adjustable via Ctrl+\[/Ctrl+\] |

## Markdown Rendering

| Feature | Description |
|---------|-------------|
| Inline formatting | **bold**, *italic*, ~~strikethrough~~, `inline code` with colors/modifiers; pulldown-cmark |
| Fenced code blocks | Distinct background color, language label top-right (dimmed), syntax highlighting via syntect matching the project palette, no word-wrap |
| Headers | H1-H3 with distinct colors from palette |
| Lists | Bullet (•) and numbered lists |
| Blockquotes | `│` prefix in muted color |
| Links | URL shown dimmed |

## Navigation & Search

| Feature | Description |
|---------|-------------|
| Swipe / response alternatives | Alt+Left/Alt+Right navigates variants; "2/4" counter near header; swiping past end triggers regen; resets on new user message. Uses `alt_index`/`alt_count` from Message object. |
| Full-text search overlay | Ctrl+R opens centered overlay; real-time case-insensitive substring search; results show role+timestamp+preview; Enter scrolls to match; Escape closes |

## Command Picker

| Feature | Description |
|---------|-------------|
| Picker infrastructure | Type `/` in empty input to open filterable list; Up/Down select, Enter execute, Escape cancel; secondary lists for sub-selection |
| /compact | Trigger compaction; shows "Compacting conversation..." until history event returns |
| /model | Fetch available models via `list_models`, show current, switch on selection via `switch_model` |
| /character | Fetch character list via `list_characters`, show current, switch via `switch_character` |
| /keepalive | Toggle cache keepalive |
| /retry | Resend last failed message |
| /regen | Optional guidance text prompt before sending regen (uses `guidance` field on regen message) |

## Conversation Management

| Feature | Description |
|---------|-------------|
| Conversation picker | /conv or Ctrl+N (new) / Ctrl+S (switch); sub-options for new/switch/info; switch fetches list via `list_chats` and shows picker; info shows popup with conversation metadata |
| Inline message actions | Press e to enter focus mode (highlighted border); Up/Down moves between messages; e=edit, d=delete (confirm), i=insert before, D=detach; Escape exits focus mode. Uses `msg_id` from Message object for `edit`/`delete` commands. |
| Fork conversation | /fork in picker; prompts for optional name/depth; sends `chat_fork` command; confirms and switches to new fork |

## Settings

| Feature | Description |
|---------|-------------|
| Settings persistence | Settings struct saved to `$XDG_DATA_HOME/shore-tui/settings.json`; loads on startup, saves on change; ignores unknown fields |
| Settings panel | /settings or Ctrl+,; toggles for timestamps, rules, gaps, thinking, tokens, model display, tool verbosity; margin control; Space/Enter toggles; immediate apply + save; Escape closes |

## Images

| Feature | Description |
|---------|-------------|
| /attach command | /attach \<path\> or file path prompt; shows `[image.png]` badge in input; multiple attachments; sent as `images[]` in message |
| Inline image rendering | Kitty graphics protocol; fallback to `[image: filename]` on unsupported terminals; APC query for detection; scale to content_width, max 20 lines; scroll accounts for image height |
| Server-pushed images | Handle `send_image` push events; render inline with caption |

## Notifications

| Feature | Description |
|---------|-------------|
| Desktop notifications | notify-send when message arrives while unfocused (FocusGained/FocusLost tracking); title=character name, body=first 120 chars; toggleable (default on); failure silently ignored |

## Stretch Goals

| Feature | Description |
|---------|-------------|
| Memory agent session | Interactive memory shell within TUI. Requires additional protocol messages (deferred in SWP §3.8). |
