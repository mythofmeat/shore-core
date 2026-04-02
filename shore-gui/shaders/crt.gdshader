shader_type canvas_item;

uniform sampler2D screen_tex : hint_screen_texture, filter_linear_mipmap;
uniform float scanline_opacity : hint_range(0.0, 1.0) = 0.15;
uniform float barrel_distortion : hint_range(0.0, 0.5) = 0.1;
uniform float aberration : hint_range(0.0, 10.0) = 1.0;
uniform float brightness : hint_range(0.5, 1.5) = 1.0;
uniform float vignette_strength : hint_range(0.0, 1.0) = 0.3;
uniform float effect_enabled : hint_range(0.0, 1.0) = 1.0;

vec2 warp(vec2 uv, float distort) {
	vec2 delta = uv - 0.5;
	float d2 = dot(delta, delta);
	return uv + delta * d2 * distort;
}

void fragment() {
	vec2 uv = warp(SCREEN_UV, barrel_distortion * effect_enabled);
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

	// Brightness
	col *= mix(1.0, brightness, effect_enabled);

	// Black outside warped bounds
	col *= in_bounds;

	COLOR = vec4(col, 1.0);
}
