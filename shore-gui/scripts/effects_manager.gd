extends Node

# ── Toggles ───────────────────────────────────────────────────────

@export var crt_enabled := true
@export var glow_enabled := true
@export var audio_enabled := true
@export var fire_cursor_enabled := true
@export var screen_shake_enabled := true
@export var vhs_enabled := true
@export var starfield_enabled := true
@export var wpm_enabled := true
@export var typing_sounds_enabled := true
@export var ambient_enabled := true
@export var master_volume_db := -6.0
@export var ambient_volume_db := -24.0

# ── Intensities ───────────────────────────────────────────────────

@export_range(0.0, 1.0) var crt_scanline_intensity := 0.15
@export_range(0.0, 0.5) var crt_distortion_intensity := 0.1
@export_range(0.0, 1.0) var crt_vignette_intensity := 0.3
@export_range(0.0, 10.0) var glow_radius := 3.0
@export_range(0.0, 1.0) var shake_scale := 1.0
@export_range(0.0, 1.0) var star_density := 0.4

# ── Node refs (set in _ready) ────────────────────────────────────

var _crt_rect: ColorRect
var _glow_rect: ColorRect
var _vhs_rect: ColorRect
var _starfield_rect: ColorRect
var _token_audio: AudioStreamPlayer
var _error_audio: AudioStreamPlayer
var _complete_audio: AudioStreamPlayer
var _boot_audio: AudioStreamPlayer
var _type_audio: AudioStreamPlayer
var _ambient_audio: AudioStreamPlayer
var _fire_cursor: GPUParticles2D
var _completion_burst: GPUParticles2D
var _error_crumble: GPUParticles2D
var _token_trail: GPUParticles2D
var _input_field: TextEdit
var _main_node: Control
var _wpm_label: Label
var _scroll: ScrollContainer

# ── State ─────────────────────────────────────────────────────────

var _stream_char_count := 0
var _glow_target := 0.0
var _glow_current := 0.0
var _click_cooldown := 0.0
var _type_cooldown := 0.0
var _prev_text_length := 0
var _typing_speed := 0.0

# Screen shake
var _shake_intensity := 0.0
var _shake_decay := 8.0
var _original_position := Vector2.ZERO

# VHS glitch
var _vhs_intensity := 0.0

# Boot sequence
var _boot_phase := 0
var _boot_timer := 0.0

# Ambient
var _ambient_target_pitch := 1.0
var _is_streaming := false

# ── Audio streams (generated in _ready) ──────────────────────────

var _click_stream: AudioStreamWAV
var _thunk_stream: AudioStreamWAV
var _buzz_stream: AudioStreamWAV
var _boot_stream: AudioStreamWAV
var _whoosh_stream: AudioStreamWAV
var _keypress_stream: AudioStreamWAV
var _ambient_stream: AudioStreamWAV

const MIX_RATE := 44100
const AMBIENT_LOOP_SECONDS := 4.0

func _ready() -> void:
	_crt_rect = get_node("../CRTLayer/CRTRect")
	_glow_rect = get_node("../GlowLayer/GlowRect")
	_vhs_rect = get_node("../VHSLayer/VHSRect")
	_starfield_rect = get_node("../StarfieldLayer/StarfieldRect")
	_token_audio = $TokenAudio
	_error_audio = $ErrorAudio
	_complete_audio = $CompleteAudio
	_boot_audio = $BootAudio
	_type_audio = $TypeAudio
	_ambient_audio = $AmbientAudio
	_input_field = get_node("../Layout/InputArea/InputField")
	_fire_cursor = _input_field.get_node("FireCursor")
	_completion_burst = get_node("../Layout/MessageScroll/CompletionBurst")
	_error_crumble = get_node("../Layout/MessageScroll/ErrorCrumble")
	_token_trail = get_node("../Layout/MessageScroll/TokenTrail")
	_main_node = get_parent()
	_original_position = _main_node.position
	_wpm_label = get_node("../Layout/Header/WPMLabel")
	_scroll = get_node("../Layout/MessageScroll")

	_click_stream = _generate_click()
	_thunk_stream = _generate_thunk()
	_buzz_stream = _generate_buzz()
	_boot_stream = _generate_boot_sound()
	_whoosh_stream = _generate_whoosh()
	_keypress_stream = _generate_keypress()
	_ambient_stream = _generate_ambient_loop()

	_token_audio.stream = _click_stream
	_complete_audio.stream = _thunk_stream
	_error_audio.stream = _buzz_stream
	_boot_audio.stream = _boot_stream
	_type_audio.stream = _keypress_stream

	_token_audio.volume_db = master_volume_db
	_complete_audio.volume_db = master_volume_db
	_error_audio.volume_db = master_volume_db
	_boot_audio.volume_db = master_volume_db + 3.0
	_type_audio.volume_db = master_volume_db - 3.0
	_ambient_audio.stream = _ambient_stream
	_ambient_audio.volume_db = ambient_volume_db

	if _wpm_label:
		_wpm_label.visible = false

	if _completion_burst:
		_completion_burst.emitting = false
	if _error_crumble:
		_error_crumble.emitting = false
	if _token_trail:
		_token_trail.emitting = false

	_apply_toggles()
	_start_boot_sequence()

func _process(delta: float) -> void:
	# ── Boot sequence ─────────────────────────────────────────────
	if _boot_phase == 1:
		_boot_timer += delta
		var warmth := clampf(_boot_timer / 1.5, 0.0, 1.0)
		if _crt_rect:
			_crt_rect.material.set_shader_parameter("brightness", lerpf(0.0, 1.0, warmth))
			_crt_rect.material.set_shader_parameter("scanline_opacity", lerpf(0.6, 0.15, warmth))
			_crt_rect.material.set_shader_parameter("barrel_distortion", lerpf(0.3, 0.1, warmth))
		if _boot_timer >= 1.5:
			_boot_phase = 2
			if _crt_rect:
				_crt_rect.material.set_shader_parameter("brightness", 1.0)
				_crt_rect.material.set_shader_parameter("scanline_opacity", 0.15)
				_crt_rect.material.set_shader_parameter("barrel_distortion", 0.1)

	# ── Glow decay ────────────────────────────────────────────────
	if glow_enabled and _glow_rect:
		_glow_target = lerpf(_glow_target, 0.0, delta * 3.0)
		_glow_current = lerpf(_glow_current, _glow_target, delta * 8.0)
		_glow_rect.material.set_shader_parameter("glow_intensity", _glow_current)

	# ── Cooldowns ─────────────────────────────────────────────────
	_click_cooldown = maxf(_click_cooldown - delta, 0.0)
	_type_cooldown = maxf(_type_cooldown - delta, 0.0)

	# ── Screen shake decay ────────────────────────────────────────
	if _shake_intensity > 0.01:
		_shake_intensity = lerpf(_shake_intensity, 0.0, delta * _shake_decay)
		_main_node.position = _original_position + Vector2(
			randf_range(-_shake_intensity, _shake_intensity),
			randf_range(-_shake_intensity, _shake_intensity)
		)
	elif _main_node.position != _original_position:
		_main_node.position = _original_position

	# ── VHS glitch decay ─────────────────────────────────────────
	if _vhs_intensity > 0.01:
		_vhs_intensity = lerpf(_vhs_intensity, 0.0, delta * 6.0)
		if _vhs_rect:
			_vhs_rect.material.set_shader_parameter("intensity", _vhs_intensity)
	elif _vhs_rect and _vhs_intensity > 0.0:
		_vhs_intensity = 0.0
		_vhs_rect.material.set_shader_parameter("intensity", 0.0)

	# ── Starfield parallax ────────────────────────────────────────
	if starfield_enabled and _starfield_rect and _scroll:
		var scroll_val := float(_scroll.scroll_vertical)
		_starfield_rect.material.set_shader_parameter("scroll_offset", scroll_val)

	# ── Ambient pitch modulation ──────────────────────────────────
	if ambient_enabled and _ambient_audio:
		if not _ambient_audio.playing:
			_ambient_audio.play()
		_ambient_audio.pitch_scale = lerpf(_ambient_audio.pitch_scale, _ambient_target_pitch, delta * 2.0)
	elif _ambient_audio and _ambient_audio.playing:
		_ambient_audio.stop()

	# ── Fire cursor + typing speed + typing sounds ────────────────
	if _fire_cursor and _input_field:
		_update_fire_cursor(delta)

	# ── WPM display ───────────────────────────────────────────────
	if wpm_enabled and _wpm_label:
		_update_wpm_label()

# ── Public API (called from main.gd) ─────────────────────────────

func on_stream_start() -> void:
	_stream_char_count = 0
	_is_streaming = true
	_ambient_target_pitch = 1.05  # slightly higher during generation
	if crt_enabled and _crt_rect:
		_crt_rect.material.set_shader_parameter("aberration", 1.0)
	# Start token trail
	if _token_trail and _scroll:
		_token_trail.emitting = true

func on_stream_chunk(text: String, _content_type: String) -> void:
	_stream_char_count += text.length()

	if crt_enabled and _crt_rect:
		var ab := clampf(1.0 + float(_stream_char_count) * 0.002, 1.0, 8.0)
		_crt_rect.material.set_shader_parameter("aberration", ab)

	if glow_enabled:
		_glow_target = clampf(_glow_target + 0.15, 0.0, 0.8)

	if audio_enabled and _click_cooldown <= 0.0 and not text.is_empty():
		_play_token_click(text)
		_click_cooldown = 0.03

	# Move token trail to bottom-right of scroll area (where new text appears)
	if _token_trail and _scroll:
		var scroll_rect := _scroll.get_rect()
		_token_trail.position = Vector2(scroll_rect.size.x * 0.8, scroll_rect.size.y - 10.0)

func on_stream_end() -> void:
	_is_streaming = false
	_ambient_target_pitch = 1.0  # settle back to normal
	# Stop token trail
	if _token_trail:
		_token_trail.emitting = false
	if crt_enabled and _crt_rect:
		var tween := create_tween()
		tween.tween_method(
			func(val: float): _crt_rect.material.set_shader_parameter("aberration", val),
			_crt_rect.material.get_shader_parameter("aberration"),
			1.0, 0.5
		)

	_glow_target = 0.0

	if audio_enabled:
		_complete_audio.stream = _thunk_stream
		_complete_audio.volume_db = master_volume_db
		_complete_audio.pitch_scale = randf_range(0.9, 1.1)
		_complete_audio.play()

	if screen_shake_enabled:
		_shake_intensity = 2.0 * shake_scale

	# Completion particle burst
	if _completion_burst and _scroll:
		var scroll_rect := _scroll.get_global_rect()
		_completion_burst.position = Vector2(scroll_rect.size.x * 0.5, scroll_rect.size.y - 20.0)
		_completion_burst.emitting = true

func on_error() -> void:
	if audio_enabled:
		_error_audio.pitch_scale = randf_range(0.85, 1.0)
		_error_audio.play()

	if screen_shake_enabled:
		_shake_intensity = 8.0 * shake_scale

	if vhs_enabled:
		_vhs_intensity = 1.0

	# Error crumble particles
	if _error_crumble and _scroll:
		var scroll_rect := _scroll.get_rect()
		_error_crumble.position = Vector2(scroll_rect.size.x * 0.5, scroll_rect.size.y - 30.0)
		_error_crumble.emitting = true

func on_message_sent() -> void:
	if audio_enabled:
		_complete_audio.stream = _whoosh_stream
		_complete_audio.pitch_scale = randf_range(1.1, 1.3)
		_complete_audio.volume_db = master_volume_db - 3.0
		_complete_audio.play()
		await _complete_audio.finished
		_complete_audio.stream = _thunk_stream
		_complete_audio.volume_db = master_volume_db

func on_new_message() -> void:
	if vhs_enabled:
		_vhs_intensity = 0.6

# ── Toggle management ─────────────────────────────────────────────

func _apply_toggles() -> void:
	if _crt_rect:
		_crt_rect.material.set_shader_parameter("effect_enabled", 1.0 if crt_enabled else 0.0)
		_crt_rect.material.set_shader_parameter("scanline_opacity", crt_scanline_intensity)
		_crt_rect.material.set_shader_parameter("barrel_distortion", crt_distortion_intensity)
		_crt_rect.material.set_shader_parameter("vignette_strength", crt_vignette_intensity)
	if _glow_rect:
		_glow_rect.material.set_shader_parameter("effect_enabled", 1.0 if glow_enabled else 0.0)
		_glow_rect.material.set_shader_parameter("glow_radius", glow_radius)
	if _fire_cursor:
		_fire_cursor.emitting = fire_cursor_enabled
	if _starfield_rect:
		_starfield_rect.visible = starfield_enabled
		_starfield_rect.material.set_shader_parameter("star_density", star_density)

func set_all_effects(val: bool) -> void:
	crt_enabled = val
	glow_enabled = val
	audio_enabled = val
	fire_cursor_enabled = val
	screen_shake_enabled = val
	vhs_enabled = val
	starfield_enabled = val
	wpm_enabled = val
	typing_sounds_enabled = val
	ambient_enabled = val
	_apply_toggles()

# ── Boot sequence ─────────────────────────────────────────────────

func _start_boot_sequence() -> void:
	_boot_phase = 1
	_boot_timer = 0.0
	if _crt_rect:
		_crt_rect.material.set_shader_parameter("brightness", 0.0)
		_crt_rect.material.set_shader_parameter("scanline_opacity", 0.6)
		_crt_rect.material.set_shader_parameter("barrel_distortion", 0.3)
	if audio_enabled:
		_boot_audio.play()

# ── WPM display ───────────────────────────────────────────────────

func _update_wpm_label() -> void:
	if _typing_speed < 0.5:
		_wpm_label.visible = false
	else:
		_wpm_label.visible = true
		var wpm := int(_typing_speed / 5.0 * 60.0)
		_wpm_label.text = "%d WPM" % wpm

		# Color ramp: gray -> white -> yellow -> orange -> red
		var speed_t := clampf(float(wpm) / 150.0, 0.0, 1.0)
		var col: Color
		if speed_t < 0.3:
			col = Color(0.6, 0.6, 0.6).lerp(Color(1.0, 1.0, 1.0), speed_t / 0.3)
		elif speed_t < 0.6:
			col = Color(1.0, 1.0, 1.0).lerp(Color(1.0, 0.9, 0.2), (speed_t - 0.3) / 0.3)
		elif speed_t < 0.8:
			col = Color(1.0, 0.9, 0.2).lerp(Color(1.0, 0.5, 0.0), (speed_t - 0.6) / 0.2)
		else:
			col = Color(1.0, 0.5, 0.0).lerp(Color(1.0, 0.1, 0.0), (speed_t - 0.8) / 0.2)
		_wpm_label.add_theme_color_override("font_color", col)

# ── Audio generation ──────────────────────────────────────────────

func _generate_click() -> AudioStreamWAV:
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 220  # ~5ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := 1.0 - t * 200.0
		var sample := sin(t * TAU * 800.0) * maxf(envelope, 0.0)
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_keypress() -> AudioStreamWAV:
	# Shorter, snappier mechanical key sound
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 132  # ~3ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := 1.0 - t * 333.0  # very fast decay
		var sample := sin(t * TAU * 1200.0) * maxf(envelope, 0.0)
		# Add a click transient at the start
		sample += sin(t * TAU * 3500.0) * maxf(1.0 - t * 1000.0, 0.0) * 0.4
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_thunk() -> AudioStreamWAV:
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 2205  # ~50ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := exp(-t * 40.0)
		var sample := sin(t * TAU * 120.0) * envelope
		sample += sin(t * TAU * 60.0) * envelope * 0.5
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_buzz() -> AudioStreamWAV:
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 4410  # ~100ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := 1.0 - t * 10.0
		var sample := sin(t * TAU * 150.0) * 0.4
		sample += sin(t * TAU * 237.0) * 0.3
		sample += sin(t * TAU * 89.0) * 0.3
		sample *= maxf(envelope, 0.0)
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_boot_sound() -> AudioStreamWAV:
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var duration := 1.2
	var samples := int(MIX_RATE * duration)
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var progress := t / duration
		var freq := lerpf(60.0, 800.0, progress * progress)
		var hum := sin(t * TAU * 60.0) * 0.15 * (1.0 - progress)
		var sweep := sin(t * TAU * freq) * 0.3 * progress
		var crackle := sin(t * TAU * 4000.0 * (1.0 + sin(t * 13.7))) * 0.05 * progress
		var env := clampf(t / 0.3, 0.0, 1.0)
		var sample := (hum + sweep + crackle) * env
		if t > duration - 0.02:
			sample += sin((t - (duration - 0.02)) * TAU * 2000.0) * 0.5 * (1.0 - (t - (duration - 0.02)) / 0.02)
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_whoosh() -> AudioStreamWAV:
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 3300  # ~75ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var env := exp(-t * 30.0)
		var sample := sin(t * TAU * (2000.0 - t * 15000.0)) * 0.3
		sample += sin(t * TAU * (1200.0 - t * 8000.0)) * 0.2
		sample *= env
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_ambient_loop() -> AudioStreamWAV:
	# Warm synthwave drone — layered detuned sines with slow LFO modulation
	# Generates a seamless loop
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	stream.loop_mode = AudioStreamWAV.LOOP_FORWARD
	var total_samples := int(MIX_RATE * AMBIENT_LOOP_SECONDS)
	stream.loop_end = total_samples
	var data := PackedByteArray()
	for i in total_samples:
		var t := float(i) / float(MIX_RATE)
		# Base frequencies — low pad chord (C2, E2, G2 roughly)
		var f1 := 65.41   # C2
		var f2 := 82.41   # E2
		var f3 := 98.0    # G2
		# Slight detuning for warmth
		var detune := sin(t * TAU * 0.1) * 0.5
		# Main pad voices
		var pad := sin(t * TAU * (f1 + detune)) * 0.25
		pad += sin(t * TAU * (f2 - detune * 0.7)) * 0.2
		pad += sin(t * TAU * (f3 + detune * 0.3)) * 0.15
		# Sub octave
		pad += sin(t * TAU * (f1 * 0.5)) * 0.15
		# Slow amplitude LFO — breathing effect
		var lfo := sin(t * TAU / AMBIENT_LOOP_SECONDS) * 0.15 + 0.85
		pad *= lfo
		# Gentle high harmonic shimmer
		var shimmer := sin(t * TAU * f1 * 4.0) * 0.03 * sin(t * TAU * 0.25)
		pad += shimmer
		var s16 := int(clampf(pad, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _play_token_click(text: String) -> void:
	var ch := text[-1]
	var pitch := 1.0
	if ch in "aeiouAEIOU":
		pitch = randf_range(1.2, 1.4)
	elif ch in " \t\n":
		pitch = randf_range(0.6, 0.8)
		_token_audio.volume_db = master_volume_db - 6.0
	elif ch in ".,;:!?()[]{}\"'":
		pitch = randf_range(0.7, 0.9)
	else:
		pitch = randf_range(0.95, 1.15)

	_token_audio.pitch_scale = pitch
	if ch not in " \t\n":
		_token_audio.volume_db = master_volume_db
	_token_audio.play()

# ── Fire cursor ───────────────────────────────────────────────────

func _update_fire_cursor(delta: float) -> void:
	var current_len := _input_field.text.length()
	var chars_this_frame := absf(float(current_len - _prev_text_length))
	_prev_text_length = current_len

	var instant_cps := chars_this_frame / maxf(delta, 0.001)
	_typing_speed = lerpf(_typing_speed, instant_cps, delta * 5.0)

	# Typing keypress sound
	if typing_sounds_enabled and audio_enabled and chars_this_frame > 0.0 and _type_cooldown <= 0.0:
		_type_audio.pitch_scale = randf_range(0.9, 1.1)
		_type_audio.play()
		_type_cooldown = 0.02

	# Update particle color based on typing speed
	if fire_cursor_enabled:
		var speed_t := clampf(_typing_speed / 15.0, 0.0, 1.0)
		var base_color := Color(1.0, 0.6, 0.1).lerp(Color(0.5, 0.7, 1.0), speed_t)
		if _fire_cursor.process_material:
			_fire_cursor.process_material.color = base_color

	# Position at caret
	var line := _input_field.get_caret_line()
	var col := _input_field.get_caret_column()
	var rect := _input_field.get_rect_at_line_column(line, col)
	_fire_cursor.position = Vector2(rect.position.x + rect.size.x, rect.position.y)
