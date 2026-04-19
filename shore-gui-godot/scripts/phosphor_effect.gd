@tool
class_name PhosphorEffect extends RichTextEffect

var bbcode := "phosphor"

# Set by haunting_manager to extend the glow duration
var linger_active := false

func _process_custom_fx(char_fx: CharFXTransform) -> bool:
	var t := char_fx.elapsed_time
	var decay_end := 4.0 if linger_active else 1.5
	if t > 0.3 and t < decay_end:
		var glow_t := (t - 0.3) / (decay_end - 0.3)
		var glow := (1.0 - glow_t) * 0.15
		char_fx.color = char_fx.color.lightened(glow)
	return true
