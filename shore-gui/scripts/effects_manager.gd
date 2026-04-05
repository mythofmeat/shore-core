extends Node

signal preset_changed(preset_name: String, color_palette: String, audio_palette: String)

# ── Presets ───────────────────────────────────────────────────────

const PRESETS := {
	"Default": {
		"background_shader": "starfield",
		"crt_enabled": true,
		"glow_enabled": true,
		"audio_enabled": true,
		"fire_cursor_enabled": true,
		"screen_shake_enabled": true,
		"vhs_enabled": true,
		"starfield_enabled": true,
		"wpm_enabled": true,
		"typing_sounds_enabled": true,
		"ambient_enabled": true,
		"hauntings_enabled": false,
		"master_volume_db": -6.0,
		"ambient_volume_db": -24.0,
		"crt_scanline_intensity": 0.15,
		"crt_distortion_intensity": 0.1,
		"crt_vignette_intensity": 0.3,
		"glow_radius": 3.0,
		"shake_scale": 1.0,
		"star_density": 0.4,
		"_color_palette": "default",
		"_audio_palette": "default",
	},
	"Silent Shore": {
		"background_shader": "rain_fog",
		"crt_enabled": true,
		"glow_enabled": true,
		"audio_enabled": true,
		"fire_cursor_enabled": false,
		"screen_shake_enabled": false,
		"vhs_enabled": true,
		"starfield_enabled": true,
		"wpm_enabled": false,
		"typing_sounds_enabled": true,
		"ambient_enabled": true,
		"hauntings_enabled": true,
		"master_volume_db": -6.0,
		"ambient_volume_db": -20.0,
		"crt_scanline_intensity": 0.08,
		"crt_distortion_intensity": 0.04,
		"crt_vignette_intensity": 0.55,
		"glow_radius": 2.0,
		"shake_scale": 0.3,
		"star_density": 0.6,
		"_color_palette": "silent_shore",
		"_audio_palette": "silent_shore",
	},
}

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
@export var hauntings_enabled := false
@export var time_sync_enabled := false
@export var master_volume_db := -6.0
@export var ambient_volume_db := -24.0

# ── Intensities ───────────────────────────────────────────────────

@export_range(0.0, 1.0) var crt_scanline_intensity := 0.15
@export_range(0.0, 0.5) var crt_distortion_intensity := 0.1
@export_range(0.0, 1.0) var crt_vignette_intensity := 0.3
@export_range(0.0, 10.0) var glow_radius := 3.0
@export_range(0.0, 1.0) var shake_scale := 1.0
@export_range(0.0, 1.0) var star_density := 0.4

# ── Preset state ─────────────────────────────────────────────────

var background_shader := "starfield"
var _active_preset := "Default"
var _color_palette := "default"
var _audio_palette := "default"
var _starfield_material: ShaderMaterial
var _rain_fog_material: ShaderMaterial
var _vhs_max_intensity := 1.0  # capped lower for Silent Shore

# For haunting system (set externally by haunting_manager)
var phosphor_linger_active := false

# Rain intensity fluctuation
var _rain_intensity := 1.0
var _rain_intensity_target := 1.0
var _rain_shift_timer := 0.0
var _rain_shift_interval := 20.0  # seconds until next target change

# Thought-lights (typing glow)
var _thought_intensity := 0.0
var _thought_light_rect: ColorRect
var _thought_light_material: ShaderMaterial

# Time-of-day sync
var _time_poll_timer := 0.0
var _time_tint := Color(0.0, 0.0, 0.0, 0.0)  # additive tint applied to bg
var _time_star_mult := 1.0  # star density multiplier
var _time_warmth := 0.0  # extra rain_warmth from time

# Typing combo escalation
var _combo_intensity := 0.0  # 0.0 = idle, 1.0 = 150+ WPM

# Context-sensitive reverb
var _reverb_wet_target := 0.0
var _reverb_wet_current := 0.0
var _idle_timer := 0.0  # seconds since last activity

# Ghost typing
var _ghost_idle_timer := 0.0
var _ghost_active := false
var _ghost_text := ""
var _ghost_char_idx := 0
var _ghost_char_timer := 0.0
var _ghost_fade_timer := 0.0

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
var _settings_loaded := false

# ── Audio streams (generated in _ready) ──────────────────────────

var _click_stream: AudioStreamWAV
var _thunk_stream: AudioStreamWAV
var _buzz_stream: AudioStreamWAV
var _boot_stream: AudioStreamWAV
var _whoosh_stream: AudioStreamWAV
var _keypress_stream: AudioStreamWAV
var _ambient_stream: AudioStreamWAV

# Silent Shore audio variants
var _water_drop_stream: AudioStreamWAV
var _distant_thunk_stream: AudioStreamWAV
var _low_rumble_stream: AudioStreamWAV
var _crt_boot_stream: AudioStreamWAV
var _soft_whoosh_stream: AudioStreamWAV
var _muffled_keypress_stream: AudioStreamWAV
var _rain_ambient_stream: AudioStream
var _ocean_stream: AudioStreamWAV
var _ocean_audio: AudioStreamPlayer
var _glass_tap_stream: AudioStreamWAV
var _glass_crack_stream: AudioStreamWAV
var _glass_audio: AudioStreamPlayer
var _sub_bass_stream: AudioStreamWAV
var _resonance_audio: AudioStreamPlayer
var _effects_bus_idx := -1

const MIX_RATE := 44100
const AMBIENT_LOOP_SECONDS := 4.0
const GHOST_FRAGMENTS := [
	"what if the signal was always there",
	"the water remembers",
	"somewhere a light is on",
	"tell me what you see",
	"the fog is listening",
	"do you hear the rain",
	"it was never quiet here",
	"something stirs beneath",
	"the shore knows your name",
	"between the static",
	"a thought half-formed",
	"the tide is changing",
]
const GHOST_IDLE_DELAY := 12.0  # seconds of idle before ghost appears
const GHOST_CHAR_SPEED := 0.08  # seconds per character

func _ready() -> void:
	_crt_rect = get_node("../CRTLayer/CRTRect")
	_glow_rect = get_node("../GlowLayer/GlowRect")
	_vhs_rect = get_node("../VHSLayer/VHSRect")
	_starfield_rect = get_node("../StarfieldLayer/StarfieldRect")

	# Store starfield material and create rain_fog material
	_starfield_material = _starfield_rect.material as ShaderMaterial
	var rain_fog_shader := load("res://shaders/rain_fog.gdshader") as Shader
	_rain_fog_material = ShaderMaterial.new()
	_rain_fog_material.shader = rain_fog_shader

	# Thought-lights
	_thought_light_rect = get_node("../ThoughtLightsLayer/ThoughtLightsRect")
	if _thought_light_rect:
		_thought_light_material = _thought_light_rect.material as ShaderMaterial

	_token_audio = $TokenAudio
	_error_audio = $ErrorAudio
	_complete_audio = $CompleteAudio
	_boot_audio = $BootAudio
	_type_audio = $TypeAudio
	_ambient_audio = $AmbientAudio
	_ocean_audio = $OceanAudio
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

	# Silent Shore audio variants (eagerly generated)
	_water_drop_stream = _generate_water_drop()
	_distant_thunk_stream = _generate_distant_thunk()
	_low_rumble_stream = _generate_low_rumble()
	_crt_boot_stream = _generate_crt_boot()
	_soft_whoosh_stream = _generate_soft_whoosh()
	_muffled_keypress_stream = _generate_muffled_keypress()
	_rain_ambient_stream = load("res://audio/rain_ambient.ogg")
	if _rain_ambient_stream is AudioStreamOggVorbis:
		(_rain_ambient_stream as AudioStreamOggVorbis).loop = true
	_ocean_stream = _generate_ocean_waves()
	_glass_tap_stream = _generate_glass_tap()
	_glass_crack_stream = _generate_glass_crack()
	_sub_bass_stream = _generate_sub_bass()

	# Glass tap/crack audio player (one-shot, reused)
	_glass_audio = AudioStreamPlayer.new()
	_glass_audio.bus = "Master"
	_glass_audio.volume_db = -10.0
	add_child(_glass_audio)

	# Resonance audio player (for emotional resonance effect)
	_resonance_audio = AudioStreamPlayer.new()
	_resonance_audio.bus = "Master"
	_resonance_audio.volume_db = master_volume_db - 6.0
	add_child(_resonance_audio)

	# Audio bus for Silent Shore reverb/muffling
	_effects_bus_idx = AudioServer.bus_count
	AudioServer.add_bus(_effects_bus_idx)
	AudioServer.set_bus_name(_effects_bus_idx, "Effects")
	AudioServer.set_bus_send(_effects_bus_idx, "Master")
	var lpf := AudioEffectLowPassFilter.new()
	lpf.cutoff_hz = 2000.0
	lpf.resonance = 0.5
	AudioServer.add_bus_effect(_effects_bus_idx, lpf)
	AudioServer.set_bus_effect_enabled(_effects_bus_idx, 0, false)
	var reverb := AudioEffectReverb.new()
	reverb.room_size = 0.6
	reverb.damping = 0.7
	reverb.wet = 0.3
	AudioServer.add_bus_effect(_effects_bus_idx, reverb)
	AudioServer.set_bus_effect_enabled(_effects_bus_idx, 1, false)

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
	_ocean_audio.stream = _ocean_stream
	_ocean_audio.volume_db = -30.0  # very faint undertone
	_ocean_audio.bus = "Master"

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
			_boot_phase = 0  # done — lerp already reached targets at warmth=1.0

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

	# ── Background parallax ───────────────────────────────────────
	if starfield_enabled and _starfield_rect and _scroll:
		var scroll_val := float(_scroll.scroll_vertical)
		_starfield_rect.material.set_shader_parameter("scroll_offset", scroll_val)

	# ── Rain intensity fluctuation ────────────────────────────────
	if background_shader == "rain_fog" and _rain_fog_material:
		_rain_shift_timer -= delta
		if _rain_shift_timer <= 0.0:
			# Pick new target: 0.3 (drizzle) to 1.8 (downpour)
			_rain_intensity_target = randf_range(0.3, 1.8)
			_rain_shift_interval = randf_range(10.0, 30.0)
			_rain_shift_timer = _rain_shift_interval
		# Slow lerp toward target — transitions take several seconds
		_rain_intensity = lerpf(_rain_intensity, _rain_intensity_target, delta * 0.15)
		_rain_fog_material.set_shader_parameter("rain_intensity", _rain_intensity)

	# ── Ambient pitch modulation ──────────────────────────────────
	if ambient_enabled and _ambient_audio:
		if not _ambient_audio.playing:
			_ambient_audio.play()
		_ambient_audio.pitch_scale = lerpf(_ambient_audio.pitch_scale, _ambient_target_pitch, delta * 2.0)
	elif _ambient_audio and _ambient_audio.playing:
		_ambient_audio.stop()

	# ── Ocean waves undertone (Silent Shore only) ─────────────────
	if ambient_enabled and _ocean_audio and background_shader == "rain_fog":
		if not _ocean_audio.playing:
			_ocean_audio.play()
		_ocean_audio.volume_db = ambient_volume_db - 6.0
	elif _ocean_audio and _ocean_audio.playing:
		_ocean_audio.stop()

	# ── Context-sensitive reverb ──────────────────────────────────
	if audio_enabled and _effects_bus_idx >= 0:
		if _is_streaming or _typing_speed > 1.0:
			_idle_timer = 0.0
			_reverb_wet_target = 0.05  # dry, close, present
		else:
			_idle_timer += delta
			if _idle_timer > 5.0:
				_reverb_wet_target = 0.35  # idle: sounds drift away
			else:
				_reverb_wet_target = lerpf(0.05, 0.35, _idle_timer / 5.0)
		_reverb_wet_current = lerpf(_reverb_wet_current, _reverb_wet_target, delta * 1.5)
		var reverb_fx := AudioServer.get_bus_effect(_effects_bus_idx, 1) as AudioEffectReverb
		if reverb_fx:
			reverb_fx.wet = _reverb_wet_current
			AudioServer.set_bus_effect_enabled(_effects_bus_idx, 1, _reverb_wet_current > 0.01)

	# ── Fire cursor + typing speed + typing sounds ────────────────
	if _fire_cursor and _input_field:
		_update_fire_cursor(delta)

	# ── Typing combo escalation ───────────────────────────────────
	var combo_target := clampf(_typing_speed / 25.0, 0.0, 1.0)  # 25 cps ≈ 150 WPM
	if combo_target > _combo_intensity:
		_combo_intensity = lerpf(_combo_intensity, combo_target, delta * 4.0)
	else:
		_combo_intensity = lerpf(_combo_intensity, 0.0, delta * 1.0)  # 2s settle

	if _combo_intensity > 0.01:
		# Screen shake: micro-tremors from typing fury
		if screen_shake_enabled and _shake_intensity < _combo_intensity * 1.5:
			_shake_intensity = _combo_intensity * 1.5 * shake_scale
		# CRT distortion creep
		if crt_enabled and _crt_rect:
			var combo_distort := crt_distortion_intensity + _combo_intensity * 0.08
			_crt_rect.material.set_shader_parameter("barrel_distortion", combo_distort)
		# Glow swell
		if glow_enabled:
			_glow_target = maxf(_glow_target, _combo_intensity * 0.4)
		# Ambient pitch climb
		if ambient_enabled and not _is_streaming:
			_ambient_target_pitch = 1.0 + _combo_intensity * 0.15
		# Starfield rotation boost
		if starfield_enabled and _starfield_rect and background_shader == "starfield":
			_starfield_material.set_shader_parameter("rotation_speed",
				0.01 + _combo_intensity * 0.09)
		# Fire cursor scale
		if fire_cursor_enabled and _fire_cursor:
			_fire_cursor.amount_ratio = lerpf(0.5, 1.0, _combo_intensity)
			_fire_cursor.process_material.scale_min = lerpf(2.0, 5.0, _combo_intensity)
	elif _combo_intensity <= 0.01 and _combo_intensity > 0.0:
		# Settle: reset boosted values
		_combo_intensity = 0.0
		if crt_enabled and _crt_rect:
			_crt_rect.material.set_shader_parameter("barrel_distortion", crt_distortion_intensity)
		if starfield_enabled and _starfield_rect and background_shader == "starfield":
			_starfield_material.set_shader_parameter("rotation_speed", 0.01)
		if fire_cursor_enabled and _fire_cursor:
			_fire_cursor.amount_ratio = 0.5
			_fire_cursor.process_material.scale_min = 2.0

	# ── Thought-lights (typing glow in background) ────────────────
	if _thought_light_material:
		# Ramp up when typing, slow fade when idle
		var target := clampf(_typing_speed / 8.0, 0.0, 1.0)
		if target > _thought_intensity:
			_thought_intensity = lerpf(_thought_intensity, target, delta * 4.0)
		else:
			_thought_intensity = lerpf(_thought_intensity, 0.0, delta * 0.4)
		_thought_light_material.set_shader_parameter("typing_intensity", _thought_intensity)
		if _scroll:
			_thought_light_material.set_shader_parameter("scroll_offset", float(_scroll.scroll_vertical))

	# ── WPM display ───────────────────────────────────────────────
	if wpm_enabled and _wpm_label:
		_update_wpm_label()

	# ── Time-of-day ambient shift ─────────────────────────────────
	if time_sync_enabled:
		_time_poll_timer -= delta
		if _time_poll_timer <= 0.0:
			_time_poll_timer = 60.0
			_update_time_of_day()

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

	# ── Emotional resonance ───────────────────────────────────────
	if _stream_char_count > 500:
		# Long response: the response landed with weight
		# Vignette breathe
		if crt_enabled and _crt_rect:
			var base_v: float = _crt_rect.material.get_shader_parameter("vignette_strength")
			var tween := create_tween()
			tween.tween_method(
				func(val: float): _crt_rect.material.set_shader_parameter("vignette_strength", val),
				base_v, base_v + 0.15, 1.0
			)
			tween.tween_method(
				func(val: float): _crt_rect.material.set_shader_parameter("vignette_strength", val),
				base_v + 0.15, base_v, 1.0
			)
		# Ambient swell
		_ambient_target_pitch = 0.95
		var pitch_tween := create_tween()
		pitch_tween.tween_callback(func(): _ambient_target_pitch = 1.0).set_delay(2.0)
		# Sub-bass tone
		if audio_enabled and _resonance_audio:
			_resonance_audio.stream = _sub_bass_stream
			_resonance_audio.volume_db = master_volume_db - 12.0
			_resonance_audio.play()
	elif _stream_char_count < 50 and _stream_char_count > 0:
		# Short response: barely disturbed the surface
		if audio_enabled:
			play_glass_tap()
		if crt_enabled and _crt_rect:
			var bright: float = _crt_rect.material.get_shader_parameter("brightness")
			_crt_rect.material.set_shader_parameter("brightness", bright + 0.1)
			var flick_tween := create_tween()
			flick_tween.tween_method(
				func(val: float): _crt_rect.material.set_shader_parameter("brightness", val),
				bright + 0.1, bright, 0.15
			)

func on_error() -> void:
	if audio_enabled:
		_error_audio.pitch_scale = randf_range(0.85, 1.0)
		_error_audio.play()

	if screen_shake_enabled:
		_shake_intensity = 8.0 * shake_scale

	if vhs_enabled:
		_vhs_intensity = _vhs_max_intensity

	# Error crumble particles
	if _error_crumble and _scroll:
		var scroll_rect := _scroll.get_rect()
		_error_crumble.position = Vector2(scroll_rect.size.x * 0.5, scroll_rect.size.y - 30.0)
		_error_crumble.emitting = true

	# Reverb spike: sound bounces off walls
	if _effects_bus_idx >= 0:
		_reverb_wet_current = 0.6
		var spike_tween := create_tween()
		spike_tween.tween_callback(func(): _reverb_wet_target = 0.05).set_delay(0.8)

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
		_vhs_intensity = minf(0.6, _vhs_max_intensity)

# ── Toggle management ─────────────────────────────────────────────

func _apply_toggles() -> void:
	if _crt_rect:
		_crt_rect.material.set_shader_parameter("effect_enabled", 1.0 if crt_enabled else 0.0)
		_crt_rect.material.set_shader_parameter("scanline_opacity", crt_scanline_intensity)
		_crt_rect.material.set_shader_parameter("barrel_distortion", crt_distortion_intensity)
		_crt_rect.material.set_shader_parameter("vignette_strength", crt_vignette_intensity)
		# Phosphor tint: warm amber for Silent Shore, white for Default
		var tint := Color(1.0, 0.95, 0.88) if _color_palette == "silent_shore" else Color.WHITE
		_crt_rect.material.set_shader_parameter("phosphor_tint", tint)
	if _glow_rect:
		_glow_rect.material.set_shader_parameter("effect_enabled", 1.0 if glow_enabled else 0.0)
		_glow_rect.material.set_shader_parameter("glow_radius", glow_radius)
		# Glow color: warm amber for Silent Shore, cyan-green for Default
		var glow_col := Color(0.9, 0.7, 0.3, 1.0) if _color_palette == "silent_shore" else Color(0.3, 1.0, 0.5, 1.0)
		_glow_rect.material.set_shader_parameter("glow_color", glow_col)
	if _fire_cursor:
		_fire_cursor.emitting = fire_cursor_enabled
	if _starfield_rect:
		_starfield_rect.visible = starfield_enabled
		# Set uniforms on whichever background shader is active
		if background_shader == "rain_fog" and _rain_fog_material:
			_rain_fog_material.set_shader_parameter("rain_density", star_density)
			_rain_fog_material.set_shader_parameter("fog_depth", 0.6)
		elif _starfield_material:
			_starfield_material.set_shader_parameter("star_density", star_density)
	# ── Volume propagation ────────────────────────────────────────
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
	# Auto-save on every toggle/slider change (but not during initial load)
	if _settings_loaded:
		save_settings()

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

# ── Preset management ────────────────────────────────────────────

func apply_preset(preset_name: String) -> void:
	if preset_name not in PRESETS:
		return
	_active_preset = preset_name
	var preset: Dictionary = PRESETS[preset_name]
	for key in preset:
		if key == "_color_palette":
			_color_palette = preset[key]
		elif key == "_audio_palette":
			_audio_palette = preset[key]
		else:
			set(key, preset[key])

	# VHS max intensity depends on preset
	_vhs_max_intensity = 0.3 if _audio_palette == "silent_shore" else 1.0

	_swap_background_shader()
	_apply_audio_palette(_audio_palette)
	_apply_toggles()
	preset_changed.emit(preset_name, _color_palette, _audio_palette)

func _apply_audio_palette(palette: String) -> void:
	# Stop ambient so _process restarts it with the new stream
	if _ambient_audio.playing:
		_ambient_audio.stop()
	# Ocean audio is managed by _process — stop it here so it restarts correctly
	if _ocean_audio and _ocean_audio.playing:
		_ocean_audio.stop()
	if palette == "silent_shore":
		_token_audio.stream = _water_drop_stream
		_complete_audio.stream = _distant_thunk_stream
		_error_audio.stream = _low_rumble_stream
		_boot_audio.stream = _crt_boot_stream
		_type_audio.stream = _muffled_keypress_stream
		_ambient_audio.stream = _rain_ambient_stream
		# Route effect audio to Effects bus (with reverb + LPF)
		_token_audio.bus = "Effects"
		_type_audio.bus = "Effects"
		_complete_audio.bus = "Effects"
		_error_audio.bus = "Effects"
		_boot_audio.bus = "Effects"
		# Enable bus effects
		if _effects_bus_idx >= 0:
			AudioServer.set_bus_effect_enabled(_effects_bus_idx, 0, true)  # LPF
			AudioServer.set_bus_effect_enabled(_effects_bus_idx, 1, true)  # Reverb
		# Ambient stays on Master (rain should be clear)
		_ambient_audio.bus = "Master"
	else:
		_token_audio.stream = _click_stream
		_complete_audio.stream = _thunk_stream
		_error_audio.stream = _buzz_stream
		_boot_audio.stream = _boot_stream
		_type_audio.stream = _keypress_stream
		_ambient_audio.stream = _ambient_stream
		# Route all back to Master
		_token_audio.bus = "Master"
		_type_audio.bus = "Master"
		_complete_audio.bus = "Master"
		_error_audio.bus = "Master"
		_boot_audio.bus = "Master"
		_ambient_audio.bus = "Master"
		# Disable bus effects
		if _effects_bus_idx >= 0:
			AudioServer.set_bus_effect_enabled(_effects_bus_idx, 0, false)
			AudioServer.set_bus_effect_enabled(_effects_bus_idx, 1, false)

func _swap_background_shader() -> void:
	if not _starfield_rect:
		return
	match background_shader:
		"rain_fog":
			_starfield_rect.material = _rain_fog_material
		_:
			_starfield_rect.material = _starfield_material

# ── Settings persistence ──────────────────────────────────────────

const SETTINGS_PATH := "user://settings.cfg"
const SAVE_KEYS := [
	"background_shader", "crt_enabled", "glow_enabled", "audio_enabled",
	"fire_cursor_enabled", "screen_shake_enabled", "vhs_enabled",
	"starfield_enabled", "wpm_enabled", "typing_sounds_enabled",
	"ambient_enabled", "hauntings_enabled", "time_sync_enabled", "master_volume_db",
	"ambient_volume_db", "crt_scanline_intensity", "crt_distortion_intensity",
	"crt_vignette_intensity", "glow_radius", "shake_scale", "star_density",
]

func save_settings(extra: Dictionary = {}) -> void:
	var cfg := ConfigFile.new()
	cfg.set_value("effects", "preset", _active_preset)
	for key in SAVE_KEYS:
		cfg.set_value("effects", key, get(key))
	# Extra settings from main.gd (font, enter_sends, etc.)
	for key in extra:
		cfg.set_value("text", key, extra[key])
	cfg.save(SETTINGS_PATH)

func load_settings() -> Dictionary:
	var cfg := ConfigFile.new()
	if cfg.load(SETTINGS_PATH) != OK:
		return {}
	# Load preset first, then overrides
	var preset_name: String = cfg.get_value("effects", "preset", "Default")
	if preset_name in PRESETS:
		apply_preset(preset_name)
	# Apply individual overrides (user may have tweaked after preset)
	for key in SAVE_KEYS:
		if cfg.has_section_key("effects", key):
			var val: Variant = cfg.get_value("effects", key)
			set(key, val)
	_swap_background_shader()
	_apply_audio_palette(_audio_palette)
	_apply_toggles()
	# Return text settings for main.gd to apply
	var text_settings := {}
	if cfg.has_section("text"):
		for key in cfg.get_section_keys("text"):
			text_settings[key] = cfg.get_value("text", key)
	return text_settings

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

# ── Time-of-day ──────────────────────────────────────────────────

func _update_time_of_day() -> void:
	var dt := Time.get_datetime_dict_from_system()
	var hour: int = dt["hour"]
	var minute: int = dt["minute"]
	var h := float(hour) + float(minute) / 60.0  # 0.0 .. 23.99

	# Map hour to tint + star multiplier + warmth
	# Night (22-5): deep blue tint, full stars, cool
	# Dawn (5-8): warm orange/pink, fading stars, warm
	# Day (8-17): neutral, minimal stars, neutral
	# Dusk (17-22): amber/purple, rising stars, warm
	var tint := Color(0.0, 0.0, 0.0, 0.0)
	var star_m := 1.0
	var warmth := 0.0

	if h < 5.0 or h >= 22.0:
		# Deep night
		tint = Color(-0.02, -0.01, 0.04, 0.0)
		star_m = 1.3
		warmth = -0.02
	elif h < 8.0:
		# Dawn transition (5-8)
		var t := (h - 5.0) / 3.0
		tint = Color(-0.02, -0.01, 0.04, 0.0).lerp(Color(0.03, 0.01, -0.02, 0.0), t)
		star_m = lerpf(1.3, 0.5, t)
		warmth = lerpf(-0.02, 0.04, t)
	elif h < 17.0:
		# Daytime
		tint = Color(0.0, 0.0, 0.0, 0.0)
		star_m = 0.5
		warmth = 0.0
	else:
		# Dusk transition (17-22)
		var t := (h - 17.0) / 5.0
		tint = Color(0.0, 0.0, 0.0, 0.0).lerp(Color(-0.02, -0.01, 0.04, 0.0), t)
		star_m = lerpf(0.5, 1.3, t)
		warmth = lerpf(0.0, -0.02, t)

	_time_tint = tint
	_time_star_mult = star_m
	_time_warmth = warmth

	# Apply star density modulation
	if starfield_enabled and _starfield_rect:
		if background_shader == "starfield" and _starfield_material:
			_starfield_material.set_shader_parameter("star_density", star_density * _time_star_mult)
		elif background_shader == "rain_fog" and _rain_fog_material:
			_rain_fog_material.set_shader_parameter("rain_warmth", clampf(warmth + 0.5, 0.0, 1.0))

	# Apply ambient pitch offset for time-of-day mood
	if h < 5.0 or h >= 22.0:
		_ambient_target_pitch = 0.97  # slightly lower at night
	elif not _is_streaming:
		_ambient_target_pitch = 1.0

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

# ── Silent Shore audio generators ─────────────────────────────────

func _generate_water_drop() -> AudioStreamWAV:
	# Upward chirp "drip" — softer than mechanical click
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 660  # ~15ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var freq := lerpf(400.0, 600.0, t * 66.0)  # upward chirp
		var env := exp(-t * 100.0) * 0.6
		# Slight tail for "reverb" feel
		env += maxf(0.0, 0.18 - t * 12.0) * 0.3
		var sample := sin(t * TAU * freq) * env
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_muffled_keypress() -> AudioStreamWAV:
	# Lower frequency, no click transient, faster decay
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 220  # ~5ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := 1.0 - t * 500.0
		var sample := sin(t * TAU * 600.0) * maxf(envelope, 0.0) * 0.7
		sample += sin(t * TAU * 1800.0) * maxf(envelope, 0.0) * 0.2
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_distant_thunk() -> AudioStreamWAV:
	# Quieter, lower, slower decay
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 3528  # ~80ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var envelope := exp(-t * 20.0)
		var sample := sin(t * TAU * 80.0) * envelope * 0.3
		sample += sin(t * TAU * 40.0) * envelope * 0.15
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_low_rumble() -> AudioStreamWAV:
	# Deep detuned rumble for errors — slow attack/decay
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 8820  # ~200ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var attack := clampf(t / 0.05, 0.0, 1.0)
		var decay := clampf((0.2 - t) / 0.15, 0.0, 1.0)
		var env := attack * decay
		var sample := sin(t * TAU * 45.0) * 0.5
		sample += sin(t * TAU * 55.0) * 0.4
		sample *= env
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_crt_boot() -> AudioStreamWAV:
	# Old CRT clicking on — quiet hum, rising whine, ending click
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var duration := 1.5
	var total := int(MIX_RATE * duration)
	for i in total:
		var t := float(i) / float(MIX_RATE)
		var progress := t / duration
		# Electric hum (60Hz + 120Hz harmonics) for first 0.5s
		var hum_env := clampf(1.0 - (t - 0.3) * 5.0, 0.0, 1.0) * clampf(t / 0.1, 0.0, 1.0)
		var hum := (sin(t * TAU * 60.0) + sin(t * TAU * 120.0) * 0.5) * 0.1 * hum_env
		# Rising whine (200Hz -> 4000Hz) from 0.3s onward
		var whine_t := clampf((t - 0.3) / 1.0, 0.0, 1.0)
		var whine_freq := lerpf(200.0, 4000.0, whine_t * whine_t)
		var whine_env := whine_t * clampf(1.0 - (t - 1.2) * 5.0, 0.0, 1.0)
		var whine := sin(t * TAU * whine_freq) * 0.1 * whine_env
		# Ending click
		var click := 0.0
		if t > duration - 0.005:
			click = sin((t - (duration - 0.005)) * TAU * 2000.0) * 0.3 * (1.0 - (t - (duration - 0.005)) / 0.005)
		var sample := (hum + whine + click) * 0.5
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_soft_whoosh() -> AudioStreamWAV:
	# Lower, softer send sound
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var samples := 3300  # ~75ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var env := exp(-t * 30.0)
		var sample := sin(t * TAU * (1200.0 - t * 10000.0)) * 0.15
		sample += sin(t * TAU * (800.0 - t * 6000.0)) * 0.1
		sample *= env
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_ocean_waves() -> AudioStreamWAV:
	# Distant ocean — slow filtered noise surges, very low frequency
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	stream.loop_mode = AudioStreamWAV.LOOP_FORWARD
	var loop_seconds := 8.0  # longer loop for ocean variation
	var total_samples := int(MIX_RATE * loop_seconds)
	stream.loop_end = total_samples
	var data := PackedByteArray()
	var prev := 0.0
	var prev2 := 0.0
	var prev3 := 0.0
	var rng_state := 77777
	for i in total_samples:
		var t := float(i) / float(MIX_RATE)
		# White noise source
		rng_state = (rng_state * 1103515245 + 12345) & 0x7FFFFFFF
		var white := float(rng_state) / float(0x7FFFFFFF) * 2.0 - 1.0
		# Heavy low-pass filtering (3 cascaded IIR stages for deep rumble)
		prev = prev * 0.92 + white * 0.08
		prev2 = prev2 * 0.94 + prev * 0.06
		prev3 = prev3 * 0.96 + prev2 * 0.04
		var filtered := prev3
		# Wave surge envelope — slow sine modulation at ~0.12Hz (one surge per ~8s)
		var surge := sin(t * TAU * 0.125) * 0.5 + 0.5
		surge *= surge  # sharpen peaks
		# Secondary smaller wave at ~0.3Hz
		var ripple := sin(t * TAU * 0.31) * 0.25 + 0.75
		filtered *= surge * ripple * 0.4
		# Crossfade for seamless loop
		var loop_t := float(i) / float(total_samples)
		var fade := 1.0
		if loop_t > 0.94:
			fade = (1.0 - loop_t) / 0.06
		elif loop_t < 0.06:
			fade = loop_t / 0.06
		filtered *= fade
		var s16 := int(clampf(filtered, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_glass_tap() -> AudioStreamWAV:
	# Short bright tap — like fingernail on window glass
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var duration := 0.06  # very short
	var total_samples := int(MIX_RATE * duration)
	var data := PackedByteArray()
	for i in total_samples:
		var t := float(i) / float(MIX_RATE)
		# Sharp attack, fast decay envelope
		var env := exp(-t * 80.0)
		# High-frequency ping (glass resonance around 2-4kHz)
		var sample := sin(t * TAU * 2800.0) * 0.5
		sample += sin(t * TAU * 3900.0) * 0.3
		sample += sin(t * TAU * 5200.0) * 0.15
		sample *= env * 0.4
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_glass_crack() -> AudioStreamWAV:
	# Crack — sharp transient with noise burst
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var duration := 0.15
	var total_samples := int(MIX_RATE * duration)
	var data := PackedByteArray()
	var rng_state := 55555
	var prev := 0.0
	for i in total_samples:
		var t := float(i) / float(MIX_RATE)
		rng_state = (rng_state * 1103515245 + 12345) & 0x7FFFFFFF
		var white := float(rng_state) / float(0x7FFFFFFF) * 2.0 - 1.0
		# Bright noise burst (minimal filtering)
		prev = prev * 0.3 + white * 0.7
		# Sharp crack envelope — instant attack, two-phase decay
		var env := exp(-t * 35.0) * 0.7 + exp(-t * 120.0) * 0.3
		# Add a snap transient in the first 2ms
		if t < 0.002:
			prev += sin(t * TAU * 1500.0) * 2.0
		var sample := prev * env * 0.5
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream

func _generate_sub_bass() -> AudioStreamWAV:
	# Very low tone (~40Hz, 200ms) — felt more than heard
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var duration := 0.25
	var total_samples := int(MIX_RATE * duration)
	var data := PackedByteArray()
	data.resize(total_samples * 2)
	for i in total_samples:
		var t := float(i) / float(MIX_RATE)
		var env := sin(t / duration * PI)  # smooth hump
		var sample := sin(t * TAU * 40.0) * 0.5
		sample += sin(t * TAU * 60.0) * 0.25
		sample *= env * 0.3
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data[i * 2] = s16 & 0xFF
		data[i * 2 + 1] = (s16 >> 8) & 0xFF
	stream.data = data
	return stream

func play_glass_tap() -> void:
	if _glass_audio and _glass_tap_stream:
		_glass_audio.stream = _glass_tap_stream
		_glass_audio.pitch_scale = randf_range(0.9, 1.15)
		_glass_audio.volume_db = -10.0
		_glass_audio.play()

func play_glass_crack() -> void:
	if _glass_audio and _glass_crack_stream:
		_glass_audio.stream = _glass_crack_stream
		_glass_audio.pitch_scale = randf_range(0.85, 1.1)
		_glass_audio.volume_db = -6.0
		_glass_audio.play()

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
	var chars_this_frame := maxf(float(current_len - _prev_text_length), 0.0)
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

	# Cursor breath: slow brightness pulse when idle (not typing)
	if _typing_speed < 0.5 and _input_field.has_focus():
		var breath := sin(Time.get_ticks_msec() * 0.001 * TAU / 3.0) * 0.2 + 0.8
		var caret_col := Color(1.0, 1.0, 1.0, breath)
		_input_field.add_theme_color_override("caret_color", caret_col)
	else:
		_input_field.add_theme_color_override("caret_color", Color.WHITE)

	# Ghost typing: phantom fragments when idle + empty input
	if _input_field.text.is_empty() and _typing_speed < 0.5 and not _is_streaming:
		_ghost_idle_timer += delta
		if _ghost_idle_timer >= GHOST_IDLE_DELAY and not _ghost_active:
			# Start a new ghost
			_ghost_active = true
			_ghost_text = GHOST_FRAGMENTS[randi() % GHOST_FRAGMENTS.size()]
			_ghost_char_idx = 0
			_ghost_char_timer = 0.0
			_ghost_fade_timer = 0.0
			_input_field.placeholder_text = ""
		if _ghost_active:
			if _ghost_char_idx < _ghost_text.length():
				# Type out one char at a time
				_ghost_char_timer += delta
				if _ghost_char_timer >= GHOST_CHAR_SPEED:
					_ghost_char_timer -= GHOST_CHAR_SPEED
					_ghost_char_idx += 1
					_input_field.placeholder_text = _ghost_text.substr(0, _ghost_char_idx)
			else:
				# Fully typed — hold for a beat then fade
				_ghost_fade_timer += delta
				if _ghost_fade_timer > 3.0:
					# Fade out by dimming placeholder color
					var fade := clampf(1.0 - (_ghost_fade_timer - 3.0) / 1.5, 0.0, 1.0)
					_input_field.add_theme_color_override("font_placeholder_color", Color(0.35, 0.35, 0.45, fade))
					if fade <= 0.0:
						_ghost_active = false
						_ghost_idle_timer = 0.0
						_input_field.placeholder_text = ""
						_input_field.add_theme_color_override("font_placeholder_color", Color(0.35, 0.35, 0.45, 1.0))
				else:
					_input_field.add_theme_color_override("font_placeholder_color", Color(0.35, 0.35, 0.45, 1.0))
	else:
		# User is typing or input has text — reset ghost state
		if _ghost_active:
			_ghost_active = false
			_input_field.placeholder_text = ""
			_input_field.add_theme_color_override("font_placeholder_color", Color(0.35, 0.35, 0.45, 1.0))
		_ghost_idle_timer = 0.0

	# Auto-grow input field height based on content
	var line_count := _input_field.get_line_count()
	var line_height := _input_field.get_line_height()
	var content_height := float(line_count * line_height) + 20.0  # padding
	var target_h := clampf(content_height, 60.0, 200.0)
	if absf(_input_field.custom_minimum_size.y - target_h) > 1.0:
		_input_field.custom_minimum_size.y = target_h
