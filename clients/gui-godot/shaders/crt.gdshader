shader_type canvas_item;

uniform sampler2D screen_tex : hint_screen_texture, filter_linear_mipmap;
uniform float scanline_opacity : hint_range(0.0, 1.0) = 0.15;
uniform float barrel_distortion : hint_range(0.0, 0.5) = 0.1;
uniform float aberration : hint_range(0.0, 10.0) = 1.0;
uniform float brightness : hint_range(0.5, 1.5) = 1.0;
uniform float vignette_strength : hint_range(0.0, 1.0) = 0.3;
uniform float effect_enabled : hint_range(0.0, 1.0) = 1.0;
uniform vec4 phosphor_tint : source_color = vec4(1.0, 1.0, 1.0, 1.0);
uniform float pixel_shift : hint_range(0.0, 2.0) = 0.0;

vec2 warp(vec2 uv, float distort) {
	vec2 delta = uv - 0.5;
	float d2 = dot(delta, delta);
	return uv + delta * d2 * distort;
}

void fragment() {
	vec2 shifted_uv = SCREEN_UV + vec2(pixel_shift * SCREEN_PIXEL_SIZE.x, 0.0);
	vec2 uv = warp(shifted_uv, barrel_distortion * effect_enabled);
	float in_bounds = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);

	// Chromatic aberration
	float ab_offset = aberration * SCREEN_PIXEL_SIZE.x * effect_enabled;
	float r = texture(screen_tex, uv + vec2(ab_offset, 0.0)).r;
	float g = texture(screen_tex, uv).g;
	float b = texture(screen_tex, uv - vec2(ab_offset, 0.0)).b;
	vec3 col = vec3(r, g, b);

	// Scanlines
	float scanline = sin(uv.y / SCREEN_PIXEL_SIZE.y * 3.14159265) * 0.5 + 0.5;
	col = mix(col, col * scanline, scanline_opacity * effect_enabled);

	// Vignette
	float v = length(uv - 0.5) * 2.0;
	col *= 1.0 - smoothstep(0.5, 1.0, v) * vignette_strength * effect_enabled;

	// Phosphor grid micro-pattern: visible only with barrel distortion
	float phosphor_vis = smoothstep(0.03, 0.08, barrel_distortion) * effect_enabled * 0.03;
	if (phosphor_vis > 0.001) {
		vec2 pixel = uv / SCREEN_PIXEL_SIZE;
		int px = int(mod(pixel.x, 3.0));
		vec3 phosphor_mask = vec3(
			px == 0 ? 1.0 : 0.2,
			px == 1 ? 1.0 : 0.2,
			px == 2 ? 1.0 : 0.2
		);
		col = mix(col, col * phosphor_mask, phosphor_vis);
	}

	// Brightness
	col *= mix(1.0, brightness, effect_enabled);

	// Phosphor tint (warm amber for Silent Shore, white for Default)
	col *= phosphor_tint.rgb;

	// Black outside warped bounds
	col *= in_bounds;

	COLOR = vec4(col, 1.0);
}
