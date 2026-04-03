shader_type canvas_item;

uniform vec4 bg_color : source_color = vec4(0.05, 0.06, 0.08, 1.0);
uniform float rain_density : hint_range(0.0, 1.0) = 0.5;
uniform float rain_angle : hint_range(-0.5, 0.5) = 0.15;
uniform float fog_depth : hint_range(0.0, 1.0) = 0.6;
uniform float scroll_offset : hint_range(-1000.0, 1000.0) = 0.0;
uniform float wind_offset : hint_range(-1.0, 1.0) = 0.0;
uniform float time_scale : hint_range(0.1, 3.0) = 1.0;
uniform float rain_intensity : hint_range(0.0, 2.0) = 1.0;
uniform float rain_warmth : hint_range(0.0, 1.0) = 0.0;

// Pseudo-random hash
float hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

// 2D value noise
float noise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	float a = hash(i);
	float b = hash(i + vec2(1.0, 0.0));
	float c = hash(i + vec2(0.0, 1.0));
	float d = hash(i + vec2(1.0, 1.0));
	return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Fog layer: large-scale noise with slow drift
float fog_layer(vec2 uv, float scale, float speed, float t) {
	vec2 offset = vec2(t * speed + wind_offset * 0.5, t * speed * 0.4);
	float n = noise((uv + offset) * scale);
	n += noise((uv + offset * 1.3) * scale * 2.0) * 0.5;
	n += noise((uv + offset * 0.7) * scale * 4.0) * 0.25;
	return n / 1.75;
}

// Rain streak for a single layer
float rain_layer(vec2 uv, float scale, float speed, float density_threshold, float t) {
	// Shear UV by rain angle for diagonal rain
	vec2 sheared = vec2(uv.x + uv.y * rain_angle, uv.y);
	// Scroll downward (subtract to move pattern down-screen)
	sheared.y -= t * speed;

	// Grid cells — use rectangular cells (taller than wide) for streak shape
	vec2 cell_scale = vec2(scale, scale * 0.25);
	vec2 grid = floor(sheared * cell_scale);
	vec2 frac_uv = fract(sheared * cell_scale);

	float rand_val = hash(grid);
	float has_rain = step(1.0 - density_threshold, rand_val);

	// Rain drop position within cell
	float drop_x = hash(grid + 0.1) * 0.6 + 0.2;
	float dist_x = abs(frac_uv.x - drop_x);

	// Streak width
	float streak_width = 0.025 + hash(grid + 0.2) * 0.015;

	// Streak shape: thin line with soft edges
	float streak = smoothstep(streak_width, streak_width * 0.3, dist_x);
	// Fade at top and bottom of cell for streaky appearance
	streak *= smoothstep(0.0, 0.15, frac_uv.y) * smoothstep(1.0, 0.7, frac_uv.y);

	// Brightness variation per drop
	float brightness = (0.4 + hash(grid + 0.4) * 0.6) * has_rain;

	return streak * brightness;
}

// Condensation streaks running down the glass
float condensation(vec2 uv, float t) {
	float channel_scale = 25.0;
	float channel_x = floor(uv.x * channel_scale);
	float channel_frac = fract(uv.x * channel_scale);

	float has_streak = step(0.82, hash(vec2(channel_x, 0.0)));

	// Wobble path
	float wobble = sin(uv.y * 6.0 + hash(vec2(channel_x, 1.0)) * TAU) * 0.12;
	float dist = abs(channel_frac - 0.5 + wobble);

	float width = 0.06 + hash(vec2(channel_x, 2.0)) * 0.06;
	float streak = smoothstep(width, width * 0.3, dist) * has_streak;

	// Animate downward
	float speed = 0.03 + hash(vec2(channel_x, 3.0)) * 0.04;
	float phase = fract(uv.y * 1.5 - t * speed + hash(vec2(channel_x, 4.0)));
	float fade = smoothstep(0.0, 0.25, phase) * smoothstep(1.0, 0.5, phase);

	return streak * fade * 0.2;
}

void fragment() {
	vec2 uv = UV;
	float t = TIME * time_scale;
	float parallax = scroll_offset * 0.0001;

	// ── Deep fog layer (slow, dark) ──────────────────────────────
	vec2 deep_uv = uv + vec2(0.0, parallax * 0.2);
	float deep = fog_layer(deep_uv, 1.5, 0.12, t);
	vec3 deep_color = mix(
		vec3(0.03, 0.04, 0.06),
		vec3(0.07, 0.09, 0.12),
		deep
	) * fog_depth;

	// ── Mid fog layer ────────────────────────────────────────────
	vec2 mid_uv = uv + vec2(0.0, parallax * 0.5);
	float mid = fog_layer(mid_uv, 3.0, 0.18, t);
	vec3 mid_color = mix(
		vec3(0.04, 0.06, 0.08),
		vec3(0.09, 0.11, 0.14),
		mid
	) * fog_depth * 0.5;

	// ── Near fog layer (fastest, lightest) ───────────────────────
	vec2 near_uv = uv + vec2(0.0, parallax * 0.8);
	float near_fog = fog_layer(near_uv, 5.0, 0.25, t);
	vec3 near_color = vec3(0.10, 0.12, 0.15) * near_fog * fog_depth * 0.25;

	// ── Combine fog ──────────────────────────────────────────────
	vec3 col = bg_color.rgb + deep_color + mid_color + near_color;

	// ── Rain layers (two depths for parallax) ────────────────────
	// rain_intensity modulates density threshold, speed, and brightness
	float ri = rain_intensity;
	float rain_near = rain_layer(uv + vec2(0.0, parallax * 0.6), 40.0, 1.2 * (0.6 + ri * 0.4), rain_density * 0.5 * ri, t);
	float rain_far = rain_layer(uv + vec2(0.0, parallax * 0.3), 25.0, 0.7 * (0.6 + ri * 0.4), rain_density * 0.7 * ri, t);

	// Rain color: bright cool streaks against dark background
	vec3 rain_col = vec3(0.4, 0.45, 0.55);
	col += rain_col * (rain_near * 0.7 + rain_far * 0.35) * (0.5 + ri * 0.5);

	// ── Condensation on glass ────────────────────────────────────
	// More condensation during heavier rain
	float cond = condensation(uv, t) * (0.6 + ri * 0.4);
	col += vec3(0.2, 0.22, 0.28) * cond;

	// ── Subtle color variation (sea-glass tint) ──────────────────
	float tint_val = sin(uv.x * 2.0 + t * 0.03) * sin(uv.y * 1.5 - t * 0.02) * 0.5 + 0.5;
	col += mix(vec3(0.01, 0.02, 0.015), vec3(0.015, 0.01, 0.02), tint_val) * 0.1;

	COLOR = vec4(col, 1.0);
}
