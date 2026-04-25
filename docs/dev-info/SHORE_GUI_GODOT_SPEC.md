# Shore GUI Godot Notes

`shore-gui-godot` is experimental. It is not the canonical client, and it must not own character state.

Current architectural rule:

- Godot UI talks to the daemon like every other client.
- Conversation, memory, autonomy, and model state stay in the daemon.
- The GUI may keep presentation state: animation, ambience, layout, local settings.

Design direction:

- atmospheric, companion-like UI
- useful for long-running character presence
- not a marketing/landing-page surface
- no alternate memory implementation

Verification expectations:

- can connect to a running daemon
- renders authoritative history without duplicating streamed turns
- handles reconnects
- does not mutate character files except through daemon commands/tools

This document is intentionally light until the Godot client becomes release-relevant.
