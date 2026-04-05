# Shore GUI: Creative Proposal

## New Feature Ideas

### 1. Breath Fog on Idle

When the user stops typing for 60+ seconds, the screen slowly fogs up from the bottom — not the existing condensation system, but an actual warm-breath effect. Two translucent plumes rise from the bottom corners of the screen (where the user's "face" would be), expand, and slowly occlude the lower third. The fog is warm-tinted (slight amber) and animated with slow turbulence noise. Breathing in is a 3s expansion, breathing out is a 5s contraction, repeating in a slow rhythm. The effect stacks with the existing condensation system — you're sitting at a cold window, breathing on the glass, AND the glass is fogging up from humidity.

Wiping works on this too. Clicking through breath fog leaves a warm smear that refracts slightly differently than the cold condensation wipe.

Technical approach: New shader layer (`breath_fog.gdshader`) at CanvasLayer 8, using animated Perlin noise with a directional bias (upward from two source points). Driven by the glass_manager idle timer. Blends multiplicatively with the condensation layer rather than additively.

### 2. Message Decay / Ink Bleed

In the Silent Shore preset, old messages (more than ~20 visible messages ago) slowly degrade. The text doesn't disappear — it bleeds. Characters develop slight color drift, like ink running in water. Individual characters occasionally shift 1-2px in random directions. The effect is strongest on the oldest visible messages and completely absent on recent ones.

This creates the feeling that the conversation is physically deteriorating — that you're reading notes left on a wet surface. Combined with the existing droplet overlay, old messages would look like rain-soaked paper.

Technical approach: A custom RichTextEffect (`ink_bleed.gd`) that takes a `message_age` parameter. Characters with age > threshold get progressive offset jitter (seeded by char index for stability — same char always drifts the same way), alpha variation, and slight hue rotation. The message display would need to tag older messages with their age.

### 3. Tidal Pull — Scroll Momentum with Character

Replace the default scroll behavior with a physics-based scroll that has "weight" to it. Scrolling up feels like pulling against a current — there's resistance, a slight overshoot at the top, and the starfield/rain parallax exaggerates the motion. Scrolling down is effortless, like being carried by water. The scroll velocity affects rain angle and star rotation speed in real time.

When you reach the very top of the conversation (the oldest messages), the background shifts slightly — darker, colder, like you've gone deeper. When you snap back to the bottom, there's a brief "surfacing" flash of brightness.

Technical approach: Override ScrollContainer scroll behavior in main.gd with a custom physics-based scroll that applies asymmetric friction. Pass scroll velocity to effects_manager, which feeds it into rain_angle and starfield rotation_speed as additive offsets. The depth-shift is a color uniform on the background shader driven by normalized scroll position.

### 4. Ghost Typing — Phantom Characters in the Input Field

In the Silent Shore preset, when you pause mid-sentence for 5+ seconds, faint gray characters occasionally appear ahead of your cursor — as if the chat window is trying to finish your thought. They're never real suggestions. They're fragments: a word, a trailing ellipsis, sometimes just "..." that fades in and out. They vanish the instant you type again.

This is purely atmospheric. The ghost text is generated from a small pool of fragments ("...", "and then", "I think", "maybe", "but", the character's name) and rendered in the input field at very low opacity (0.15 alpha) using the phosphor effect's glow color.

Technical approach: A timer in main.gd that fires after 5s of input-field inactivity. Appends ghost text at cursor position using input_field BBCode (or overlay Label), fades it with a Tween. Any InputEventKey clears the ghost instantly.

### 5. Lighthouse Sweep

A slow, periodic light sweep across the screen in Silent Shore mode. Every 30-60 seconds, a soft beam of warm light rotates across the window from left to right (or right to left), as if a distant lighthouse is somewhere out beyond the rain. The sweep takes about 4 seconds, is very soft and diffuse, and brightens the rain streaks it passes through.

Technical approach: A new shader uniform on the rain_fog shader: `lighthouse_angle` (animated 0.0 to 1.0 by effects_manager). The fragment shader adds a soft gradient beam (smoothstep on distance-from-line) that brightens existing color by ~15%. The haunting_manager triggers this as a new haunting type with weight 2.

### 6. Drowned UI — Underwater Mode

An unlockable/secret preset. The entire UI renders as if submerged: slow caustic light patterns play across all text and UI elements, text wobbles more aggressively (water refraction), ambient audio switches to muffled low-pass with bubble sounds, and typing produces bubble particle effects instead of fire. The background is a deep blue-green gradient with slowly undulating light pillars.

The entry animation: the screen "fills" from the bottom up with a blue wash over 3 seconds, and all audio crossfades from dry to wet (reverb + LPF ramp). Switching away reverses it — the water "drains."

Technical approach: New preset in PRESETS dict. New background shader (`deep_water.gdshader`) using animated Voronoi noise for caustics. Reuse the existing reverb/LPF bus but with more extreme settings (cutoff 800Hz, reverb wet 0.6). Replace fire cursor particles with bubble particles (round, slow, rising). The caustic overlay is a separate CanvasLayer shader that modulates existing screen content.

### 7. The Clock — Time-of-Day Ambient Shift

The app subtly shifts its ambient mood based on the user's local time. Not dramatically — no "dark mode at night" — but:

- **Dawn (5-7am)**: Slightly warmer background tint, ambient pitch rises slowly
- **Midday (11am-2pm)**: Starfield is dimmer (washed out by implied sunlight), rain is brighter
- **Dusk (6-8pm)**: Purple/orange tint bleeds into background shader, ambient gets richer harmonics
- **Night (10pm-3am)**: Everything slightly darker, stars brighter, rain sounds closer, occasional owl hoot in haunting pool
- **Deep night (3-5am)**: Special unlisted behaviors — phosphor linger is always slightly active, a very rare haunting plays a distant ship horn

This should be opt-in (a "Time Sync" toggle in config) and override-able. The user should never feel forced into a mood by their clock.

Technical approach: `Time.get_datetime_dict_from_system()` polled once per minute. Time-of-day maps to a normalized value that modulates existing shader uniforms (bg_color tint, star_density multiplier, rain_warmth) and audio parameters (ambient pitch offset, reverb wetness).

### 8. Emotional Resonance — Response Vibration

When the AI completes a long response (500+ chars), the screen doesn't just do the completion burst — it resonates. A very low-frequency vibration (sub-visual, felt more than seen) pulses through the UI for 2 seconds: the vignette breathes in and out once, the ambient drone swells and recedes, and there's a single deep tone (~40Hz, 200ms, barely audible). It feels like the response landed with weight.

For very short responses (<50 chars), the opposite: a light, crisp micro-sound (like a single raindrop hitting glass) and a tiny CRT flicker. The response barely disturbed the surface.

Technical approach: In on_stream_end(), check _stream_char_count. Long: tween vignette_strength up 0.15 and back over 2s, pitch-shift ambient down 0.95 and back, play a sub-bass tone. Short: play glass_tap at high pitch, 1-frame CRT brightness bump.

---

## Existing Feature Improvements

### Rain Fog Shader (`rain_fog.gdshader`)

**Replace with SDF rain-on-glass approach.** The current rain uses grid-based random streaks — functional but flat. The godotshaders.com "Rain on Glass" shader uses SDF egg shapes for tear-drop geometry, layered dynamic drops with gravity trails, and actual refraction of the screen behind them via `textureLod()` blur. This would transform rain from "parallel lines falling" to "water running down a window with visible droplets that distort what's behind them."

Adaptation needed: The Rain on Glass shader is a screen-space post-process shader. For Shore GUI, it should be adapted to work as a background shader (sample a pre-rendered background texture rather than SCREEN_TEXTURE), or applied as a post-process CanvasLayer on top of the existing background. The `rain_amount` uniform maps directly to the existing `rain_density` slider. Add `rain_warmth` modulation to the blue_amount uniform.

Keep the existing fog layers — they're good. Layer the SDF rain ON TOP of the fog, so you see droplets running down a fogged window.

### Glass Cracks (`glass_cracks.gdshader`)

**Add refraction distortion to cracks.** Currently cracks are purely visual (white lines). Real cracked glass refracts light along the fracture. The crack_pattern function should output a normal map perturbation that gets applied via SCREEN_TEXTURE displacement — each crack line slightly shifts the image behind it, creating a prism-edge effect. Fresh cracks (age near 0.0) refract more; healing cracks refract less.

**Add spider-web secondary fractures.** When a crack heals, there's a 20% chance it leaves behind a faint spider-web pattern (concentric rings only, no radial rays) that persists for 3x longer. This rewards repeated interaction — the glass develops a history.

### Condensation (`condensation.gdshader`)

**Add finger-trail streaks.** Currently, wiping clears a circular zone. Real condensation wiping leaves a streak trail as you drag. Track mouse motion during click-drag and pass the trail as a series of UV points. The shader clears condensation along the trail path with a finger-width (0.04 UV) soft edge. The trail slowly re-fogs over 12 seconds, from the edges inward.

**Add writing-in-condensation.** If the user slowly drags across fogged glass, they should be able to "write" in it — the clear trail persists longer for slow drags (the slower you move, the wider and more persistent the clear zone). This emerges naturally from the streak system if the decay rate is inversely proportional to drag speed.

### CRT Shader (`crt.gdshader`)

**Add subtle phosphor grid texture.** Real CRTs have a visible RGB phosphor dot pattern when viewed closely. Add a repeating micro-pattern (3px period) that's visible only when `barrel_distortion > 0.05` — three tiny colored dots (R, G, B) repeated across the screen, at about 0.03 opacity. This is the kind of detail you notice on the third day.

**Add screen curvature reflection.** A very faint gradient at the edges of the barrel distortion that suggests light reflecting off the curved glass surface. Brightest at the top-left corner (implied overhead light source), about 0.05 opacity. Makes the CRT feel like a physical object rather than a flat filter.

### Starfield (`starfield.gdshader`)

**Add shooting stars.** Rare (one every 30-90 seconds), a bright streak crosses the field over ~0.5 seconds. It's a bright point that leaves a decaying trail, moving in a consistent direction. The trail fades over 0.3 seconds behind the point. This is the kind of detail that makes someone pause mid-sentence.

**Add depth fog.** The faintest stars (the 80.0-scale layer) should fade slightly into a background haze — not sharply visible, but suggesting depth. A per-layer fog multiplier that's 1.0 for nearby stars and 0.6 for distant ones, modulated by a slow noise function.

### Seagull System (`seagull_manager.gd`)

**Better seagull silhouettes.** The current V-shape is functional but minimal. Replace SeagullDraw with a slightly more detailed silhouette: add a head dot, tail extension, and make the wing shape curved rather than straight lines. Still simple (5-6 draw calls), but reads as "seagull" rather than "checkmark." The wing flap should be asymmetric — wings up longer than wings down, with a slight pause at the apex.

**Seagull cries.** 30% of seagulls that fly across should emit a distant call. Generate a simple seagull cry: two frequency-swept tones (800Hz->1200Hz over 0.2s, repeated twice with a 0.1s gap), very quiet (-28dB), with slight pitch variation per bird. Play when the seagull is at ~40% of its flight path (middle of the screen).

### Haunting System (`haunting_manager.gd`)

**Add new haunting: "Distant Thunder."** A very low rumble (25-35Hz, 1.5s duration with 0.5s attack, 0.8s sustain, 0.2s decay) followed 0.5-2s later by a brief screen flash (CRT brightness bumps to 1.3 for 100ms, then decays back over 300ms). Weight: 1 (rare). This is the kind of thing that makes you look up from the keyboard.

**Add new haunting: "Radio Fragment."** A 0.5-1s burst of what sounds like a distant radio — mid-frequency sine waves with aggressive AM modulation (15-25Hz) and slight frequency drift. Like catching a fragment of a transmission. Volume at -30dB. Weight: 1. Sometimes the fragment is just static; sometimes it has a tonal quality that almost sounds like speech.

**Add new haunting: "Tide Shift."** The rain_fog background shader's fog_depth slowly increases by 0.15 over 8 seconds, then slowly returns over 12 seconds. During the shift, the ambient audio pitch drops by 0.03. The overall effect: the fog rolls in, things get quieter and heavier, then it passes. Weight: 2.

### Phosphor Effect (`phosphor_effect.gd`)

**Add per-character color temperature variation.** During linger_active, each character's glow should have a slightly different warmth — some lean amber, some lean blue-white. Seeded by character index so it's stable. This mimics real phosphor irregularity where different dots on a CRT age differently.

### Thought Lights (`thought_lights.gdshader`)

**Add light interaction with rain.** When rain_fog is the active background, the thought lights should occasionally "catch" a raindrop — when a light's position coincides with a rain streak position, the light briefly flares brighter (1.5x for 0.1s) and spawns a tiny ripple. This requires passing rain streak positions to the thought_lights shader as additional uniforms, or combining them into a single shader pass.

---

## Addon Recommendations

### 1. GodotSynth — Procedural Audio Engine

**What it enables:** Replace all the hand-rolled AudioStreamWAV generation functions (~400 lines in effects_manager.gd) with a proper synthesis engine. GodotSynth provides virtual analog oscillators, ADSR envelopes, filters, LFOs, reverb, delay, and distortion — all controllable via GDScript API. This means richer sounds with less code, plus the ability to modulate sounds in real time (e.g., typing speed drives filter cutoff on the ambient drone).

**Critical for:** The procedural music generation idea from IDEAS.md. Without a synthesis library, building a generative ambient system from AudioStreamGenerator sample-by-sample is a multi-day grind. GodotSynth gives you oscillators, envelopes, and effects out of the box.

**Installation:**
```
git clone https://github.com/EclipsingLines/GodotSynth
cp -r GodotSynth/addons/GodotSynth shore-gui/addons/GodotSynth
```
Enable in Project > Project Settings > Plugins.

### 2. Godot Post Process Addon — Unified PostFX Stack

**What it enables:** A managed post-processing stack with vignette, film grain, chromatic aberration, color correction, and blur — all as stackable, independently toggleable effects with inspector controls. This would let you layer film grain on top of the CRT effect, add a color grading LUT for different presets, and manage the PostFX pipeline without manually stacking CanvasLayer shaders.

**Critical for:** The color grading / LUT approach would transform preset creation. Instead of hardcoding color values per preset, you could define a LUT texture per mood and swap it. "Synthwave" becomes a LUT. "Silent Shore" becomes a LUT. Instant, consistent, professionally tunable.

**Installation:**
```
git clone https://github.com/GodotPostProcess/addon
cp -r addon/addons/PostProcess shore-gui/addons/PostProcess
```

### 3. Anima — UI Animation Library

**What it enables:** 89 built-in animation presets (fade, slide, bounce, pulse, shake, etc.) with CSS-like syntax for sequencing. Parallel and sequential animation composition. This would dramatically improve UI transitions: config panel slide-in, message fade-in, status label animations, preset transition crossfades.

**Critical for:** The config panel currently just appears/disappears with `queue_free()`. Anima gives it a slide-in from the right with a single line. Message entry animations could go from the current simple `[fadein]` BBCode to a full slide-up-and-fade with per-character staggering.

**Installation:**
```
git clone https://github.com/ceceppa/anima-godot-4
cp -r anima-godot-4/addons/anima shore-gui/addons/anima
```

### 4. Color Correction and Screen Effects

**What it enables:** Video-editor-quality color correction as visual shaders: brightness/contrast/saturation curves, HSV manipulation, color balance (shadows/midtones/highlights), channel mixer. Each effect is a separate visual shader that can be applied to a CanvasLayer.

**Critical for:** Making presets look dramatically different from each other without touching individual shader uniforms. A warm-toned Silent Shore vs. a cool-toned Default vs. a high-contrast Synthwave — all achievable by swapping a single color correction shader.

**Installation:**
```
git clone https://github.com/ArseniyMirniy/Godot-4-Color-Correction-and-Screen-Effects
cp -r Godot-4-Color-Correction-and-Screen-Effects/addons shore-gui/addons/
```

---

## Mood/Preset Ideas

### "Deep Current"
Everything is slow and heavy. Background: deep blue-black gradient with undulating caustic light patterns (the deep_water shader at very low intensity). CRT is on but with minimal scanlines — just vignette and a slight amber tint. Rain is off. Ambient audio: a very low drone (~40Hz fundamental) with slow filter sweeps. Typing sounds are deep, muffled, distant. No fire cursor — instead, the cursor position emits a faint glow that bleeds into surrounding text. Hauntings include whale-song fragments (frequency-swept low tones, 2-3 seconds, very quiet). Feels like: chatting from the bottom of the ocean. Everything above is far away.

### "Static"
Maximum noise. Background: aggressive TV static (white noise shader — no stars, no rain, just animated Perlin noise at high frequency). CRT cranked with heavy scanlines and strong barrel distortion. All audio has a background hiss (add white noise to the ambient bus). Typing sounds are harsh clicks. Fire cursor is bright white. VHS intensity idles at 0.2 instead of 0.0 — always slightly unstable. Hauntings: "signal found" moments where the static briefly resolves into coherent imagery (the background shader briefly switches to starfield for 0.5s, then snaps back to static). Feels like: tuning a radio in the void. You're searching for something.

### "Greenhouse"
Warm and alive. Background: slow-drifting Perlin noise in greens and ambers, like sunlight through leaves. No CRT effects. Glow is warm gold. Typing sounds are soft taps — almost like rain on leaves. Ambient audio: layered nature sounds (synthesized — not samples). Crickets (high-frequency oscillation), wind (filtered noise), distant water (the ocean generator pitched up). Fire cursor becomes a pollen particle effect — slow, floating, golden. Hauntings are replaced with "nature events": a butterfly crosses the screen (simple sprite, like the seagull system but with different geometry), a gust of wind shifts all background particles, birdsong fragments. Feels like: sitting in a sunlit conservatory having a conversation.

### "Void"
Nothing. Literally nothing. Background: pure black. No CRT, no glow, no VHS, no particles, no ambient audio. Text is white on black. But — and this is the trick — every 30-60 seconds, something appears for exactly one frame. A star. A crack. A word. A color. Then it's gone. The user questions whether they saw it. Over time (10+ minutes of use), the events get slightly more frequent and slightly more persistent (2 frames, then 3). The void is waking up. Feels like: being the first person to turn on a computer that's been sleeping for a thousand years.

### "Retrowave"
Distinct from "Synthwave" — this is specifically 1980s. Background: a perspective grid (Tron-style) that stretches to a vanishing point, with horizontal lines scrolling toward the viewer. Color palette: hot pink, electric cyan, deep purple. CRT is moderate. A sunset gradient sits behind the grid (orange-to-purple, top to bottom). Stars visible above the "horizon line." Ambient audio: arpeggiated synth pad (would benefit from GodotSynth). Text glow is neon pink for user, cyan for assistant. Fire cursor is electric blue. Feels like: a conversation happening inside an album cover.

### "Manuscript"
Paper aesthetic. Background: warm off-white (#f5f0e0) with subtle paper grain (high-frequency low-amplitude noise). No CRT, no glow. Text is dark brown (#3a2f25) in a serif font (if available, otherwise default). No sound effects except very quiet pen-scratch sounds on typing (short, breathy noise bursts with high-pass filter). No ambient audio. Margins are slightly inset. The feel is intentionally quiet and analog. Scrolling has slight inertia, like pulling a scroll. Feels like: passing notes in a library.

---

## Ambient Detail Ideas

### Cursor Breath
When the input field has focus but no typing is happening, the text cursor pulses with a very slow brightness oscillation (3-second period). Not a blink — a breath. The brightness moves between 0.6 and 1.0 alpha. This makes the idle state feel alive rather than dead.

### Message Gravity
New messages don't just appear — they settle. When a new message is appended to the display, it starts 2px above its final position and drops into place over 0.3 seconds with a slight overshoot (settles at -0.5px, then springs back to 0). This is so subtle that most users won't consciously notice it, but removing it would make the UI feel "off."

### Rain Puddle in Header
In Silent Shore mode, the header area (status label + WPM) has a very thin reflection effect. A 2px-high strip below the header text shows a faint, distorted, vertically-flipped copy of the text above. As if there's a thin puddle on the header shelf. The reflection wobbles slightly with rain_intensity.

### Distant Ship Lights
In Silent Shore mode, occasionally (every 2-5 minutes) a tiny dim dot of warm light appears near the horizon line of the rain background and slowly moves horizontally across the screen over 30-60 seconds, then fades. It's a ship in the distance. Too small to have detail. Just a light. Sometimes two lights (one white, one red — port and starboard). Most users will never notice.

### Typing Warmth
The background very subtly shifts warmer (a tiny amount of red/orange added to bg_color) when the user has been typing for 10+ consecutive seconds, and slowly cools back when they stop. The shift is so small (~0.02 in the red channel) that it's invisible if you look for it, but a user comparing "I was just typing" vs. "I haven't touched the keyboard in a minute" would see different screens.

### Ghost in the Header
Once every 4-8 hours of continuous use, the character name in the status label briefly flickers to a different name — the name of a character that doesn't exist. It lasts exactly 2 frames, then snaps back. If the user blinks, they miss it. There is no explanation. There is no configuration for this. It just happens.

### Starfield Constellation Flash
Every 2-3 minutes in the starfield background, 4-7 nearby stars briefly brighten simultaneously for 0.5 seconds, as if briefly forming a constellation pattern. They don't draw lines — just glow together and then dim back. It suggests pattern without confirming it.

### Input Shadow
The text in the input field casts a very faint shadow — 1px down and right, at 0.08 alpha, in a slightly warmer color than the text. This makes the input text feel like it sits slightly above the surface. The shadow gets darker (0.12 alpha) when the CRT barrel distortion is higher, as if the curved glass creates more depth.

### Rain Speed Responds to Scroll
When the user is actively scrolling (scroll velocity > 0), the rain streaks temporarily speed up in the same direction as the scroll — creating a brief parallax rush, as if scrolling through the conversation moves the wind. The effect lasts only while scrolling and settles back over 0.5 seconds when scrolling stops.

---

## Implementation Log (2026-04-05)

### What was implemented from this document

**Lighthouse Sweep** (#5): Implemented as proposed. New `lighthouse_angle` uniform on rain_fog shader, driven by haunting_manager. Sweeps left-to-right over 4s. Weight 2. The warm beam brightens existing colors by ~15% and adds a subtle amber tint. Works well with fog layers.

**Distant Thunder** (Haunting System): Implemented. Low rumble at 30Hz fundamental with filtered noise crackle (2.5s), plus a screen flash 0.8s after onset (100ms bright → 300ms decay). Weight 1. The delay between sound and flash is key — you hear it first, then see it.

**Radio Fragment** (Haunting System): Implemented. AM-modulated mid-frequency carrier (600Hz ± drift) with aggressive 18Hz modulation. 0.7s duration. Very quiet (-30dB). The frequency drift gives it that "catching a signal" quality. Weight 1.

**CRT Phosphor Grid** (Existing: CRT Shader): Implemented. 3px-period RGB phosphor mask at 0.03 opacity, gated behind `barrel_distortion > 0.05`. It's nearly invisible but adds physicality when you crank the distortion.

**Shooting Stars** (Existing: Starfield): Implemented. Three independent shooting star cycles (30s, 55s, 80s) with seeded positions per cycle. Each streak is a 0.5s diagonal movement with a decaying tail. The staggered cycles mean they're rare enough to feel special but frequent enough to notice.

**Cursor Breath** (Ambient Details): Implemented as caret color alpha pulse (3s period, 0.6-1.0 range) when typing speed < 0.5 and input has focus. Subtler than the original proposal — only the caret breathes, not the whole field.

### What was deferred

**Breath Fog on Idle** (#1): Needs a new shader layer with directional Perlin noise. Medium effort, deferred to a dedicated session.

**Message Decay / Ink Bleed** (#2): Requires per-message RichTextLabel refactor. Deferred until that architecture change is made.

**Tidal Pull** (#3): The parallax/color-shift parts are easy but the asymmetric scroll friction fights usability. Deferred pending a UX-safe approach (parallax yes, resistance no).

**Drowned UI** (#6): Many pieces (shader, particles, audio bus, transition). Saved for a feature branch.

### Phase 3 Implementation (2026-04-05)

**Settings Persistence**: All effect toggles, intensity sliders, preset name, and text settings (font, size, LCD filter, enter-sends) saved to `user://settings.cfg` via Godot's ConfigFile API. Auto-saves on every toggle/slider change after initial load. Loaded in main.gd before boot sequence.

**Emotional Resonance** (#8): Implemented in `on_stream_end()`. Long responses (>500 chars) trigger a 2s vignette breathe (base + 0.15 → base), ambient pitch drop to 0.95 for 2s, and a sub-bass tone at master_volume - 12dB. Short responses (<50 chars) get a glass tap + 100ms brightness flicker. The thresholds feel right — 500 chars is roughly a full paragraph.

**The Clock** (#7): Implemented. Polls `Time.get_datetime_dict_from_system()` every 60s. Maps hour to four zones: deep night (22-5: blue tint, 1.3x stars, pitch 0.97), dawn (5-8: warm transition), day (8-17: neutral, 0.5x stars), dusk (17-22: blue fade). Modulates star_density multiplier, rain_warmth, and ambient pitch. Toggle in config panel, persisted in settings.

**Ghost Typing** (#4): Implemented. After 12s of idle with empty input (not during streaming), a random phrase from a pool of 12 atmospheric fragments types out character-by-character (80ms/char) into the input field's placeholder text. Holds for 3s, then fades over 1.5s. Instantly cleared on any typing. The fragments are evocative but never pretend to be completions — "the water remembers", "between the static", etc.

### What changed from the proposals

- **rain_warmth** was a declared-but-unused shader uniform. Added actual tint logic in the shader fragment.
- **Wobble settle** (from UX doc) was implemented — messages settle into stillness after 12s, which is both a performance fix and thematically appropriate.
- **VHS tracking bar** got noise-modulated speed rather than a full drift system — keeps the spirit without overcomplicating the shader.
