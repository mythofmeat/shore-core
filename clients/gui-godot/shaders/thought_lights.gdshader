shader_type canvas_item;

uniform float typing_intensity : hint_range(0.0, 1.0) = 0.0;
uniform float scroll_offset : hint_range(-1000.0, 1000.0) = 0.0;
uniform int light_count : hint_range(0, 24) = 12;

// Hash functions
float hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

void fragment() {
	vec2 uv = UV;
	float parallax = scroll_offset * 0.00005;
	float t = TIME;
	float intensity = typing_intensity;

	vec3 col = vec3(0.0);
	float total_alpha = 0.0;

	for (int i = 0; i < 24; i++) {
		if (i >= light_count) break;

		float fi = float(i);
		// Each light has a unique seed
		float seed = fi * 7.31;

		// Position: slow drift based on unique sine paths
		float base_x = hash(vec2(seed, 0.0));
		float base_y = hash(vec2(seed, 1.0));
		float drift_speed = 0.02 + hash(vec2(seed, 2.0)) * 0.04;
		float drift_radius = 0.03 + hash(vec2(seed, 3.0)) * 0.08;

		vec2 pos = vec2(
			base_x + sin(t * drift_speed * TAU + seed) * drift_radius,
			base_y + cos(t * drift_speed * 0.7 * TAU + seed * 1.3) * drift_radius + parallax
		);

		// Distance from this fragment to the light
		float dist = distance(uv, pos);

		// Light size and glow
		float size = 0.005 + hash(vec2(seed, 4.0)) * 0.01;
		float glow = size / (dist * dist + size);
		glow = clamp(glow * 0.003, 0.0, 1.0);

		// Per-light twinkle — independent phase and rate
		float twinkle_rate = 0.3 + hash(vec2(seed, 5.0)) * 0.6;
		float twinkle = sin(t * twinkle_rate * TAU + seed * 5.0) * 0.3 + 0.7;

		// Color: warm white to pale gold
		float hue_shift = hash(vec2(seed, 6.0));
		vec3 light_color = mix(
			vec3(1.0, 0.95, 0.85),
			vec3(0.85, 0.9, 1.0),
			hue_shift
		);

		float contribution = glow * twinkle * intensity;
		col += light_color * contribution;
		total_alpha += contribution;
	}

	total_alpha = clamp(total_alpha, 0.0, 1.0);
	COLOR = vec4(col, total_alpha);
}
