extends MarginContainer

@onready var bridge: ShoreBridge = $ShoreBridge
@onready var status_label: Label = $Layout/Header/StatusLabel
@onready var message_display: RichTextLabel = $Layout/MessageScroll/MessageDisplay
@onready var input_field: TextEdit = $Layout/InputArea/InputField
@onready var send_button: Button = $Layout/InputArea/SendButton
@onready var scroll: ScrollContainer = $Layout/MessageScroll
@onready var effects: Node = $EffectsManager
@onready var haunting_manager: Node = $HauntingManager
@onready var seagull_manager: Node = $SeagullManager
@onready var seagull_layer: CanvasLayer = $SeagullLayer
@onready var glass_manager: Node = $GlassManager
@onready var condensation_rect: ColorRect = $CondensationLayer/CondensationRect
@onready var glass_cracks_rect: ColorRect = $GlassCracksLayer/GlassCracksRect

const COLOR_PALETTES := {
	"default": {
		"user": "white",
		"assistant": "green",
		"system": "gray",
		"error": "red",
		"tool_success": "cyan",
		"tool_header": "magenta",
		"thinking": "gray",
		"metadata": "gray",
		"regen": "cyan",
		"phase": "gray",
	},
	"silent_shore": {
		"user": "#f5deb3",
		"assistant": "#8899aa",
		"system": "#6b7b8d",
		"error": "#a65050",
		"tool_success": "#5f8a8a",
		"tool_header": "#9988bb",
		"thinking": "#5a6a7a",
		"metadata": "#5a6570",
		"regen": "#5f8a8a",
		"phase": "#5a6570",
	},
}

var _streaming := false
var _stream_fx_open := false
var _character_name := "Assistant"
var _show_thinking := true
var _show_tools := true
var _config_panel: Control = null
var _config_scene := preload("res://scenes/config_panel.tscn")
var _font_size := 16
var _font_name := "default"  # "default", "mono", or a system font name
var _colors: Dictionary = COLOR_PALETTES["default"]
var _color_palette_name := "default"
var _phosphor_effect: PhosphorEffect
var _droplet_overlay: Control
var _dead_bird_label: Label
var _dead_bird_timer := 0.0
var _dead_bird_visible := false
var _needs_scroll := false
var _enter_sends := false
var _tilted := false

func _ready() -> void:
	# Register RichTextEffects for text animations
	_phosphor_effect = PhosphorEffect.new()
	message_display.install_effect(FadeInEffect.new())
	message_display.install_effect(_phosphor_effect)
	message_display.install_effect(WobbleEffect.new())

	# Bridge signals
	bridge.connected.connect(_on_connected)
	bridge.disconnected.connect(_on_disconnected)
	bridge.stream_start.connect(_on_stream_start)
	bridge.stream_chunk.connect(_on_stream_chunk)
	bridge.stream_end.connect(_on_stream_end)
	bridge.error_received.connect(_on_error)
	bridge.tool_call.connect(_on_tool_call)
	bridge.tool_result.connect(_on_tool_result)
	bridge.phase_changed.connect(_on_phase_changed)
	bridge.command_output.connect(_on_command_output)

	# UI signals
	send_button.pressed.connect(_on_send_pressed)

	# Preset signal
	effects.preset_changed.connect(_on_preset_changed)

	# Haunting system
	haunting_manager.setup(effects, _phosphor_effect)

	# Seagull system
	seagull_manager.setup(effects, seagull_layer)

	# Glass / condensation system
	glass_manager.setup(effects, condensation_rect, glass_cracks_rect)

	# Droplet overlay (rain accumulates on old messages)
	_droplet_overlay = preload("res://scripts/droplet_overlay.gd").new()
	scroll.add_child(_droplet_overlay)
	_droplet_overlay.setup(effects, message_display)
	_droplet_overlay.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)

	# Dead bird in the corner
	_dead_bird_label = Label.new()
	_dead_bird_label.text = "🐦"
	_dead_bird_label.add_theme_font_size_override("font_size", 20)
	_dead_bird_label.position = Vector2(8.0, 8.0)
	_dead_bird_label.modulate.a = 0.3
	_dead_bird_label.visible = false
	_dead_bird_label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(_dead_bird_label)
	_dead_bird_timer = randf_range(30.0, 120.0)

	# Load saved settings
	var text_settings := effects.load_settings()
	effects._settings_loaded = true
	for key in text_settings:
		_on_text_settings_changed(key, text_settings[key])

	# Auto-connect on launch
	status_label.text = "Connecting..."
	bridge.connect_to_daemon("", "")

func _input(event: InputEvent) -> void:
	if not (event is InputEventKey and event.pressed):
		return
	# Send message: Ctrl+Enter always, bare Enter when _enter_sends is on
	if event.keycode == KEY_ENTER and input_field.has_focus():
		if event.ctrl_pressed:
			_on_send_pressed()
			get_viewport().set_input_as_handled()
		elif _enter_sends and not event.shift_pressed:
			_on_send_pressed()
			get_viewport().set_input_as_handled()
	# F2 to toggle config panel
	if event.keycode == KEY_F2:
		_toggle_config_panel()
		get_viewport().set_input_as_handled()
	# Escape: close config panel first, cancel generation second
	if event.keycode == KEY_ESCAPE:
		if _config_panel and is_instance_valid(_config_panel):
			_toggle_config_panel()
			get_viewport().set_input_as_handled()
		elif _streaming:
			bridge.cancel_generation()
			get_viewport().set_input_as_handled()

func _process(delta: float) -> void:
	# ── Debounced scroll-to-bottom ────────────────────────────────
	if _needs_scroll:
		scroll.scroll_vertical = scroll.get_v_scroll_bar().max_value as int
		_needs_scroll = false

	# ── Streaming indicator (animated dots in status label) ───────
	if _streaming:
		var dots := ".".repeat((int(Time.get_ticks_msec() / 500) % 3) + 1)
		status_label.text = "%s — generating%s" % [_character_name, dots]

	# ── Dead bird appearances ─────────────────────────────────────
	if effects.background_shader == "rain_fog":
		_dead_bird_timer -= delta
		if _dead_bird_timer <= 0.0:
			_dead_bird_visible = not _dead_bird_visible
			_dead_bird_label.visible = _dead_bird_visible
			# Visible for 15-45s, hidden for 30-120s
			if _dead_bird_visible:
				_dead_bird_timer = randf_range(15.0, 45.0)
			else:
				_dead_bird_timer = randf_range(30.0, 120.0)
	elif _dead_bird_label.visible:
		_dead_bird_label.visible = false

	# ── Warm rain (check input for character name) ────────────────
	if effects.background_shader == "rain_fog" and effects._rain_fog_material:
		var text := input_field.text.to_lower()
		var warmth_target := 1.0 if _character_name.to_lower() in text else 0.0
		var current_warmth: float = effects._rain_fog_material.get_shader_parameter("rain_warmth")
		effects._rain_fog_material.set_shader_parameter("rain_warmth", lerpf(current_warmth, warmth_target, delta * 2.0))

func _on_send_pressed() -> void:
	if _streaming:
		bridge.cancel_generation()
		return
	var text := input_field.text.strip_edges()
	if text.is_empty() or not bridge.is_connected():
		return
	_append_user_message(text)
	bridge.send_message(text)
	input_field.text = ""
	effects.on_message_sent()

# ── Bridge signal handlers ────────────────────────────────────────

func _on_connected(server_name: String, characters: Array, _history_json: String, _config_json: String) -> void:
	# Set character name from first available character (may be overridden by status)
	if characters.size() > 0:
		_character_name = characters[0]
	else:
		_character_name = "Assistant"

	status_label.text = "%s — %s" % [_character_name, server_name]
	message_display.clear()

	# Request full history and status from daemon
	bridge.send_command("log", "{}")
	bridge.send_command("status", "{}")
	input_field.grab_focus()

func _on_disconnected(reason: String) -> void:
	status_label.text = "Disconnected: %s" % reason
	send_button.text = "Send"
	_close_stream_fx()
	_streaming = false

func _on_command_output(name: String, data_json: String) -> void:
	var data = JSON.parse_string(data_json)
	if not data or not data is Dictionary:
		return

	match name:
		"log":
			var messages: Array = data.get("messages", [])
			message_display.clear()
			for msg in messages:
				if msg is Dictionary:
					_render_history_message(msg)
			_scroll_to_bottom()
		"status":
			var character: String = data.get("character", "")
			if not character.is_empty():
				_character_name = character
				status_label.text = "%s — %s" % [_character_name, "connected"]

func _on_stream_start(is_regen: bool) -> void:
	_streaming = true
	var use_fx := _color_palette_name == "silent_shore"
	if is_regen:
		message_display.append_text("\n[color=%s][b]Regenerating...[/b][/color]\n" % _colors["regen"])
	var prefix := ""
	if use_fx:
		prefix = "[fadein][phosphor][wobble]"
		_stream_fx_open = true
	message_display.append_text("\n%s[color=%s][b]%s:[/b][/color] " % [prefix, _colors["assistant"], _character_name])
	send_button.text = "Stop"
	effects.on_new_message()
	effects.on_stream_start()

func _on_stream_chunk(text: String, content_type: String) -> void:
	if content_type == "thinking":
		if _show_thinking:
			message_display.append_text("[color=%s][i]%s[/i][/color]" % [_colors["thinking"], _escape_bbcode(text)])
	else:
		message_display.append_text(_escape_bbcode(text))
	effects.on_stream_chunk(text, content_type)
	_scroll_to_bottom()

func _on_stream_end(_content: String, metadata_json: String) -> void:
	_streaming = false
	send_button.text = "Send"
	status_label.text = "%s — connected" % _character_name
	_close_stream_fx()

	# ── THE CURSED TILT (0.1% chance per message, permanent) ─────
	if not _tilted and randf() < 0.001:
		_tilted = true
		rotation_degrees = 2.0
	effects.on_stream_end()
	var meta = JSON.parse_string(metadata_json)
	if meta and meta is Dictionary:
		var tokens = meta.get("tokens", {})
		var timing = meta.get("timing", {})
		message_display.append_text("\n[color=%s][i]%s | %d in / %d out | %dms[/i][/color]\n" % [
			_colors["metadata"],
			meta.get("model", "?"),
			tokens.get("input", 0),
			tokens.get("output", 0),
			timing.get("total_ms", 0),
		])
	_scroll_to_bottom()

func _on_error(message: String) -> void:
	_close_stream_fx()
	message_display.append_text("\n[color=%s][b]Error:[/b] %s[/color]\n" % [_colors["error"], _escape_bbcode(message)])
	effects.on_error()
	_scroll_to_bottom()

func _on_tool_call(_tool_id: String, tool_name: String, input_json: String) -> void:
	if not _show_tools:
		return
	message_display.append_text("\n[color=%s][b]  ▶ %s[/b][/color]" % [_colors["tool_header"], tool_name])
	var truncated := input_json.left(300)
	if input_json.length() > 300:
		truncated += "..."
	message_display.append_text("\n[color=%s]  │ %s[/color]" % [_colors["metadata"], _escape_bbcode(truncated)])

func _on_tool_result(_tool_id: String, tool_name: String, output: String, is_error: bool) -> void:
	if not _show_tools:
		return
	var color: String = _colors["error"] if is_error else _colors["tool_success"]
	var icon := "✗" if is_error else "◀"
	message_display.append_text("\n[color=%s][b]  %s %s[/b][/color]" % [color, icon, tool_name])
	var truncated := output.left(500)
	if output.length() > 500:
		truncated += "..."
	message_display.append_text("\n[color=%s]  │ %s[/color]" % [_colors["metadata"], _escape_bbcode(truncated)])

func _on_phase_changed(phase: String, model: String) -> void:
	var label := phase
	if not model.is_empty():
		label += " (%s)" % model
	message_display.append_text("\n[color=%s][i]Phase: %s[/i][/color]" % [_colors["phase"], label])

# ── History rendering ─────────────────────────────────────────────

func _render_history_message(msg: Dictionary) -> void:
	var role: String = msg.get("role", "")
	var content: String = msg.get("content", "")
	var content_blocks: Array = msg.get("content_blocks", [])

	match role:
		"user":
			message_display.append_text("\n[color=%s][b]You:[/b][/color] %s\n" % [_colors["user"], _escape_bbcode(content)])
		"system":
			message_display.append_text("\n[color=%s][b]System:[/b] %s[/color]\n" % [_colors["system"], _escape_bbcode(content)])
		"assistant":
			_render_assistant_message(content, content_blocks)

func _render_assistant_message(content: String, content_blocks: Array) -> void:
	var use_fx := _color_palette_name == "silent_shore"
	if content_blocks.is_empty():
		var open := "[wobble][phosphor]" if use_fx else ""
		var close := "[/phosphor][/wobble]" if use_fx else ""
		message_display.append_text("\n%s[color=%s][b]%s:[/b][/color] %s%s\n" % [
			open, _colors["assistant"], _character_name, _escape_bbcode(content), close
		])
		return

	# Build a tool_use_id -> tool_name map for labeling results
	var tool_names := {}
	for block in content_blocks:
		if block.get("type") == "tool_use":
			tool_names[block.get("id", "")] = block.get("name", "tool")

	var text_parts: PackedStringArray = []

	for block in content_blocks:
		var block_type: String = block.get("type", "")
		match block_type:
			"thinking":
				if _show_thinking:
					var thinking: String = block.get("thinking", "")
					if not thinking.is_empty():
						message_display.append_text("\n[color=%s][b]  ◆ thinking[/b][/color]" % _colors["tool_header"])
						for line in thinking.split("\n"):
							message_display.append_text("\n[color=%s][i]  │ %s[/i][/color]" % [_colors["thinking"], _escape_bbcode(line)])
			"redacted_thinking":
				if _show_thinking:
					message_display.append_text("\n[color=%s][b]  ◆ thinking[/b][/color]" % _colors["tool_header"])
					message_display.append_text("\n[color=%s][i]  │ [lb]redacted][/i][/color]" % _colors["thinking"])
			"tool_use":
				if _show_tools:
					var tool_name: String = block.get("name", "tool")
					var input_json := JSON.stringify(block.get("input", {}))
					message_display.append_text("\n[color=%s][b]  ▶ %s[/b][/color]" % [_colors["tool_header"], tool_name])
					var truncated := input_json.left(300)
					if input_json.length() > 300:
						truncated += "..."
					message_display.append_text("\n[color=%s]  │ %s[/color]" % [_colors["metadata"], _escape_bbcode(truncated)])
			"tool_result":
				if _show_tools:
					var tool_use_id: String = block.get("tool_use_id", "")
					var tool_name: String = tool_names.get(tool_use_id, "tool")
					var output: String = block.get("content", "")
					var is_error: bool = block.get("is_error", false)
					var color: String = _colors["error"] if is_error else _colors["tool_success"]
					var icon := "✗" if is_error else "◀"
					message_display.append_text("\n[color=%s][b]  %s %s[/b][/color]" % [color, icon, tool_name])
					var truncated := output.left(500)
					if output.length() > 500:
						truncated += "..."
					message_display.append_text("\n[color=%s]  │ %s[/color]" % [_colors["metadata"], _escape_bbcode(truncated)])
			"text":
				var text: String = block.get("text", "").strip_edges()
				if not text.is_empty():
					text_parts.append(text)

	var combined := "\n".join(text_parts)
	if not combined.strip_edges().is_empty():
		var open := "[wobble][phosphor]" if use_fx else ""
		var close := "[/phosphor][/wobble]" if use_fx else ""
		message_display.append_text("\n%s[color=%s][b]%s:[/b][/color] %s%s\n" % [
			open, _colors["assistant"], _character_name, _escape_bbcode(combined), close
		])

# ── Helpers ───────────────────────────────────────────────────────

func _append_user_message(text: String) -> void:
	var use_fx := _color_palette_name == "silent_shore"
	var open := "[fadein]" if use_fx else ""
	var close := "[/fadein]" if use_fx else ""
	message_display.append_text("\n%s[color=%s][b]You:[/b][/color] %s%s\n" % [open, _colors["user"], _escape_bbcode(text), close])
	_scroll_to_bottom()

func _escape_bbcode(text: String) -> String:
	return text.replace("[", "[lb]")

func _on_preset_changed(_preset_name: String, color_palette: String, _audio_palette: String) -> void:
	_apply_color_palette(color_palette)

func _apply_color_palette(palette_name: String) -> void:
	if palette_name in COLOR_PALETTES:
		_colors = COLOR_PALETTES[palette_name]
		_color_palette_name = palette_name

func _close_stream_fx() -> void:
	if _stream_fx_open:
		message_display.append_text("[/wobble][/phosphor][/fadein]")
		_stream_fx_open = false

func _scroll_to_bottom() -> void:
	_needs_scroll = true

func _toggle_config_panel() -> void:
	if _config_panel and is_instance_valid(_config_panel):
		# Slide out to the right, then free
		var tween := create_tween()
		tween.tween_property(_config_panel, "offset_left", 0.0, 0.15).set_ease(Tween.EASE_IN)
		var panel_ref := _config_panel
		tween.tween_callback(func():
			if is_instance_valid(panel_ref):
				panel_ref._on_close()
		)
		_config_panel = null
	else:
		_config_panel = _config_scene.instantiate()
		add_child(_config_panel)
		# Start offscreen (offset_left = 0 means right edge), slide in
		_config_panel.offset_left = 0.0
		var tween := create_tween()
		tween.tween_property(_config_panel, "offset_left", -360.0, 0.2).set_ease(Tween.EASE_OUT)
		_config_panel.setup(effects)
		_config_panel.text_changed.connect(_on_text_settings_changed)
		_config_panel.closed.connect(func(): _config_panel = null)

func _on_text_settings_changed(setting: String, value: Variant) -> void:
	match setting:
		"font_size":
			_font_size = int(value)
			_apply_font_size()
		"font_name":
			_font_name = str(value)
			_apply_font()
		"lcd_filter":
			var mode: int = int(value)
			# 0=disabled, 1=light, 2=normal
			message_display.add_theme_constant_override("lcd_subpixel_layout", mode)
			input_field.add_theme_constant_override("lcd_subpixel_layout", mode)
		"enter_sends":
			_enter_sends = bool(value)
	# Persist text settings alongside effects settings
	if effects._settings_loaded:
		effects.save_settings({
			"font_size": _font_size,
			"font_name": _font_name,
			"enter_sends": _enter_sends,
		})

func _apply_font_size() -> void:
	message_display.add_theme_font_size_override("normal_font_size", _font_size)
	message_display.add_theme_font_size_override("bold_font_size", _font_size)
	message_display.add_theme_font_size_override("italics_font_size", _font_size)
	message_display.add_theme_font_size_override("bold_italics_font_size", _font_size)
	message_display.add_theme_font_size_override("mono_font_size", _font_size)
	input_field.add_theme_font_size_override("font_size", _font_size)
	status_label.add_theme_font_size_override("font_size", _font_size)

func _apply_font() -> void:
	var font: Font = null
	if _font_name == "mono":
		font = SystemFont.new()
		font.font_names = PackedStringArray(["monospace", "Courier New", "DejaVu Sans Mono"])
		font.antialiasing = TextServer.FONT_ANTIALIASING_LCD
	elif _font_name != "default":
		font = SystemFont.new()
		font.font_names = PackedStringArray([_font_name])
		font.antialiasing = TextServer.FONT_ANTIALIASING_LCD

	if font:
		message_display.add_theme_font_override("normal_font", font)
		message_display.add_theme_font_override("bold_font", font)
		message_display.add_theme_font_override("italics_font", font)
		message_display.add_theme_font_override("bold_italics_font", font)
		message_display.add_theme_font_override("mono_font", font)
		input_field.add_theme_font_override("font", font)
	else:
		# Reset to default
		message_display.remove_theme_font_override("normal_font")
		message_display.remove_theme_font_override("bold_font")
		message_display.remove_theme_font_override("italics_font")
		message_display.remove_theme_font_override("bold_italics_font")
		message_display.remove_theme_font_override("mono_font")
		input_field.remove_theme_font_override("font")
