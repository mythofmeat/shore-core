shader_type canvas_item;

uniform sampler2D screen_tex : hint_screen_texture, filter_linear_mipmap;
uniform float intensity : hint_range(0.0, 1.0) = 0.0;

void fragment() {
	vec4 original = texture(screen_tex, SCREEN_UV);

	// Horizontal displacement — scanline-style offset
	float time_val = TIME * 20.0;
	float line_noise = fract(sin(SCREEN_UV.y * 400.0 + time_val) * 43758.5453);
	float displacement = (line_noise - 0.5) * 0.02 * intensity;
	vec2 displaced_uv = SCREEN_UV + vec2(displacement, 0.0);

	// Color bleed — offset R and B channels vertically
	float color_bleed = 0.003 * intensity;
	float r = texture(screen_tex, displaced_uv + vec2(0.0, color_bleed)).r;
	float g = texture(screen_tex, displaced_uv).g;
	float b = texture(screen_tex, displaced_uv - vec2(0.0, color_bleed)).b;
	vec3 col = vec3(r, g, b);

	// Static noise
	float noise = fract(sin(dot(SCREEN_UV * TIME, vec2(12.9898, 78.233))) * 43758.5453);
	col = mix(col, vec3(noise), intensity * 0.15);

	// Tracking bar — a bright horizontal band that scrolls
	float bar_pos = fract(TIME * 0.3);
	float bar_dist = abs(SCREEN_UV.y - bar_pos);
	float bar = smoothstep(0.03, 0.0, bar_dist) * intensity * 0.3;
	col += vec3(bar);

	COLOR = vec4(mix(original.rgb, col, intensity), original.a);
}
