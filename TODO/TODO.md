- [ ] shore matrix re-enable [./asap/shore-matrix.md]

- [ ] perform an adversarial steelman review of the critical review [./review/REVIEW.md]

- [ ] shore-web? [./ideas/shore-web.md]
# ux annoyances
- [ ] Persistent system notifications
  Right now every error, reconnect attempt, cache warning, and "connected" message becomes a ConversationEntry::System appended to the chat log with no lifecycle. Two-part
  proposal:
  - Status line at the bottom of the TUI for transient/connectivity state: connection indicator + last reconnect reason. Replaces in place, so reconnect storms dedupe naturally.
  - Inline system entries only for real errors (LLM failure, command error), plus a keybinding or command (:clear) to dismiss them.

  Tradeoff: you have to classify system messages by severity — not every set_status call belongs inline. Worth it, since today the log becomes unreadable exactly when you most need
   it (flaky connection).
