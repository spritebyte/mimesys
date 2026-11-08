shader_type canvas_item;
// LCD: a faint grid between the pixels, like a handheld's screen. The grid fades out
// when the window is too small for it. Needs linear filtering.

uniform float strength : hint_range(0.0, 1.0) = 0.6;

void fragment() {
	vec2 tex_size = 1.0 / TEXTURE_PIXEL_SIZE;
	vec2 texel = UV * tex_size;
	vec2 scale = max(1.0 / max(fwidth(texel), vec2(1e-5)), vec2(1.0));
	vec2 d = fract(texel) - 0.5;
	vec2 hold = 0.5 - 0.5 / scale;
	vec2 f = (d - clamp(d, -hold, hold)) * scale + 0.5;
	vec3 c = texture(TEXTURE, (floor(texel) + f) / tex_size).rgb;
	// distance to the cell's edge, in screen pixels; about one screen pixel of gap
	vec2 e = (0.5 - abs(fract(texel) - 0.5)) * scale;
	float grid = smoothstep(0.0, 1.2, min(e.x, e.y));
	float k = strength * smoothstep(2.5, 4.0, min(scale.x, scale.y));
	c *= mix(1.0, mix(0.55, 1.08, grid), k);
	COLOR = vec4(c, 1.0);
}
