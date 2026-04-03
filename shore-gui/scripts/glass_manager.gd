extends Node

# Manages condensation buildup on idle and glass cracks on click.

var _effects: Node
var _condensation_rect: ColorRect
var _cracks_rect: ColorRect
var _condensation_mat: ShaderMaterial
var _cracks_mat: ShaderMaterial

# Idle / condensation state
var _idle_time := 0.0
var _fog_buildup := 0.0
var _last_input_time := 0.0
var _wipe_pos := Vector2(-1.0, -1.0)
var _wipe_age := 10.0

const IDLE_THRESHOLD := 30.0  # seconds before condensation starts
const FOG_RATE := 0.015  # buildup per second after threshold
const FOG_DECAY := 0.05  # decay per second when active

# Crack state
var _cracks: Array[Dictionary] = []  # {pos: Vector2, age: float}
const MAX_CRACKS := 8
const CRACK_HEAL_RATE := 0.02  # age increase per second (heals in ~50s)

func setup(effects: Node, condensation: ColorRect, cracks: ColorRect) -> void:
	_effects = effects
	_condensation_rect = condensation
	_cracks_rect = cracks
	if _condensation_rect:
		_condensation_mat = _condensation_rect.material as ShaderMaterial
	if _cracks_rect:
		_cracks_mat = _cracks_rect.material as ShaderMaterial

func _input(event: InputEvent) -> void:
	if not _effects or not _effects.hauntings_enabled:
		return

	# Any input resets idle timer
	if event is InputEventKey or event is InputEventMouseMotion:
		_idle_time = 0.0

	# Mouse click — wipe condensation OR create crack
	if event is InputEventMouseButton and event.pressed and event.button_index == MOUSE_BUTTON_LEFT:
		var viewport_size := get_viewport().get_visible_rect().size
		var uv := event.position / viewport_size

		if _fog_buildup > 0.1:
			# Wipe the condensation
			_wipe_pos = uv
			_wipe_age = 0.0
			_fog_buildup = maxf(_fog_buildup - 0.15, 0.0)
		else:
			# Tap on glass — create crack
			_add_crack(uv)

func _process(delta: float) -> void:
	if not _effects or not _effects.hauntings_enabled:
		# Hide layers when not in haunting mode
		if _condensation_rect:
			_condensation_rect.visible = false
		if _cracks_rect:
			_cracks_rect.visible = false
		return

	var in_rain := _effects.background_shader == "rain_fog"
	if _condensation_rect:
		_condensation_rect.visible = in_rain
	if _cracks_rect:
		_cracks_rect.visible = in_rain
	if not in_rain:
		return

	# ── Condensation ──────────────────────────────────────────────
	_idle_time += delta
	_wipe_age += delta

	if _idle_time > IDLE_THRESHOLD:
		_fog_buildup = minf(_fog_buildup + delta * FOG_RATE, 0.85)
	else:
		_fog_buildup = maxf(_fog_buildup - delta * FOG_DECAY, 0.0)

	if _condensation_mat:
		_condensation_mat.set_shader_parameter("idle_time", _idle_time)
		_condensation_mat.set_shader_parameter("fog_buildup", _fog_buildup)
		_condensation_mat.set_shader_parameter("wipe_pos", _wipe_pos)
		_condensation_mat.set_shader_parameter("wipe_age", _wipe_age)

	# ── Glass cracks ──────────────────────────────────────────────
	for i in range(_cracks.size() - 1, -1, -1):
		_cracks[i]["age"] += delta * CRACK_HEAL_RATE
		if _cracks[i]["age"] >= 1.0:
			_cracks.remove_at(i)

	_update_crack_uniforms()

func _add_crack(uv: Vector2) -> void:
	if _cracks.size() >= MAX_CRACKS:
		# Replace oldest
		_cracks.pop_front()
	_cracks.append({"pos": uv, "age": 0.0})
	_update_crack_uniforms()

func _update_crack_uniforms() -> void:
	if not _cracks_mat:
		return
	for i in 8:
		var pos_name := "crack_%d" % i
		var age_name := "age_%d" % i
		if i < _cracks.size():
			_cracks_mat.set_shader_parameter(pos_name, _cracks[i]["pos"])
			_cracks_mat.set_shader_parameter(age_name, _cracks[i]["age"])
		else:
			_cracks_mat.set_shader_parameter(pos_name, Vector2(-1.0, -1.0))
			_cracks_mat.set_shader_parameter(age_name, 1.0)
