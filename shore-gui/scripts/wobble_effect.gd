class_name WobbleEffect extends RichTextEffect

# Messages drift slightly, like floating in water.
# BBCode: [wobble][/wobble]

var bbcode := "wobble"

func _process_custom_fx(char_fx: CharFXTransform) -> bool:
	var idx := float(char_fx.relative_index)
	var t := char_fx.elapsed_time

	# Slow horizontal drift — each character has a slightly different phase
	var x_offset := sin(t * 0.8 + idx * 0.15) * 1.2
	# Very slow vertical bob
	var y_offset := sin(t * 0.5 + idx * 0.1 + 1.7) * 0.8

	char_fx.offset += Vector2(x_offset, y_offset)
	return true
