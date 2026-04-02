shader_type canvas_item;

uniform vec4 bg_color : source_color = vec4(0.02, 0.02, 0.06, 1.0);
uniform float scroll_offset : hint_range(-1000.0, 1000.0) = 0.0;
uniform float star_density : hint_range(0.0, 1.0) = 0.4;
uniform float rotation_speed : hint_range(0.0, 0.1) = 0.01;

// Pseudo-random hash
float hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

// Star layer: returns brightness for a given UV at a given scale
float star_layer(vec2 uv, float scale, float threshold) {
	vec2 grid = floor(uv * scale);
	vec2 frac_uv = fract(uv * scale);

	float rand_val = hash(grid);
	// Only some cells have stars
	float has_star = step(1.0 - threshold, rand_val);

	// Star position within cell (randomized)
	vec2 star_pos = vec2(hash(grid + 0.1), hash(grid + 0.2));
	float dist = length(frac_uv - star_pos);

	// Star size varies
	float star_size = hash(grid + 0.3) * 0.03 + 0.01;
	float brightness = smoothstep(star_size, 0.0, dist) * has_star;

	// Twinkle
	float twinkle = sin(TIME * (hash(grid + 0.4) * 3.0 + 1.0) + hash(grid + 0.5) * TAU) * 0.3 + 0.7;
	brightness *= twinkle;

	return brightness;
}

void fragment() {
	vec2 uv = UV;

	// Apply slow rotation around center
	float angle = TIME * rotation_speed;
	vec2 centered = uv - 0.5;
	float cos_a = cos(angle);
	float sin_a = sin(angle);
	uv = vec2(
		centered.x * cos_a - centered.y * sin_a,
		centered.x * sin_a + centered.y * cos_a
	) + 0.5;

	// Parallax offset from scroll
	float parallax = scroll_offset * 0.0001;

	// Three star layers at different scales and parallax depths
	float stars = 0.0;
	stars += star_layer(uv + vec2(0.0, parallax * 0.3), 80.0, star_density * 0.3) * 0.5;
	stars += star_layer(uv + vec2(0.0, parallax * 0.6), 40.0, star_density * 0.5) * 0.7;
	stars += star_layer(uv + vec2(0.0, parallax * 1.0), 20.0, star_density * 0.7) * 1.0;

	// Faint nebula-like color wash
	float nebula = sin(uv.x * 3.0 + TIME * 0.05) * sin(uv.y * 2.0 - TIME * 0.03) * 0.5 + 0.5;
	vec3 nebula_color = mix(vec3(0.05, 0.0, 0.1), vec3(0.0, 0.05, 0.1), nebula) * 0.3;

	// Star color: mostly white, slight blue/warm tint variation
	vec3 star_color = mix(vec3(0.8, 0.9, 1.0), vec3(1.0, 0.9, 0.7), hash(uv * 100.0));

	vec3 col = bg_color.rgb + nebula_color + star_color * stars;
	COLOR = vec4(col, 1.0);
}
