extends Node

# Occasional seagulls that fly across the screen.
# Sometimes one hits the window and leaves a smear.

var _effects: Node
var _canvas: CanvasLayer
var _smear_rects: Array[ColorRect] = []

# Seagull state
var _next_seagull := 30.0
var _active_seagulls: Array[Dictionary] = []

# Smear shader (lazy-loaded)
var _smear_material: ShaderMaterial

const MIN_INTERVAL := 20.0
const MAX_INTERVAL := 60.0
const SMEAR_CHANCE := 0.15  # 15% chance of hitting the window
const SMEAR_FADE_TIME := 15.0  # seconds for smear to fade

func setup(effects: Node, canvas: CanvasLayer) -> void:
	_effects = effects
	_canvas = canvas

func _process(delta: float) -> void:
	if not _effects or not _effects.hauntings_enabled:
		return
	if _effects.background_shader != "rain_fog":
		return

	# Spawn timer
	_next_seagull -= delta
	if _next_seagull <= 0.0:
		_spawn_seagull()
		_next_seagull = randf_range(MIN_INTERVAL, MAX_INTERVAL)

	# Update active seagulls
	var finished: Array[int] = []
	for i in range(_active_seagulls.size() - 1, -1, -1):
		var gull: Dictionary = _active_seagulls[i]
		gull["time"] += delta
		var t: float = gull["time"]
		var duration: float = gull["duration"]
		var progress := t / duration

		if progress >= 1.0:
			# Seagull left screen — check for impact
			if gull["will_smear"]:
				_create_smear(gull["smear_pos"])
			# Remove the draw node
			var node: Node2D = gull["node"]
			node.queue_free()
			_active_seagulls.remove_at(i)
			continue

		# Update position
		var node: Node2D = gull["node"]
		var start: Vector2 = gull["start"]
		var end_pos: Vector2 = gull["end"]
		var mid_y_offset: float = gull["arc"]
		var x := lerpf(start.x, end_pos.x, progress)
		var y := lerpf(start.y, end_pos.y, progress) + sin(progress * PI) * mid_y_offset
		# Wing flap wobble
		y += sin(t * 8.0) * 2.0
		node.position = Vector2(x, y)
		node.queue_redraw()

	# Fade smears
	for i in range(_smear_rects.size() - 1, -1, -1):
		var rect: ColorRect = _smear_rects[i]
		if not is_instance_valid(rect):
			_smear_rects.remove_at(i)
			continue
		var alpha: float = rect.modulate.a
		alpha -= delta / SMEAR_FADE_TIME
		if alpha <= 0.0:
			rect.queue_free()
			_smear_rects.remove_at(i)
		else:
			rect.modulate.a = alpha

func _spawn_seagull() -> void:
	var viewport_size := get_viewport().get_visible_rect().size
	var from_left := randf() > 0.5
	var start_x := -30.0 if from_left else viewport_size.x + 30.0
	var end_x := viewport_size.x + 30.0 if from_left else -30.0
	var y_band := randf_range(0.05, 0.35)  # upper portion of screen
	var start_y := viewport_size.y * y_band
	var end_y := start_y + randf_range(-40.0, 40.0)

	var will_smear := randf() < SMEAR_CHANCE
	var smear_x := randf_range(0.2, 0.8) * viewport_size.x
	var smear_y := lerpf(start_y, end_y, 0.5) + randf_range(-20.0, 20.0)

	# Create draw node for the seagull (simple V shape)
	var gull_node := SeagullDraw.new()
	gull_node.z_index = 1
	_canvas.add_child(gull_node)
	gull_node.facing_left = not from_left

	var duration := randf_range(3.0, 6.0)
	_active_seagulls.append({
		"node": gull_node,
		"start": Vector2(start_x, start_y),
		"end": Vector2(end_x, end_y),
		"arc": randf_range(-30.0, -10.0),  # slight upward arc
		"duration": duration,
		"time": 0.0,
		"will_smear": will_smear,
		"smear_pos": Vector2(smear_x, smear_y),
	})

func _create_smear(pos: Vector2) -> void:
	# Small translucent smear mark on the glass
	var rect := ColorRect.new()
	rect.size = Vector2(randf_range(12.0, 24.0), randf_range(8.0, 16.0))
	rect.position = pos - rect.size * 0.5
	rect.rotation = randf_range(-0.3, 0.3)
	rect.color = Color(0.35, 0.38, 0.42, 0.0)
	rect.modulate.a = 0.4
	rect.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_canvas.add_child(rect)
	_smear_rects.append(rect)

# ── Inner class: draws a simple seagull V-shape ──────────────────

class SeagullDraw extends Node2D:
	var facing_left := false
	var _wing_phase := 0.0

	func _process(delta: float) -> void:
		_wing_phase += delta * 8.0

	func _draw() -> void:
		var wing_flap := sin(_wing_phase) * 4.0
		var dir := -1.0 if facing_left else 1.0
		# Body
		var body_len := 6.0
		draw_line(Vector2(-body_len * dir, 0.0), Vector2(body_len * dir, 0.0), Color(0.15, 0.18, 0.22), 1.5)
		# Left wing
		draw_line(Vector2(0.0, 0.0), Vector2(-8.0 * dir, -6.0 + wing_flap), Color(0.15, 0.18, 0.22), 1.5)
		# Right wing
		draw_line(Vector2(0.0, 0.0), Vector2(8.0 * dir, -6.0 - wing_flap), Color(0.15, 0.18, 0.22), 1.5)
