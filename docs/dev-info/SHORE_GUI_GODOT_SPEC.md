# shore-gui-godot: The Most Overengineered Chat Client Ever Made

> A GPU-accelerated, shader-heavy, physics-enabled GUI for Shore, built in Godot + gdext (Rust bindings). It does everything the TUI does, but with completely unnecessary visual flair that requires hardware acceleration.

## Tech Stack

- **Engine:** Godot 4 (GDScript for UI/effects, visual editors for shaders/particles/audio)
- **Bridge:** gdext (Rust) wrapping `shore-client` for SWP communication
- **Backend:** Existing `shore-daemon` over SWP (no changes needed)

## Architecture

```
shore-daemon <--SWP--> [gdext Rust bridge] <--signals/calls--> [Godot scene tree]
```

The Rust bridge is a single class exposing:
- `connect(endpoint)` / `disconnect()`
- `send_message(text)`
- `signal token_received(text)`
- `signal message_complete()`
- `signal error(message)`
- Context window / session state as needed

Everything else — rendering, shaders, audio, physics — lives in Godot.

---

## Functional Requirements (The Part That Actually Matters)

These features replicate the TUI and make it a usable chat client:

### Chat Interface
- Scrollable message history (user messages and AI responses)
- Rich text rendering (markdown: bold, italic, code blocks, inline code, lists, headers)
- Streaming token display (tokens appear as they arrive, not on completion)
- Multi-line text input with standard keybindings
- Copy/paste support
- Message selection and copying

### Session Management
- Connect to / disconnect from shore-daemon
- Character selection
- Conversation history persistence (whatever the daemon provides)
- Session metadata display (model, character, token counts)

### Tool Interaction
- Display tool calls and their results
- Tool approval/denial flow (matching TUI behavior)

### Context Window
- Display current context usage
- Clear visual indicator when approaching limits

### Status / Diagnostics
- Connection status indicator
- Streaming state indicator
- Error display

---

## Bells and Whistles (The Actual Point)

### Shaders

**CRT Scanline Filter**
- Subtle scanline overlay on all text
- Slight screen curvature / barrel distortion
- Toggleable — on by default because of course it is

**Chromatic Aberration**
- Subtle baseline chromatic aberration on text
- Intensifies progressively as a response gets longer
- Resets on message completion

**Bloom / Glow**
- Each token glows briefly as it streams in, then fades to normal
- Code blocks have a persistent subtle glow behind them
- The input cursor has a soft bloom

**VHS Tracking Artifacts**
- Brief VHS-style distortion effect as a transition between messages
- Horizontal line displacement, color bleeding, brief static burst

**Matrix Rain**
- Code blocks render with a faint Matrix-style falling character rain behind them
- Characters in the rain are sampled from the actual code content

### Particles

**Fire Cursor**
- The text input cursor is a particle emitter
- Small, warm flame effect — not overwhelming, just enough to say "this cursor is on fire"
- Flame color shifts with typing speed (cool blue at rest, hot white when fast)

**Message Completion Burst**
- Subtle particle burst when a response finishes streaming
- Like a soft firework or sparkle dissipation

**Error Crumble**
- Error messages crack and crumble apart with debris particles
- Pieces fall with gravity, fade out

**Token Stream Trail**
- Faint particle trail behind each token as it appears
- Like a comet tail that fades quickly

### Audio

**Token Streaming**
- Soft mechanical key click per token
- Pitch varies by character class:
  - Lower pitch: punctuation, brackets
  - Mid pitch: consonants
  - Higher pitch: vowels
  - Distinct click: spaces (softer, like a space bar)
- Subtle randomization in pitch/timing to avoid sounding robotic

**Message Events**
- Satisfying *thunk* when a message completes
- Vinyl scratch on errors
- Soft *whoosh* on message send

**Ambient**
- Low synthwave drone in the background
- Pitch subtly shifts based on streaming state (slightly higher during active generation)
- Volume auto-adjusts — barely audible, more felt than heard

**Startup**
- Boot sequence sound on launch (retro console energy)
- Brief CRT power-on visual to accompany it

**Typing Feedback**
- Keypress sounds as the user types
- Mechanical keyboard feel — tactile and satisfying
- WPM-responsive: faster typing = slightly more energetic sound profile

### Physics

**Message Gravity**
- Old messages are rigid bodies in a physics sim
- Scroll normally by default, but a toggle enables physics mode
- In physics mode: messages stack with gravity, can be grabbed and flung off-screen
- Flung messages fade out with a trailing particle effect

**Error Ragdoll**
- Failed/error messages don't just crumble — the text box cracks, splits into chunks, and ragdolls
- Chunks collide with other messages on the way down

**Minimize Collapse**
- On window minimize, all messages fall to the bottom of the viewport
- On restore, they float back up to position

**Screen Shake**
- Subtle screen shake on errors
- Micro-shake on message completion (barely perceptible, adds weight)

### Visual Flourishes

**Background**
- Slowly rotating starfield or nebula — procedurally generated, not a texture
- Subtle parallax shift when scrolling

**Typing Speed Display**
- WPM counter in the corner during typing
- Flame effect on the counter when exceeding a threshold
- Completely useless information displayed with maximum drama

**Context Window Health Bar**
- Context usage displayed as an RPG-style health bar
- Green -> yellow -> red gradient as it fills
- Starts pulsing and glowing when above 80%
- Dramatic low-health heartbeat sound when above 90%

**Konami Code Easter Egg**
- Activates a hidden shader mode
- What it does is left as an exercise for the developer at 3am

---

## Audio/Visual Master Controls

All effects should be independently toggleable and have intensity sliders:
- Master effects toggle (one key to disable everything for "professional mode")
- Shader intensity (0–100%)
- Particle density (0–100%)
- Audio master volume
- Physics enable/disable
- Individual toggles for each major effect category

"Professional mode" strips it back to a clean, minimal chat GUI with none of the effects. It should still look good without them — the effects are seasoning, not structure.

---

## Non-Goals

- No additional LLM API calls for any visual/audio feature
- No gameplay mechanics (achievements, XP, leveling) — the time sink is building the effects, not maintaining a progression system
- No network features beyond existing SWP communication
- No mobile support (this thing requires a GPU and that's the point)
