# shore-gui: Future Ideas & Directions

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

## Design Principles
- Every effect must be independently toggleable
- Every intensity must be sliderable
- Nothing should require external assets (procedural generation preferred)
- The app must remain fully functional with all effects disabled
- Effects should enhance the experience without impeding usability
- When in doubt, add a slider
