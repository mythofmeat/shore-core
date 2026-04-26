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

// Radial crack pattern — more realistic impact fracture
float crack_pattern(vec2 uv, vec2 center, float seed) {
	vec2 d = uv - center;
	// Correct for aspect ratio (assume ~3:2 window)
	d.x *= 1.5;
	float dist = length(d);

	// No crack beyond radius
	float max_radius = 0.12;
	if (dist > max_radius) return 0.0;

	float angle = atan(d.y, d.x);

	// ── Radial rays (main fracture lines) ────────────────────────
	float num_rays = floor(6.0 + hash(vec2(seed, 0.0)) * 5.0); // 6-10 rays
	// Each ray has a slightly irregular angle offset
	float ray_angle = angle * num_rays + seed * 5.0;
	float ray_wobble = sin(dist * 60.0 + seed * 3.0) * 0.3; // rays aren't perfectly straight
	float ray = abs(sin(ray_angle + ray_wobble));
	// Thinner lines: sharper step
	float ray_width = smoothstep(0.96, 1.0, ray);
	// Rays get thinner farther from center
	ray_width *= smoothstep(max_radius, max_radius * 0.15, dist);
	// Rays don't start right at center
	ray_width *= smoothstep(0.005, 0.015, dist);

	// ── Concentric impact rings ──────────────────────────────────
	// Inner impact circle
	float ring1 = abs(dist - 0.015);
	ring1 = smoothstep(0.002, 0.0005, ring1);

	// Second ring — slightly irregular
	float ring2_r = 0.04 + sin(angle * 3.0 + seed) * 0.005;
	float ring2 = abs(dist - ring2_r);
	ring2 = smoothstep(0.0025, 0.0005, ring2) * 0.7;

	// Outer ring — more irregular, connects ray endpoints
	float ring3_r = 0.075 + sin(angle * 5.0 + seed * 2.0) * 0.01;
	float ring3 = abs(dist - ring3_r);
	ring3 = smoothstep(0.003, 0.0005, ring3) * 0.4;

	// ── Inner web (small secondary fractures between rings) ──────
	float web_angle = angle * (num_rays * 2.0) + seed * 7.0;
	float web = abs(sin(web_angle));
	web = smoothstep(0.97, 1.0, web);
	// Only between inner rings
	web *= smoothstep(0.01, 0.02, dist) * smoothstep(0.06, 0.03, dist) * 0.4;

	// ── Combine ──────────────────────────────────────────────────
	float crack = ray_width * 0.9 + ring1 + ring2 + ring3 + web;

	// Slight refraction highlight along crack edges
	float highlight = smoothstep(0.003, 0.0, abs(dist - 0.016)) * 0.15;
	crack += highlight;

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

	// Crack color: bright white-ish lines with slight refraction tint
	vec3 crack_color = mix(vec3(0.75, 0.8, 0.88), vec3(0.9, 0.92, 0.95), total);
	COLOR = vec4(crack_color, total * 0.6);
}
