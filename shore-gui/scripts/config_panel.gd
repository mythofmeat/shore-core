extends PanelContainer

signal closed
signal text_changed(setting: String, value: Variant)

var _effects: Node
var _value_labels: Dictionary = {}  # slider_name -> Label

func _ready() -> void:
	var style := StyleBoxFlat.new()
	style.bg_color = Color(0.08, 0.08, 0.12, 0.95)
	style.border_color = Color(0.3, 0.3, 0.4, 1.0)
	style.set_border_width_all(1)
	style.set_corner_radius_all(4)
	add_theme_stylebox_override("panel", style)

func setup(effects: Node) -> void:
	_effects = effects
	_refresh_all()
	# Set preset dropdown to match active preset
	var preset_option := find_child("PresetOption", true, false) as OptionButton
	if preset_option and _effects:
		for i in preset_option.item_count:
			if preset_option.get_item_text(i) == _effects._active_preset:
				preset_option.select(i)
				break

func _refresh_all() -> void:
	if not _effects:
		return
	# Toggles
	_set_toggle("CRT", _effects.crt_enabled)
	_set_toggle("Glow", _effects.glow_enabled)
	_set_toggle("VHS", _effects.vhs_enabled)
	_set_toggle("Starfield", _effects.starfield_enabled)
	_set_toggle("ScreenShake", _effects.screen_shake_enabled)
	_set_toggle("FireCursor", _effects.fire_cursor_enabled)
	_set_toggle("WPM", _effects.wpm_enabled)
	_set_toggle("Audio", _effects.audio_enabled)
	_set_toggle("TypingSounds", _effects.typing_sounds_enabled)
	_set_toggle("Ambient", _effects.ambient_enabled)
	_set_toggle("TimeSync", _effects.time_sync_enabled)
	# Volume sliders
	_set_slider("MasterVolume", _effects.master_volume_db, -30.0, 0.0)
	_set_slider("AmbientVolume", _effects.ambient_volume_db, -40.0, -10.0)
	# Intensity sliders
	_set_slider("Scanline", _effects.crt_scanline_intensity, 0.0, 1.0)
	_set_slider("Distortion", _effects.crt_distortion_intensity, 0.0, 0.5)
	_set_slider("Vignette", _effects.crt_vignette_intensity, 0.0, 1.0)
	_set_slider("GlowRadius", _effects.glow_radius, 0.0, 10.0)
	_set_slider("ShakeScale", _effects.shake_scale, 0.0, 1.0)
	_set_slider("StarDensity", _effects.star_density, 0.0, 1.0)

func _set_toggle(effect_name: String, value: bool) -> void:
	var toggle := find_child(effect_name + "Toggle", true, false) as CheckButton
	if toggle:
		toggle.set_pressed_no_signal(value)

func _set_slider(slider_name: String, value: float, min_val: float, max_val: float) -> void:
	var slider := find_child(slider_name + "Slider", true, false) as HSlider
	if slider:
		slider.min_value = min_val
		slider.max_value = max_val
		slider.step = (max_val - min_val) / 100.0
		slider.value = value
		_ensure_value_label(slider_name, slider, min_val, max_val)

func _ensure_value_label(slider_name: String, slider: HSlider, min_val: float, max_val: float) -> void:
	if slider_name in _value_labels:
		_update_value_label(slider_name, slider.value, min_val, max_val)
		return
	# Create a small label after the slider
	var label := Label.new()
	label.name = slider_name + "Value"
	label.add_theme_font_size_override("font_size", 12)
	label.add_theme_color_override("font_color", Color(0.5, 0.5, 0.6))
	label.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
	# Insert after the slider in its parent
	var parent := slider.get_parent()
	var idx := slider.get_index() + 1
	parent.add_child(label)
	parent.move_child(label, idx)
	_value_labels[slider_name] = label
	_update_value_label(slider_name, slider.value, min_val, max_val)
	# Connect value_changed for live updates
	var sname := slider_name
	var mn := min_val
	var mx := max_val
	slider.value_changed.connect(func(v: float): _update_value_label(sname, v, mn, mx))

func _update_value_label(slider_name: String, value: float, min_val: float, max_val: float) -> void:
	if slider_name not in _value_labels:
		return
	var label: Label = _value_labels[slider_name]
	# Volume sliders: show as percentage
	if slider_name in ["MasterVolume", "AmbientVolume"]:
		var pct := int((value - min_val) / (max_val - min_val) * 100.0)
		label.text = "%d%%" % pct
	else:
		label.text = "%.2f" % value

# ── Handlers ──────────────────────────────────────────────────────

func _on_toggle(value: bool, property: String) -> void:
	if _effects:
		_effects.set(property, value)
		_effects._apply_toggles()
		_check_custom_preset()

func _on_slider(value: float, property: String) -> void:
	if _effects:
		_effects.set(property, value)
		_effects._apply_toggles()
		_check_custom_preset()

func _check_custom_preset() -> void:
	if not _effects or _effects._active_preset == "Custom":
		return
	# Check if current values diverge from active preset
	var preset_name: String = _effects._active_preset
	if preset_name not in _effects.PRESETS:
		return
	var preset: Dictionary = _effects.PRESETS[preset_name]
	for key in preset:
		if key.begins_with("_"):
			continue
		var current: Variant = _effects.get(key)
		var expected: Variant = preset[key]
		if current != expected:
			_effects._active_preset = "Custom"
			var preset_option := find_child("PresetOption", true, false) as OptionButton
			if preset_option:
				# Add "Custom" if not present, then select it
				var custom_idx := -1
				for i in preset_option.item_count:
					if preset_option.get_item_text(i) == "Custom":
						custom_idx = i
						break
				if custom_idx < 0:
					preset_option.add_item("Custom")
					custom_idx = preset_option.item_count - 1
				preset_option.select(custom_idx)
			return

func _on_preset_selected(index: int) -> void:
	var preset_option := find_child("PresetOption", true, false) as OptionButton
	if preset_option and _effects:
		var preset_name := preset_option.get_item_text(index)
		_effects.apply_preset(preset_name)
		_refresh_all()

func _on_professional_mode(pressed: bool) -> void:
	if _effects:
		_effects.set_all_effects(not pressed)
		_refresh_all()

func _on_close() -> void:
	closed.emit()
	queue_free()

# ── Text settings ─────────────────────────────────────────────────

func _on_font_selected(index: int) -> void:
	var option := find_child("FontOption", true, false) as OptionButton
	if option:
		text_changed.emit("font_name", option.get_item_text(index))

func _on_font_size_changed(value: float) -> void:
	text_changed.emit("font_size", value)

func _on_lcd_filter_selected(index: int) -> void:
	text_changed.emit("lcd_filter", index)

func _on_enter_sends_toggled(pressed: bool) -> void:
	text_changed.emit("enter_sends", pressed)
