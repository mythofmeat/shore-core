# shore-gui-godot: Future Ideas & Directions

## Approved for Implementation

### Typing Combo Escalation
The faster you type, the more the world reacts. All effects scale with typing speed via a unified "intensity multiplier" that ramps up during sustained typing and settles back over ~2 seconds when you stop.

- Fire cursor grows larger, particles multiply
- Screen shake begins (micro-tremors, not disruptive)
- Starfield rotation accelerates
- Ambient drone pitch climbs
- CRT distortion subtly increases
- Glow intensifies
- At extreme speeds (150+ WPM), everything is vibrating and screaming
- On pause, all effects tween back to baseline over 2s

Already have all the levers — this is just a multiplier on existing systems.

### Sound Reverb & Room Simulation
Route all audio through Godot audio bus effects. Context-sensitive reverb:

- **Streaming**: dry, close, present — sounds are "right here"
- **Idle**: more reverb, sounds drift further away, like an empty room
- **Error**: brief reverb spike, like a sound bouncing off walls
- Crossfade between reverb presets based on state
- Godot has built-in Reverb, Delay, Chorus, EQ effects on audio buses

### Text Glow Per Role
User and assistant text get distinct glow treatments, but not static colors — slow pulsing between customizable color sets:

- **User text**: slow pulse between 2-3 warm colors (configurable, defaults like amber/gold/soft orange)
- **Assistant text**: slow pulse between 2-3 cool colors (configurable, defaults like cyan/teal/soft blue)
- **Error text**: pulse between red tones
- Glow is a shader effect, not just BBCode color — actual soft light bleed around the text
- Pulse speed and color sets exposed in config panel
- Implementation: likely a custom shader on the RichTextLabel, or per-message SubViewport approach

### Dynamic Background Color Layer
Separate from the starfield — an additional color wash layer that shifts based on conversation context:

- Sits between the starfield and the UI (CanvasLayer 0 or 1)
- Smooth gradient that shifts hue over time
- Warms up when user is typing/sending
- Cools down during assistant responses
- Neutral during idle
- Could also react to message sentiment if we ever add that
- Implemented as a simple shader: two-color gradient with animated blend point

### Particle Weather System
Decorative ambient particles that fade in and out randomly — not tied to application state, purely atmospheric.

- Random events: particle bursts that fade in over ~2s, persist for 5-15s, fade out over ~2s
- Types (selected randomly per event):
  - Slow drifting motes (dust in sunlight)
  - Gentle falling particles (snow-like)
  - Rising sparks (ember-like)
  - Horizontal streaks (wind/rain-like)
  - Swirling vortex (brief, dramatic)
- Frequency slider in config: "never" to "constant" (controls average time between events)
- Each event picks a random type, random position, random color from a palette
- Multiple events can overlap
- Purely decorative — no meaning, just vibes

### Presets System
Named configurations that bulk-set all effect values. A dropdown in the config panel.

Preset ideas:
- **Synthwave**: purple/cyan palette, subtle CRT, loud ambient, heavy starfield, glow cranked
- **Retro Terminal**: heavy CRT, green-on-black, loud scanlines, no starfield, harsh clicks
- **Cozy**: warm colors, minimal CRT, soft clicks, gentle starfield, low ambient
- **Chaos**: everything maxed, shake cranked, VHS always simmering, particles everywhere
- **Professional**: everything off
- **Custom**: user's current manual settings (auto-saved)

Each preset is a dictionary of all export values. Selecting a preset applies them all. Tweaking any slider after selecting a preset switches to "Custom."

### More Dynamic Backgrounds
The starfield is one option. Want more procedural background options that can be swapped:

Ideas for additional background shaders:
- **Nebula**: slow-moving volumetric clouds with color gradients, parallax depth layers
- **Grid**: Tron-style perspective grid that stretches to infinity, subtle animation
- **Waveform**: undulating sine waves layered at different frequencies, ocean-like
- **Noise field**: flowing Perlin/simplex noise with color mapping, lava lamp energy
- **Circuit board**: procedural circuit-trace patterns that slowly grow and fade, data-flow aesthetic
- **Rain on glass**: droplets running down the screen with refraction, cozy rainy day mood
- **Aurora**: northern lights ribbons slowly dancing across the top of the screen

Background selector in config panel. Each is a separate .gdshader that gets swapped onto the background ColorRect.

---

## Rabbit Holes (Interesting but Dangerous)

### Procedural Music Generation
Instead of a static ambient drone loop, generate music in real time using Godot's audio system.

- Base: generative ambient music using layered oscillators
- Key/scale selection (minor for moody, major for upbeat)
- Tempo shifts based on activity (faster during streaming, slower during idle)
- Chord progressions that evolve over time (I-vi-IV-V with variations)
- Melody generation using constrained random walks within the scale
- Arpeggiation patterns that change with typing speed
- Could react to conversation content if sentiment analysis were added
- Godot's `AudioStreamGenerator` + `AudioStreamGeneratorPlayback` allows sample-level synthesis
- This is genuinely a multi-day project but the result would be incredible

### 3D Text Perspective
Messages rendered on slightly tilted planes in 3D space with depth-of-field blur on older messages. Scrolling moves through Z-space. Would require SubViewport rendering and fundamentally changes how messages work.

### ASCII Art Shader
A post-process shader that converts the entire rendered frame into ASCII characters. Everything still functions, it just looks like a 1980s terminal. Would need a character atlas texture and a shader that samples screen regions and maps brightness to ASCII glyphs.

### Audio Spectrum Visualizer
Real-time FFT visualization of the audio output in the header. Waveform or bar spectrum display that reacts to clicks, drone, and all sounds. Godot has `AudioEffectSpectrumAnalyzer` for this.

---

## Implemented (Session 2026-04-05)

### Bug Fixes
- **Unclosed BBCode on abnormal stream end**: `_stream_fx_open` flag, tags closed in `_on_disconnected()`, `_on_error()`
- **Volume slider propagation**: `_apply_toggles()` now updates all audio player volumes
- **Backspace inflating WPM**: `absf()` → `maxf(..., 0.0)` — only count forward typing
- **rain_warmth snap**: Binary toggle replaced with smooth lerp using delta
- **Scroll jitter during streaming**: Debounced to one scroll-to-bottom per frame via flag

### UX Improvements
- **Escape keybind**: Closes config panel first, cancels generation second
- **Auto-focus input on connect**: `input_field.grab_focus()` after connection
- **Wobble settle mechanic**: Text effect amplitude decays to 0 over 8-12s (performance + atmosphere)

### Atmospheric Additions
- **Shooting stars** in starfield shader: Rare bright streaks (3 independent cycles: 30s, 55s, 80s)
- **CRT phosphor grid**: Subtle RGB dot pattern visible only at barrel_distortion > 0.05
- **Lighthouse sweep haunting**: Warm light beam sweeps across rain_fog background over 4s
- **Distant Thunder haunting**: Low rumble + brief screen flash after 0.8s delay
- **Radio Fragment haunting**: AM-modulated mid-frequency burst, 0.7s, very quiet
- **VHS tracking bar variety**: Speed now noise-modulated with occasional sticking
- **Cursor Breath**: Caret alpha pulses slowly (3s period) when idle and focused
- **Warm rain tint**: rain_warmth uniform now actually affects shader output (was declared but unused)

### Phase 2 — UX Polish
- **Streaming indicator**: Animated dots ("generating...") during AI response, Send→Stop button swap
- **Enter-sends toggle**: Configurable enter-to-send with Shift+Enter for newlines
- **Config panel slide transition**: Panel slides in from right edge (0.2s ease-out), slides out on close
- **Slider value labels**: Dynamic labels next to each slider showing current value (% for volume, decimal for others)
- **Custom preset detection**: Toggling any setting away from preset auto-switches to "Custom"

### Phase 3 — Persistence & Atmosphere
- **Settings persistence**: All toggles, sliders, preset, and text settings save to `user://settings.cfg` via ConfigFile. Auto-saves on every toggle/slider change. Loads on startup before boot sequence.
- **Emotional resonance**: Long responses (>500 chars) trigger vignette breathe + ambient pitch drop + sub-bass tone. Short responses (<50 chars) get a glass tap + brightness flicker.
- **Time-of-day ambient shift (The Clock)**: Polls system time every 60s. Night: blue tint, 1.3x stars, lower ambient pitch. Dawn: warm transition. Day: neutral. Dusk: amber fade. Toggle in config panel.
- **Ghost Typing**: After 12s idle with empty input, phantom text fragments type out character-by-character in the placeholder. Pool of atmospheric phrases. Fades out after 3s hold. Instantly cleared on any input.

### Phase 4 — Interactivity & Audio Polish
- **Typing Combo Escalation**: Unified `_combo_intensity` (0.0-1.0) derived from typing speed (25 cps = 1.0). Scales: fire cursor size/amount, screen shake micro-tremors, CRT distortion creep (+0.08), glow swell, ambient pitch climb (+0.15), starfield rotation boost. Settles back over ~2s.
- **Context-sensitive reverb**: Effects bus reverb wet modulates based on state. Streaming/typing: 0.05 (dry, close). Idle >5s: 0.35 (drifting, room tone). Error: spike to 0.6 with 0.8s decay. Smooth crossfade via lerp.
- **Config section collapsibility**: Section labels (Shaders, Effects, Audio, Text) replaced with clickable buttons that toggle child visibility. Collapsed sections show `▸`, expanded show `▾`.
- **Boot sequence snap fix**: Eliminated lerp→hard-set discontinuity by letting lerp naturally reach targets.

---

## Design Principles
- Every effect must be independently toggleable
- Every intensity must be sliderable
- Nothing should require external assets (procedural generation preferred)
- The app must remain fully functional with all effects disabled
- Effects should enhance the experience without impeding usability
- When in doubt, add a slider
