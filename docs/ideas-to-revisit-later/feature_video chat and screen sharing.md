# Video Chat And Screen Sharing

Status: idea, not current implementation.

Potential future direction: let a character see a live screen/window or participate in a video/audio session.

Hard constraints:

- must be explicit opt-in per session
- must clearly show when capture is active
- captured content must not automatically become long-term memory
- any memory writes must be deliberate markdown writes
- daemon remains authoritative; UI capture clients only provide media/context

Open questions:

- whether this belongs in the Tauri GUI, Godot GUI, or a separate capture helper
- provider support for real-time multimodal input
- cost ceilings and privacy controls
- how much context can be cached safely
