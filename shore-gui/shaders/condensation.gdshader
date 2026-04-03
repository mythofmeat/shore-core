shader_type canvas_item;

uniform float idle_time : hint_range(0.0, 300.0) = 0.0;
uniform float fog_buildup : hint_range(0.0, 1.0) = 0.0;
uniform vec2 wipe_pos = vec2(-1.0, -1.0);
uniform float wipe_radius : hint_range(0.0, 0.3) = 0.12;
uniform float wipe_age : hint_range(0.0, 10.0) = 10.0;

float hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

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

void fragment() {
	vec2 uv = UV;
	float t = TIME;

	// Condensation fog: multi-octave noise
	float n = noise(uv * 4.0 + vec2(t * 0.01, t * 0.005)) * 0.5;
	n += noise(uv * 8.0 + vec2(-t * 0.02, t * 0.01)) * 0.3;
	n += noise(uv * 16.0 + vec2(t * 0.03, -t * 0.015)) * 0.2;

	// Condensation droplet texture
	float droplets = noise(uv * 40.0 + vec2(0.0, t * 0.002));
	droplets = smoothstep(0.55, 0.65, droplets);

	// Combine fog and droplets
	float condensation = (n * 0.7 + droplets * 0.3) * fog_buildup;

	// Wipe clear zone — grows foggy again over time
	if (wipe_pos.x >= 0.0) {
		float dist = distance(uv, wipe_pos);
		// Wipe clears in a soft circle, re-fogs based on wipe_age
		float wipe_clear = smoothstep(wipe_radius, wipe_radius * 0.3, dist);
		float refog = clamp(wipe_age / 8.0, 0.0, 1.0);
		condensation *= mix(1.0 - wipe_clear, 1.0, refog);
	}

	// Foggy white overlay
	vec3 fog_color = vec3(0.7, 0.72, 0.75);
	COLOR = vec4(fog_color, condensation * 0.6);
}
