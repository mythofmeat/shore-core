- [ ] shore matrix re-enable [./asap/shore-matrix.md]

- [ ] perform an adversarial steelman review of the critical review [./review/REVIEW.md]

- [ ] shore-web? [./ideas/shore-web.md]
# ux annoyances
- [ ] Escape conflict
  In a modal UI, Escape's job should be mode transition, full stop — overloading it with "cancel" is the bug. Proposal: Escape in insert mode only exits to normal mode; cancel is
  Ctrl+C (already global) or Escape-from-normal (already works). You pay one extra keystroke to cancel while typing, and Ctrl+C is the fast path. Predictable, matches vim muscle
  memory.

- [ ] Regen spinner
  Found the asymmetry. Send sets stream.active = true optimistically and pushes a placeholder entry the moment Enter is pressed (input.rs:330). Regen doesn't — it waits for the
  daemon's StreamStart to flip the flag (main.rs:601). Any daemon latency = regen looks frozen. Fix is surgical: set stream.active = true in the Regen send path, same as Send.
  Possibly also push a placeholder "regenerating…" entry in the slot of the removed assistant message so the layout doesn't jump.

- [ ] Persistent system notifications
  Right now every error, reconnect attempt, cache warning, and "connected" message becomes a ConversationEntry::System appended to the chat log with no lifecycle. Two-part
  proposal:

  - Status line at the bottom of the TUI for transient/connectivity state: connection indicator + last reconnect reason. Replaces in place, so reconnect storms dedupe naturally.
  - Inline system entries only for real errors (LLM failure, command error), plus a keybinding or command (:clear) to dismiss them.

  Tradeoff: you have to classify system messages by severity — not every set_status call belongs inline. Worth it, since today the log becomes unreadable exactly when you most need
   it (flaky connection).
