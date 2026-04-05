# Shore GUI: Grounded UX & Code Quality Improvements

Complementary to CREATIVE_DRAFT.md. Everything here is concrete, actionable, and prioritized by impact.

---

## Critical Fixes

### 1. Unclosed BBCode tags on abnormal stream end

**File:** `main.gd:203-204, 219-220`

`_on_stream_start()` opens `[fadein][phosphor][wobble]` but these are only closed in `_on_stream_end()`. If the stream terminates abnormally (disconnect, error, cancel), the tags stay open. All subsequent text appended to the RichTextLabel inherits the wobble/phosphor effects permanently.

**Fix:** Track whether effect tags are open with a `_stream_fx_open := false` flag. In `_on_disconnected()`, `_on_error()`, and any cancel handler, close the tags if the flag is true. Better yet, close-then-reopen on every chunk boundary to make the state self-healing.

### 2. Volume changes from config panel don't propagate to audio players

**Files:** `effects_manager.gd:267-272`, `config_panel.gd:72-75`

When the user drags the Master Volume slider, `_on_slider()` sets `_effects.master_volume_db` and calls `_apply_toggles()`. But `_apply_toggles()` only updates shader parameters -- it never touches audio player volumes. The `_token_audio.volume_db`, `_error_audio.volume_db`, `_complete_audio.volume_db`, `_boot_audio.volume_db`, and `_type_audio.volume_db` are set once in `_ready()` (line 267-271) and in `_play_token_click()` (lines 1030, 1038), but never updated when the slider moves.

**Fix:** Add volume propagation to `_apply_toggles()`:
```gdscript
if _token_audio:
    _token_audio.volume_db = master_volume_db
if _complete_audio:
    _complete_audio.volume_db = master_volume_db
if _error_audio:
    _error_audio.volume_db = master_volume_db
if _boot_audio:
    _boot_audio.volume_db = master_volume_db + 3.0
if _type_audio:
    _type_audio.volume_db = master_volume_db - 3.0
if _ambient_audio:
    _ambient_audio.volume_db = ambient_volume_db
if _ocean_audio:
    _ocean_audio.volume_db = ambient_volume_db - 6.0
```

### 3. No way to cancel generation

**File:** `bridge.rs:186-189`, `main.gd:116-127`

`ShoreBridge` exposes `cancel_generation()` but there's no keyboard shortcut or UI button to call it. Users stuck watching a long generation have no escape.

**Fix:** Add `Escape` keybind in `_input()` that calls `bridge.cancel_generation()` when `_streaming` is true. Also add a visual indicator (pulsing "Cancel" label or change the Send button to a Stop button during streaming).

### 4. Backspace inflates WPM counter

**File:** `effects_manager.gd:1045`

```gdscript
var chars_this_frame := absf(float(current_len - _prev_text_length))
```

`absf()` means deleting characters counts as "typing." A user furiously backspacing will show 150 WPM. The fire cursor also heats up from deletion.

**Fix:** Use `maxf(float(current_len - _prev_text_length), 0.0)` to only count additions. Or track insertions and deletions separately if you want the fire cursor to react to both but WPM to count only forward typing.

### 5. rain_warmth is a binary toggle, not a tween

**File:** `main.gd:146-148`

```gdscript
var warmth := 1.0 if _character_name.to_lower() in text else 0.0
effects._rain_fog_material.set_shader_parameter("rain_warmth", warmth)
```

This snaps between 0.0 and 1.0 every frame. If the rain_fog shader uses this value for color modulation, the transition will be an abrupt color shift when the character name is typed or deleted.

**Fix:** Lerp toward target instead of setting directly:
```gdscript
var warmth_target := 1.0 if _character_name.to_lower() in text else 0.0
var current_warmth: float = effects._rain_fog_material.get_shader_parameter("rain_warmth")
effects._rain_fog_material.set_shader_parameter("rain_warmth", lerpf(current_warmth, warmth_target, delta * 2.0))
```

Note: `_process` in `main.gd` doesn't receive `delta` — it uses `_process(delta)` signature but the warm rain block has no access to `delta`. Refactor to use `delta` from the process call.

Wait -- `main.gd:129` has `func _process(delta: float)`, so `delta` IS available. The fix is just changing the set to a lerp using `delta`.

### 6. Preset color changes don't apply to already-rendered messages

**File:** `main.gd:368-371`

Colors are baked into BBCode at render time: `[color=%s]`. When the user switches presets, `_apply_color_palette()` updates the `_colors` dict, but all previously rendered text retains its old colors. The only way to fix this is to re-render.

**Fix:** On preset change, save scroll position, clear the message display, re-request history via `bridge.send_command("log", "{}")`, and restore scroll position. This is a heavier operation but the only correct one.

---

## Polish Pass

### Config panel transition

**File:** `main.gd:377-385`, `config_panel.gd:89-91`

The config panel appears and disappears instantly via `add_child()` / `queue_free()`. This is the most obvious jank in the UI.

**Fix:** On open, set the panel's initial `offset_left` to 0 (offscreen right), then tween it to `-360` over 200ms with `EASE_OUT`. On close, tween `offset_left` from `-360` to `0` over 150ms with `EASE_IN`, then `queue_free()` on tween completion. The config_panel.tscn already positions with `offset_left = -360`, so this is straightforward.

### Boot sequence snap

**File:** `effects_manager.gd:293-305`

The boot sequence hard-sets final values at `_boot_timer >= 1.5`:
```gdscript
_crt_rect.material.set_shader_parameter("brightness", 1.0)
_crt_rect.material.set_shader_parameter("scanline_opacity", 0.15)
_crt_rect.material.set_shader_parameter("barrel_distortion", 0.1)
```

The lerp that precedes this is smooth, but the snap to phase 2 means there's a subtle discontinuity if the lerp hasn't quite reached 1.0 due to floating point.

**Fix:** Replace `_boot_phase = 2` snap with a `_boot_phase = 0` (done) and let the final values hold from the lerp. The values at `warmth = 1.0` already equal the targets.

### Glow blur quality at high radius

**File:** `shaders/glow.gdshader:14-24`

The glow shader uses a single-pass 9-tap weighted blur. At `glow_radius > 5.0`, this becomes visibly blocky because the 9 samples are too spread out to cover the kernel smoothly.

**Fix:** Either clamp `glow_radius` max to 5.0 in the config panel (pragmatic), or replace with a two-pass Gaussian (horizontal blur pass + vertical blur pass using BackBufferCopy or two CanvasLayers). For the scope of this app, clamping is sufficient.

### VHS tracking bar variety

**File:** `shaders/vhs.gdshader:27-30`

```glsl
float bar_pos = fract(TIME * 0.3);
```

The tracking bar scrolls at a fixed, predictable speed. Real VHS tracking bars drift, stutter, and sometimes reverse.

**Fix:** Modulate speed with noise: `float bar_pos = fract(TIME * 0.3 + sin(TIME * 0.7) * 0.1)`. Also add a chance for the bar to "stick" (slow down dramatically) for 0.2s before resuming.

### Auto-focus input field on connect

**File:** `main.gd:161-173`

After `_on_connected()`, the input field doesn't grab focus. The user has to click it.

**Fix:** Add `input_field.grab_focus()` at the end of `_on_connected()`.

### Escape closes config panel

**File:** `main.gd:116-127`

F2 toggles the config panel, but Escape doesn't close it.

**Fix:** In `_input()`, add:
```gdscript
if event.keycode == KEY_ESCAPE:
    if _config_panel and is_instance_valid(_config_panel):
        _toggle_config_panel()
        get_viewport().set_input_as_handled()
    elif _streaming:
        bridge.cancel_generation()
        get_viewport().set_input_as_handled()
```

This gives Escape double duty: close panel first, cancel generation second.

### Condensation wipe is single-point only

**File:** `glass_manager.gd:57-62`, `shaders/condensation.gdshader:42-49`

The condensation shader supports one `wipe_pos`. Each click replaces the previous wipe, so rapid tapping loses the first wipe.

**Fix (minimal):** Track an array of wipe points (up to 4) with individual ages, similar to how `glass_cracks.gdshader` handles 8 crack points. Pass as `wipe_0` through `wipe_3` with `wipe_age_0` through `wipe_age_3`.

### Scroll jitter during fast streaming

**File:** `main.gd:373-375`

```gdscript
func _scroll_to_bottom() -> void:
    await get_tree().process_frame
    scroll.scroll_vertical = scroll.get_v_scroll_bar().max_value as int
```

During rapid streaming, `_on_stream_chunk()` calls `_scroll_to_bottom()` on every chunk. Each call awaits one frame, creating a queue of pending scroll-to-bottom operations. This can cause visible jitter.

**Fix:** Use a debounced approach — set a `_needs_scroll := true` flag in `_on_stream_chunk()`, then in `_process()` check the flag and scroll once per frame:
```gdscript
var _needs_scroll := false

func _process(delta: float) -> void:
    # ... existing code ...
    if _needs_scroll:
        scroll.scroll_vertical = scroll.get_v_scroll_bar().max_value as int
        _needs_scroll = false

func _scroll_to_bottom() -> void:
    _needs_scroll = true
```

---

## Code Quality

### effects_manager.gd is 1069 lines and does everything

This file handles: preset management, shader parameter management, 14 audio generation functions (~400 lines), particle management, fire cursor tracking, typing speed detection, WPM display, boot sequence, ambient audio management, screen shake, VHS glitch, glow decay, background parallax, thought-light intensity, and rain intensity fluctuation.

**Fix:** Extract audio generation into a dedicated `audio_factory.gd`:
- Move all `_generate_*()` functions to a static utility class
- `effects_manager.gd` calls `AudioFactory.generate_click()` etc.
- Reduces effects_manager to ~650 lines (still large but manageable)

Consider further splitting:
- `audio_palette.gd` — stream swapping, bus routing, volume management
- `shader_manager.gd` — all `_apply_toggles()` shader parameter work
- Leave `effects_manager.gd` as the coordinator that delegates to these

### Repeated audio generation boilerplate

**Files:** `effects_manager.gd:637-1007`

Every `_generate_*()` function repeats:
```gdscript
var stream := AudioStreamWAV.new()
stream.format = AudioStreamWAV.FORMAT_16_BITS
stream.mix_rate = MIX_RATE
var data := PackedByteArray()
var samples := ...
for i in samples:
    var t := float(i) / float(MIX_RATE)
    # ... sample calculation ...
    var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
    data.append(s16 & 0xFF)
    data.append((s16 >> 8) & 0xFF)
stream.data = data
return stream
```

**Fix:** Extract a helper:
```gdscript
static func _make_wav(duration_seconds: float, callback: Callable, loop := false) -> AudioStreamWAV:
    var stream := AudioStreamWAV.new()
    stream.format = AudioStreamWAV.FORMAT_16_BITS
    stream.mix_rate = MIX_RATE
    var total_samples := int(MIX_RATE * duration_seconds)
    if loop:
        stream.loop_mode = AudioStreamWAV.LOOP_FORWARD
        stream.loop_end = total_samples
    var data := PackedByteArray()
    data.resize(total_samples * 2)
    for i in total_samples:
        var t := float(i) / float(MIX_RATE)
        var sample: float = callback.call(t, duration_seconds, i)
        var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
        data[i * 2] = s16 & 0xFF
        data[i * 2 + 1] = (s16 >> 8) & 0xFF
    stream.data = data
    return stream
```

This cuts ~250 lines of boilerplate.

### Config panel uses find_child for all control access

**File:** `config_panel.gd:52-57`

```gdscript
var toggle := find_child(effect_name + "Toggle", true, false) as CheckButton
```

All UI access goes through `find_child()` with string concatenation. If a node is renamed in the .tscn, it silently breaks with no error.

**Fix:** Use `@onready` node references or at minimum validate in `_ready()` that all expected children exist and print warnings for missing ones.

### Dictionary-heavy data structures without type safety

**Files:** `seagull_manager.gd:103-112`, `glass_manager.gd:69-80`, `haunting_manager.gd` throughout

Seagulls, cracks, and stress points are all `Array[Dictionary]` with string keys. Every access requires remembering the key names and casting:
```gdscript
sp["taps"] = (sp["taps"] as int) + 1
```

**Fix:** Define inner classes or typed structs:
```gdscript
class StressPoint:
    var pos: Vector2
    var taps: int
    var decay_timer: float
```

This catches typos at parse time instead of runtime and eliminates all the `as int` / `as float` / `as Vector2` casts.

### No settings persistence

**Observation:** All config changes (font, font size, preset, every slider and toggle) are lost on restart. There's no save/load mechanism.

**Fix:** On any config change, serialize current state to `user://settings.cfg` using ConfigFile. On `_ready()`, load and apply saved settings. This is ~30 lines of code and high-impact quality-of-life.

### Missing type hints

Several variables across the codebase lack type hints:

- `main.gd:49`: `var _config_scene := preload(...)` — type is inferred but not explicit
- `effects_manager.gd:130`: `var _stream_char_count := 0` — should be `: int`
- `haunting_manager.gd:12-15`: multiple untyped vars

Not a bug, but makes the code harder to read and refactor. GDScript's type system is opt-in but all new code and touched code should use explicit types.

---

## Text & Chat UX

### Wobble effect runs forever on all history messages

**File:** `main.gd:289-291, 347-349`, `wobble_effect.gd:8-15`

History messages rendered via `_render_assistant_message()` get wrapped in `[wobble][phosphor]...[/phosphor][/wobble]`. The wobble effect uses `char_fx.elapsed_time` which increases every frame, meaning every character in every history message is being repositioned by sin() every frame, forever.

For a conversation with 500 visible characters, that's 500 sin() + 500 cos() evaluations per frame just for wobble, plus 500 more for phosphor. This scales linearly with conversation length and will eventually cause frame drops.

**Fix (two options):**
1. Only apply `[wobble][phosphor]` to the most recent N messages (e.g., last 3). Older messages get plain rendering.
2. Add a "settle" mechanic: after `elapsed_time > 10.0`, the wobble amplitude decays to zero. This is atmospherically appropriate (messages "settle" into place like paper on water).

Option 2 is better — it's cheap (one extra multiply) and thematically coherent:
```gdscript
func _process_custom_fx(char_fx: CharFXTransform) -> bool:
    var idx := float(char_fx.relative_index)
    var t := char_fx.elapsed_time
    var settle := clampf(1.0 - (t - 8.0) / 4.0, 0.0, 1.0)  # fade out 8-12s
    var x_offset := sin(t * 0.8 + idx * 0.15) * 1.2 * settle
    var y_offset := sin(t * 0.5 + idx * 0.1 + 1.7) * 0.8 * settle
    char_fx.offset += Vector2(x_offset, y_offset)
    return true
```

### No message display buffer limit

**File:** `main.gd` — messages are appended to `message_display` indefinitely

RichTextLabel stores all appended BBCode in memory. Over a long conversation (hundreds of messages), this grows without bound. Combined with per-character RichTextEffects running every frame, this will eventually degrade performance.

**Fix:** Track message count. When it exceeds a threshold (e.g., 200 messages), trim the oldest messages from the display. RichTextLabel doesn't support partial clearing, so this would require either:
- Clearing and re-rendering the most recent N messages, or
- Using a different approach: one RichTextLabel per message in a VBoxContainer, and `queue_free()` old ones

The per-message approach also makes it possible to add per-message features (age tracking for ink bleed, individual fade-in timing, etc.).

### No streaming indicator

When the assistant is generating, there's no visual signal between chunks. If the model pauses for 2-3 seconds (thinking, tool use), the user can't tell if it's still working or if something broke.

**Fix:** Add a pulsing `...` or cursor blink at the end of the streaming text. In `_on_stream_start()`, start a tween that pulses a trailing indicator. In `_on_stream_chunk()`, remove and re-append it after the new text. In `_on_stream_end()`, remove it.

Simpler alternative: pulse the status label or add a subtle animation to it during streaming.

### Input field doesn't grow with content

**File:** `scenes/main.tscn:174`

```
custom_minimum_size = Vector2(0, 60)
```

The TextEdit has a fixed 60px height. Typing a long multi-line message doesn't expand it.

**Fix:** In `_update_fire_cursor()` (which already runs every frame when the input field exists), calculate the content height and update `custom_minimum_size.y` to `clampf(content_height, 60.0, 200.0)`. TextEdit provides `get_line_count()` and line height can be derived from the font size.

### Enter key behavior is non-configurable

**File:** `main.gd:120-123`

Only `Ctrl+Enter` sends. This is fine for power users but unexpected for most chat app users who expect Enter to send and Shift+Enter for newlines.

**Fix:** Add a toggle in the config panel: "Enter sends message" (default: off). When on, intercept bare Enter in `_input()` and send, while letting Shift+Enter pass through to TextEdit for newlines.

---

## Config Panel UX

### No value display next to sliders

**File:** `scenes/config_panel.tscn` — all slider nodes

Sliders show a visual position but no numeric value. The user can't tell if scanline intensity is 0.15 or 0.20.

**Fix:** Add a small Label to the right of each slider that displays the current value, updated by the `value_changed` signal. Format as `"%.2f"` for float sliders, `"%d dB"` for volume sliders.

### Preset doesn't switch to "Custom" on manual tweak

**File:** `config_panel.gd:67-75`

IDEAS.md specifies: "Tweaking any slider after selecting a preset switches to Custom." This isn't implemented. The dropdown stays on "Silent Shore" even after the user changes every slider.

**Fix:** In `_on_toggle()` and `_on_slider()`, check if a preset is selected and if the new value differs from the preset's value. If so, append "Custom" to the OptionButton if not present, and select it. Store `_effects._active_preset = "Custom"`.

### Slider labels use space-indented text

**File:** `scenes/config_panel.tscn:83, 93, 105, etc.`

```
text = "  Scanline Intensity"
```

Leading spaces for visual indentation. This breaks if the font changes or if the panel is resized.

**Fix:** Use a `MarginContainer` with `margin_left = 16` wrapping each sub-slider, or set `theme_override_constants/margin_left` on the label node.

### No section collapsibility

The config panel is a long vertical list of 20+ controls. At smaller window sizes, finding a specific control requires scrolling.

**Fix:** Group controls under collapsible headers. Each section header ("Shaders", "Effects", "Audio", "Text") becomes a Button that toggles visibility of its child controls. This is ~20 lines of code per section.

### Volume sliders have confusing dB ranges

**File:** `scenes/config_panel.tscn:218-220, 228-230`

Master: -30 to 0 dB. Ambient: -40 to -10 dB. Most users don't think in decibels.

**Fix:** Display as a percentage (0-100%) in the value label, even though the underlying value is dB. Map linearly: `percent = (value - min) / (max - min) * 100`.

---

## Integration with Creative Draft

### Practical to implement well (do these)

1. **Lighthouse Sweep** (Creative Draft #5) — Simplest new feature. One new shader uniform, one timer in haunting_manager. Could ship in an hour. High atmosphere-to-effort ratio.

2. **Emotional Resonance** (Creative Draft #8) — `on_stream_end()` already has `_stream_char_count`. Adding conditional VFX based on response length is ~15 lines. The sub-bass tone needs one new audio generator.

3. **The Clock** (Creative Draft #7) — `Time.get_datetime_dict_from_system()` + a 60-second timer + feeding values into existing shader uniforms. Self-contained, no architectural changes. High delight-to-effort ratio.

4. **Ghost Typing** (Creative Draft #4) — Timer + overlay Label in the input area. Small, self-contained. The fragment pool approach is elegant and avoids any AI integration complexity.

5. **CRT phosphor grid texture** (Existing: CRT Shader) — Add a 3-line repeating micro-pattern in fragment(). Trivial shader change, subtle and rewarding.

6. **Starfield shooting stars** (Existing: Starfield) — Moderate shader work but self-contained. One extra function in starfield.gdshader.

7. **New hauntings: Distant Thunder, Radio Fragment, Tide Shift** (Existing: Haunting System) — The haunting system is well-structured for additions. Each new type is ~20 lines in haunting_manager.gd + an audio generator. High value, low risk.

8. **Cursor Breath** (Ambient Details) — Pulse cursor alpha with a slow sine in `_update_fire_cursor()`. Two lines of code.

### Need significant scaffolding first

1. **Message Decay / Ink Bleed** (Creative Draft #2) — Needs per-message age tracking. Currently messages are just appended BBCode text with no individual identity. Implementing the "one RichTextLabel per message" refactor (mentioned in Text & Chat UX section) is a prerequisite. Do the refactor first, then this becomes easy.

2. **Tidal Pull scroll** (Creative Draft #3) — Needs custom scroll physics overriding ScrollContainer behavior. Godot's ScrollContainer doesn't expose its internal scroll physics well. Would likely need to replace ScrollContainer with a custom implementation. The parallax-on-scroll-velocity part is easy; the asymmetric friction is the hard part.

3. **Breath Fog on Idle** (Creative Draft #1) — Needs a new shader layer and CanvasLayer. The glass_manager idle tracking exists, but the "two plumes from bottom corners" animation is a non-trivial shader with directional Perlin noise and source-point biasing. Medium effort.

4. **Drowned UI / Underwater Mode** (Creative Draft #6) — New background shader (caustics), new particle type (bubbles), audio bus reconfiguration, transition animation. Each piece is straightforward but there are many pieces. Good candidate for a dedicated feature branch.

5. **SDF Rain-on-Glass replacement** (Existing: Rain Fog) — This replaces the core background shader. High impact but high risk — the current rain_fog shader has many uniforms and integration points. Test thoroughly.

### Conflict with usability

1. **Tidal Pull: Scroll resistance when scrolling up** — Making the user fight to scroll through their conversation history is a usability anti-pattern. The atmospheric effects (parallax, depth shift) are fine. The resistance is not. **Recommendation:** Implement the parallax and color-shift-on-scroll effects. Drop the asymmetric friction.

2. **Message Decay / Ink Bleed: Characters shift position** — Even at 1-2px, positional jitter on text reduces readability. This should be gated behind a very aggressive age threshold (only messages well above the visible fold) and should never activate on text the user is currently reading. The color drift and alpha variation parts are fine.

3. **Ghost Typing** — If this appears to come from the AI (suggesting text), users may mistake it for an actual suggestion feature and be confused when it doesn't work. The fragment pool must be obviously non-semantic ("...", single words). Never show fragments that could complete a sentence meaningfully.

### Suggested implementation order

**Phase 1: Foundation fixes** (unblocks everything else)
1. Fix unclosed BBCode tags on abnormal stream end
2. Fix volume slider propagation
3. Add cancel generation keybind (Escape)
4. Add config panel slide transition
5. Fix scroll jitter (debounced approach)
6. Add settings persistence

**Phase 2: Chat quality** (the load-bearing feature)
1. Wobble settle mechanic (performance fix)
2. Streaming indicator
3. Input field auto-grow
4. Auto-focus on connect
5. Enter-sends-message toggle

**Phase 3: Quick atmospheric wins** (high impact, low effort)
1. Lighthouse Sweep haunting
2. Cursor Breath
3. Emotional Resonance (response weight VFX)
4. New hauntings (thunder, radio, tide)
5. CRT phosphor grid micro-pattern
6. Shooting stars

**Phase 4: Config panel polish**
1. Slider value labels
2. Custom preset on manual tweak
3. Section collapsibility
4. Volume percentage display

**Phase 5: Major features** (one at a time, feature-branched)
1. Time-of-day ambient shift (self-contained, low risk)
2. Ghost Typing (self-contained, low risk)
3. Extract audio generation to audio_factory.gd (reduces effects_manager complexity, unblocks further work)
4. Per-message RichTextLabel refactor (unblocks ink bleed, age tracking)
5. SDF rain-on-glass shader replacement
6. Drowned UI preset
