extends Control

# Water droplets that accumulate on old messages from the rain.
# Sits as a child of MessageScroll, overlaying the text.

var _effects: Node
var _message_display: RichTextLabel
var _droplets: Array[Dictionary] = []
var _spawn_timer := 0.0

const MAX_DROPLETS := 60
const SPAWN_INTERVAL := 0.8  # seconds between new droplets
const DROPLET_GROW_TIME := 3.0  # seconds to reach full size

func setup(effects: Node, display: RichTextLabel) -> void:
	_effects = effects
	_message_display = display
	mouse_filter = Control.MOUSE_FILTER_IGNORE

func _process(delta: float) -> void:
	if not _effects or _effects.background_shader != "rain_fog":
		# Clear droplets when not in rain mode
		if not _droplets.is_empty():
			_droplets.clear()
			queue_redraw()
		return

	_spawn_timer -= delta
	if _spawn_timer <= 0.0 and _droplets.size() < MAX_DROPLETS:
		_try_spawn_droplet()
		_spawn_timer = SPAWN_INTERVAL

	# Age droplets
	var changed := false
	for i in range(_droplets.size() - 1, -1, -1):
		_droplets[i]["age"] += delta
		changed = true

	if changed:
		queue_redraw()

func _try_spawn_droplet() -> void:
	if not _message_display:
		return
	var display_rect := _message_display.get_rect()
	# Only spawn on the upper 70% of visible content (older messages)
	var visible_height := display_rect.size.y
	var y := randf_range(0.0, visible_height * 0.7)
	var x := randf_range(20.0, display_rect.size.x - 20.0)
	var base_size := randf_range(1.5, 3.5)

	_droplets.append({
		"pos": Vector2(x, y),
		"size": base_size,
		"age": 0.0,
		"seed": randf(),
	})

func _draw() -> void:
	for drop in _droplets:
		var age: float = drop["age"]
		var grow_t := clampf(age / DROPLET_GROW_TIME, 0.0, 1.0)
		var radius: float = drop["size"] * grow_t
		if radius < 0.5:
			continue

		var pos: Vector2 = drop["pos"]
		# Slight downward drift as droplet grows (gravity)
		pos.y += grow_t * 1.5

		# Droplet color: translucent blue-gray
		var alpha := 0.15 + grow_t * 0.15
		var col := Color(0.5, 0.55, 0.7, alpha)

		# Draw droplet body
		draw_circle(pos, radius, col)
		# Tiny highlight (refraction)
		var highlight_pos := pos + Vector2(-radius * 0.3, -radius * 0.3)
		draw_circle(highlight_pos, radius * 0.35, Color(0.8, 0.85, 0.95, alpha * 0.5))
