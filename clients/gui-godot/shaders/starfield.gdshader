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

// Shooting star: rare bright streak across the field
float shooting_star(vec2 uv, float t) {
	// Each shooting star has an ~60s cycle, offset by a seed
	float total = 0.0;
	for (int i = 0; i < 3; i++) {
		float fi = float(i);
		float seed = fi * 37.73;
		float cycle = 30.0 + fi * 25.0; // 30s, 55s, 80s cycles
		float phase = mod(t + seed, cycle);
		float streak_duration = 0.5;
		if (phase > streak_duration) continue;

		float progress = phase / streak_duration;
		// Start and end positions (seeded per cycle instance)
		float cycle_id = floor((t + seed) / cycle);
		float sx = hash(vec2(cycle_id, seed));
		float sy = hash(vec2(cycle_id, seed + 1.0)) * 0.6; // upper 60%
		// Direction: roughly diagonal
		float dx = 0.3 + hash(vec2(cycle_id, seed + 2.0)) * 0.2;
		float dy = 0.15 + hash(vec2(cycle_id, seed + 3.0)) * 0.1;

		vec2 head = vec2(sx + dx * progress, sy + dy * progress);
		vec2 tail = vec2(sx + dx * max(progress - 0.3, 0.0), sy + dy * max(progress - 0.3, 0.0));

		// Distance from point to line segment (head to tail)
		vec2 pa = uv - tail;
		vec2 ba = head - tail;
		float seg_len = length(ba);
		if (seg_len < 0.001) continue;
		float h_param = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
		float dist = length(pa - ba * h_param);

		// Brightness: sharp line with glow
		float streak = smoothstep(0.004, 0.0, dist);
		float glow = smoothstep(0.02, 0.0, dist) * 0.3;
		// Fade along the tail
		float fade = h_param;
		total += (streak + glow) * fade * (1.0 - progress * 0.5);
	}
	return clamp(total, 0.0, 1.0);
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

	// Shooting stars
	float shoot = shooting_star(uv, TIME);

	// Faint nebula-like color wash
	float nebula = sin(uv.x * 3.0 + TIME * 0.05) * sin(uv.y * 2.0 - TIME * 0.03) * 0.5 + 0.5;
	vec3 nebula_color = mix(vec3(0.05, 0.0, 0.1), vec3(0.0, 0.05, 0.1), nebula) * 0.3;

	// Star color: mostly white, slight blue/warm tint variation
	vec3 star_color = mix(vec3(0.8, 0.9, 1.0), vec3(1.0, 0.9, 0.7), hash(uv * 100.0));

	vec3 col = bg_color.rgb + nebula_color + star_color * stars;
	// Shooting star: bright white-blue streak
	col += vec3(0.9, 0.95, 1.0) * shoot;
	COLOR = vec4(col, 1.0);
}
