extends MarginContainer

@onready var bridge: ShoreBridge = $ShoreBridge
@onready var status_label: Label = $Layout/Header/StatusLabel
@onready var message_display: RichTextLabel = $Layout/MessageScroll/MessageDisplay
@onready var input_field: TextEdit = $Layout/InputArea/InputField
@onready var send_button: Button = $Layout/InputArea/SendButton
@onready var scroll: ScrollContainer = $Layout/MessageScroll
@onready var effects: Node = $EffectsManager

var _streaming := false
var _character_name := "Assistant"
var _show_thinking := true
var _show_tools := true
var _config_panel: Control = null
var _config_scene := preload("res://scenes/config_panel.tscn")
var _font_size := 16
var _font_name := "default"  # "default", "mono", or a system font name

func _ready() -> void:
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

	# Auto-connect on launch
	status_label.text = "Connecting..."
	bridge.connect_to_daemon("", "")

func _input(event: InputEvent) -> void:
	if not (event is InputEventKey and event.pressed):
		return
	# Ctrl+Enter to send
	if event.keycode == KEY_ENTER and event.ctrl_pressed:
		if input_field.has_focus():
			_on_send_pressed()
			get_viewport().set_input_as_handled()
	# F2 to toggle config panel
	if event.keycode == KEY_F2:
		_toggle_config_panel()
		get_viewport().set_input_as_handled()

func _on_send_pressed() -> void:
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

func _on_disconnected(reason: String) -> void:
	status_label.text = "Disconnected: %s" % reason
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
	if is_regen:
		message_display.append_text("\n[color=cyan][b]Regenerating...[/b][/color]\n")
	message_display.append_text("\n[color=green][b]%s:[/b][/color] " % _character_name)
	effects.on_new_message()
	effects.on_stream_start()

func _on_stream_chunk(text: String, content_type: String) -> void:
	if content_type == "thinking":
		if _show_thinking:
			message_display.append_text("[color=gray][i]%s[/i][/color]" % _escape_bbcode(text))
	else:
		message_display.append_text(_escape_bbcode(text))
	effects.on_stream_chunk(text, content_type)
	_scroll_to_bottom()

func _on_stream_end(_content: String, metadata_json: String) -> void:
	_streaming = false
	effects.on_stream_end()
	var meta = JSON.parse_string(metadata_json)
	if meta and meta is Dictionary:
		var tokens = meta.get("tokens", {})
		var timing = meta.get("timing", {})
		message_display.append_text("\n[color=gray][i]%s | %d in / %d out | %dms[/i][/color]\n" % [
			meta.get("model", "?"),
			tokens.get("input", 0),
			tokens.get("output", 0),
			timing.get("total_ms", 0),
		])
	_scroll_to_bottom()

func _on_error(message: String) -> void:
	message_display.append_text("\n[color=red][b]Error:[/b] %s[/color]\n" % _escape_bbcode(message))
	effects.on_error()
	_scroll_to_bottom()

func _on_tool_call(_tool_id: String, tool_name: String, input_json: String) -> void:
	if not _show_tools:
		return
	message_display.append_text("\n[color=magenta][b]  ▶ %s[/b][/color]" % tool_name)
	var truncated := input_json.left(300)
	if input_json.length() > 300:
		truncated += "..."
	message_display.append_text("\n[color=gray]  │ %s[/color]" % _escape_bbcode(truncated))

func _on_tool_result(_tool_id: String, tool_name: String, output: String, is_error: bool) -> void:
	if not _show_tools:
		return
	var color := "red" if is_error else "cyan"
	var icon := "✗" if is_error else "◀"
	message_display.append_text("\n[color=%s][b]  %s %s[/b][/color]" % [color, icon, tool_name])
	var truncated := output.left(500)
	if output.length() > 500:
		truncated += "..."
	message_display.append_text("\n[color=gray]  │ %s[/color]" % _escape_bbcode(truncated))

func _on_phase_changed(phase: String, model: String) -> void:
	var label := phase
	if not model.is_empty():
		label += " (%s)" % model
	message_display.append_text("\n[color=gray][i]Phase: %s[/i][/color]" % label)

# ── History rendering ─────────────────────────────────────────────

func _render_history_message(msg: Dictionary) -> void:
	var role: String = msg.get("role", "")
	var content: String = msg.get("content", "")
	var content_blocks: Array = msg.get("content_blocks", [])

	match role:
		"user":
			message_display.append_text("\n[color=white][b]You:[/b][/color] %s\n" % _escape_bbcode(content))
		"system":
			message_display.append_text("\n[color=gray][b]System:[/b] %s[/color]\n" % _escape_bbcode(content))
		"assistant":
			_render_assistant_message(content, content_blocks)

func _render_assistant_message(content: String, content_blocks: Array) -> void:
	if content_blocks.is_empty():
		message_display.append_text("\n[color=green][b]%s:[/b][/color] %s\n" % [
			_character_name, _escape_bbcode(content)
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
						message_display.append_text("\n[color=magenta][b]  ◆ thinking[/b][/color]")
						for line in thinking.split("\n"):
							message_display.append_text("\n[color=gray][i]  │ %s[/i][/color]" % _escape_bbcode(line))
			"redacted_thinking":
				if _show_thinking:
					message_display.append_text("\n[color=magenta][b]  ◆ thinking[/b][/color]")
					message_display.append_text("\n[color=gray][i]  │ [lb]redacted][/i][/color]")
			"tool_use":
				if _show_tools:
					var tool_name: String = block.get("name", "tool")
					var input_json := JSON.stringify(block.get("input", {}))
					message_display.append_text("\n[color=magenta][b]  ▶ %s[/b][/color]" % tool_name)
					var truncated := input_json.left(300)
					if input_json.length() > 300:
						truncated += "..."
					message_display.append_text("\n[color=gray]  │ %s[/color]" % _escape_bbcode(truncated))
			"tool_result":
				if _show_tools:
					var tool_use_id: String = block.get("tool_use_id", "")
					var tool_name: String = tool_names.get(tool_use_id, "tool")
					var output: String = block.get("content", "")
					var is_error: bool = block.get("is_error", false)
					var color := "red" if is_error else "cyan"
					var icon := "✗" if is_error else "◀"
					message_display.append_text("\n[color=%s][b]  %s %s[/b][/color]" % [color, icon, tool_name])
					var truncated := output.left(500)
					if output.length() > 500:
						truncated += "..."
					message_display.append_text("\n[color=gray]  │ %s[/color]" % _escape_bbcode(truncated))
			"text":
				var text: String = block.get("text", "").strip_edges()
				if not text.is_empty():
					text_parts.append(text)

	var combined := "\n".join(text_parts)
	if not combined.strip_edges().is_empty():
		message_display.append_text("\n[color=green][b]%s:[/b][/color] %s\n" % [
			_character_name, _escape_bbcode(combined)
		])

# ── Helpers ───────────────────────────────────────────────────────

func _append_user_message(text: String) -> void:
	message_display.append_text("\n[color=white][b]You:[/b][/color] %s\n" % _escape_bbcode(text))
	_scroll_to_bottom()

func _escape_bbcode(text: String) -> String:
	return text.replace("[", "[lb]")

func _scroll_to_bottom() -> void:
	await get_tree().process_frame
	scroll.scroll_vertical = scroll.get_v_scroll_bar().max_value as int

func _toggle_config_panel() -> void:
	if _config_panel and is_instance_valid(_config_panel):
		_config_panel._on_close()
		_config_panel = null
	else:
		_config_panel = _config_scene.instantiate()
		add_child(_config_panel)
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
