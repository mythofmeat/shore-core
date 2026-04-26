@tool
class_name FadeInEffect extends RichTextEffect

var bbcode := "fadein"

func _process_custom_fx(char_fx: CharFXTransform) -> bool:
	var alpha := clampf(char_fx.elapsed_time * 3.0, 0.0, 1.0)
	char_fx.color.a *= alpha
	char_fx.offset.y = lerpf(2.0, 0.0, alpha)
	return true
