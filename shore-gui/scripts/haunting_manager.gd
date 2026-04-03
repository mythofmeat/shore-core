extends Node

var _effects: Node
var _crt_rect: ColorRect
var _starfield_rect: ColorRect
var _mystery_audio: AudioStreamPlayer
var _phosphor_effect: PhosphorEffect

# Haunting state
var _next_haunting := 20.0
var _active_haunting := ""
var _haunting_timer := 0.0
var _haunting_duration := 0.0

# Wind shift state
var _original_rain_angle := 0.15
var _target_rain_angle := 0.15

# Mystery sound
var _mystery_stream: AudioStreamWAV

const MIN_INTERVAL := 15.0
const MAX_INTERVAL := 45.0
const MIX_RATE := 44100

# Weighted haunting types
const HAUNTING_WEIGHTS := {
	"fog_part": 3,
	"pixel_shift": 4,
	"wind_shift": 3,
	"mystery_sound": 2,
	"phosphor_linger": 3,
}

func setup(effects: Node, phosphor_effect: PhosphorEffect) -> void:
	_effects = effects
	_phosphor_effect = phosphor_effect
	_crt_rect = effects.get_node("../CRTLayer/CRTRect")
	_starfield_rect = effects.get_node("../StarfieldLayer/StarfieldRect")
	_mystery_audio = $MysteryAudio
	_mystery_stream = _generate_mystery_sound()
	_mystery_audio.stream = _mystery_stream
	_mystery_audio.volume_db = -24.0

func _process(delta: float) -> void:
	if not _effects or not _effects.hauntings_enabled:
		return
	# Only haunt when rain_fog is active
	if _effects.background_shader != "rain_fog":
		return

	# Process active haunting
	if _active_haunting != "":
		_haunting_timer += delta
		_process_active_haunting(delta)
		return

	# Count down to next haunting
	_next_haunting -= delta
	if _next_haunting <= 0.0:
		_trigger_random_haunting()
		_next_haunting = randf_range(MIN_INTERVAL, MAX_INTERVAL)

func _trigger_random_haunting() -> void:
	# Build weighted selection
	var total_weight := 0
	for w in HAUNTING_WEIGHTS.values():
		total_weight += w
	var roll := randi_range(0, total_weight - 1)
	var cumulative := 0
	for haunting_type in HAUNTING_WEIGHTS:
		cumulative += HAUNTING_WEIGHTS[haunting_type]
		if roll < cumulative:
			_start_haunting(haunting_type)
			return

func _start_haunting(haunting_type: String) -> void:
	_active_haunting = haunting_type
	_haunting_timer = 0.0
	match haunting_type:
		"fog_part":
			_haunting_duration = 5.0
		"pixel_shift":
			_haunting_duration = 0.05  # ~3 frames
		"wind_shift":
			_haunting_duration = 5.0
			_original_rain_angle = 0.15
			_target_rain_angle = 0.15 + randf_range(-0.3, 0.3)
		"mystery_sound":
			_haunting_duration = 0.3
			_mystery_audio.pitch_scale = randf_range(0.7, 1.3)
			_mystery_audio.play()
		"phosphor_linger":
			_haunting_duration = 3.0
			if _phosphor_effect:
				_phosphor_effect.linger_active = true

func _process_active_haunting(_delta: float) -> void:
	var t := _haunting_timer
	match _active_haunting:
		"fog_part":
			var mat := _starfield_rect.material as ShaderMaterial
			if mat:
				# Slowly increase fog depth, then return
				var fog_extra: float
				if t < 2.0:
					fog_extra = (t / 2.0) * 0.2
				elif t < 5.0:
					fog_extra = lerpf(0.2, 0.0, (t - 2.0) / 3.0)
				else:
					fog_extra = 0.0
				mat.set_shader_parameter("fog_depth", 0.6 + fog_extra)
				# Nudge wind_offset to suggest movement
				mat.set_shader_parameter("wind_offset", sin(t * 0.8) * 0.15)
		"pixel_shift":
			if _crt_rect and t < 0.02:
				_crt_rect.material.set_shader_parameter("pixel_shift", 1.0)
			elif _crt_rect:
				_crt_rect.material.set_shader_parameter("pixel_shift", 0.0)
		"wind_shift":
			var mat := _starfield_rect.material as ShaderMaterial
			if mat:
				var angle: float
				if t < 1.0:
					angle = lerpf(_original_rain_angle, _target_rain_angle, t)
				elif t < 3.0:
					angle = _target_rain_angle
				else:
					angle = lerpf(_target_rain_angle, _original_rain_angle, (t - 3.0) / 2.0)
				mat.set_shader_parameter("rain_angle", angle)

	# Check if haunting is done
	if _haunting_timer >= _haunting_duration:
		_end_haunting()

func _end_haunting() -> void:
	match _active_haunting:
		"fog_part":
			var mat := _starfield_rect.material as ShaderMaterial
			if mat:
				mat.set_shader_parameter("fog_depth", 0.6)
				mat.set_shader_parameter("wind_offset", 0.0)
		"pixel_shift":
			if _crt_rect:
				_crt_rect.material.set_shader_parameter("pixel_shift", 0.0)
		"wind_shift":
			var mat := _starfield_rect.material as ShaderMaterial
			if mat:
				mat.set_shader_parameter("rain_angle", _original_rain_angle)
		"phosphor_linger":
			if _phosphor_effect:
				_phosphor_effect.linger_active = false
	_active_haunting = ""

func _generate_mystery_sound() -> AudioStreamWAV:
	# Ambiguous tone — could be machinery, could be organic
	var stream := AudioStreamWAV.new()
	stream.format = AudioStreamWAV.FORMAT_16_BITS
	stream.mix_rate = MIX_RATE
	var data := PackedByteArray()
	var freq := randf_range(200.0, 400.0)
	var am_freq := randf_range(3.0, 7.0)
	var samples := int(MIX_RATE * 0.2)  # 200ms
	for i in samples:
		var t := float(i) / float(MIX_RATE)
		var attack := clampf(t / 0.1, 0.0, 1.0)
		var decay := clampf((0.2 - t) / 0.1, 0.0, 1.0)
		var env := attack * decay * 0.03
		var am := sin(t * TAU * am_freq) * 0.5 + 0.5
		var sample := sin(t * TAU * freq) * env * am
		# Add slight harmonic
		sample += sin(t * TAU * freq * 1.5) * env * am * 0.3
		var s16 := int(clampf(sample, -1.0, 1.0) * 32767.0)
		data.append(s16 & 0xFF)
		data.append((s16 >> 8) & 0xFF)
	stream.data = data
	return stream
