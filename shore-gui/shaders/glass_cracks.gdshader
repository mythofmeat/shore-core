shader_type canvas_item;

// Up to 8 crack points, packed as (x, y) in UV space
// Age controls healing: 0.0 = fresh, 1.0 = fully healed (invisible)
uniform vec2 crack_0 = vec2(-1.0);
uniform vec2 crack_1 = vec2(-1.0);
uniform vec2 crack_2 = vec2(-1.0);
uniform vec2 crack_3 = vec2(-1.0);
uniform vec2 crack_4 = vec2(-1.0);
uniform vec2 crack_5 = vec2(-1.0);
uniform vec2 crack_6 = vec2(-1.0);
uniform vec2 crack_7 = vec2(-1.0);
uniform float age_0 : hint_range(0.0, 1.0) = 1.0;
uniform float age_1 : hint_range(0.0, 1.0) = 1.0;
uniform float age_2 : hint_range(0.0, 1.0) = 1.0;
uniform float age_3 : hint_range(0.0, 1.0) = 1.0;
uniform float age_4 : hint_range(0.0, 1.0) = 1.0;
uniform float age_5 : hint_range(0.0, 1.0) = 1.0;
uniform float age_6 : hint_range(0.0, 1.0) = 1.0;
uniform float age_7 : hint_range(0.0, 1.0) = 1.0;

float hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

// Radial crack pattern from a point
float crack_pattern(vec2 uv, vec2 center, float seed) {
	vec2 d = uv - center;
	float dist = length(d);

	// No crack beyond radius
	if (dist > 0.08) return 0.0;

	// Radial lines (cracks radiating outward)
	float angle = atan(d.y, d.x);
	float num_cracks = 5.0 + hash(vec2(seed, 0.0)) * 4.0;
	float ray = abs(sin(angle * num_cracks + seed * 3.0));
	ray = smoothstep(0.92, 1.0, ray);

	// Concentric ring (impact circle)
	float ring = abs(dist - 0.02);
	ring = smoothstep(0.003, 0.001, ring);

	// Inner web
	float inner_ring = abs(dist - 0.04);
	inner_ring = smoothstep(0.002, 0.0005, inner_ring) * 0.5;

	// Combine: rays fade with distance, rings are sharp
	float crack = ray * smoothstep(0.08, 0.01, dist) * 0.8;
	crack += ring;
	crack += inner_ring;

	// Ray width narrows at center
	crack *= smoothstep(0.003, 0.01, dist);

	return clamp(crack, 0.0, 1.0);
}

void fragment() {
	vec2 uv = UV;
	float total = 0.0;

	// Evaluate each crack point
	vec2 cracks[8] = { crack_0, crack_1, crack_2, crack_3, crack_4, crack_5, crack_6, crack_7 };
	float ages[8] = { age_0, age_1, age_2, age_3, age_4, age_5, age_6, age_7 };

	for (int i = 0; i < 8; i++) {
		if (cracks[i].x < 0.0) continue;
		float visibility = 1.0 - ages[i];
		if (visibility < 0.01) continue;
		float c = crack_pattern(uv, cracks[i], float(i) * 7.13);
		total += c * visibility;
	}

	total = clamp(total, 0.0, 1.0);

	// Crack color: bright white-ish lines on transparent bg
	vec3 crack_color = vec3(0.8, 0.85, 0.9);
	COLOR = vec4(crack_color, total * 0.5);
}
