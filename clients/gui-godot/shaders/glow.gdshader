shader_type canvas_item;

uniform sampler2D screen_tex : hint_screen_texture, filter_linear_mipmap;
uniform float glow_intensity : hint_range(0.0, 1.0) = 0.0;
uniform float glow_radius : hint_range(0.0, 10.0) = 3.0;
uniform vec4 glow_color : source_color = vec4(0.3, 1.0, 0.5, 1.0);
uniform float effect_enabled : hint_range(0.0, 1.0) = 1.0;

void fragment() {
	vec4 original = texture(screen_tex, SCREEN_UV);
	float intensity = glow_intensity * effect_enabled;

	// 9-tap weighted blur
	vec2 off = SCREEN_PIXEL_SIZE * glow_radius;
	vec3 blur = vec3(0.0);
	blur += texture(screen_tex, SCREEN_UV + vec2(-off.x, -off.y)).rgb * 0.0625;
	blur += texture(screen_tex, SCREEN_UV + vec2(0.0, -off.y)).rgb * 0.125;
	blur += texture(screen_tex, SCREEN_UV + vec2(off.x, -off.y)).rgb * 0.0625;
	blur += texture(screen_tex, SCREEN_UV + vec2(-off.x, 0.0)).rgb * 0.125;
	blur += texture(screen_tex, SCREEN_UV).rgb * 0.25;
	blur += texture(screen_tex, SCREEN_UV + vec2(off.x, 0.0)).rgb * 0.125;
	blur += texture(screen_tex, SCREEN_UV + vec2(-off.x, off.y)).rgb * 0.0625;
	blur += texture(screen_tex, SCREEN_UV + vec2(0.0, off.y)).rgb * 0.125;
	blur += texture(screen_tex, SCREEN_UV + vec2(off.x, off.y)).rgb * 0.0625;

	vec3 glow = blur + glow_color.rgb * intensity * 0.3;
	vec3 result = mix(original.rgb, glow, intensity);

	COLOR = vec4(result, original.a);
}
