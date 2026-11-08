shader_type canvas_item;
// Scanlines: each game line drawn as a soft beam with a dark gap below it, like a TV.
// Bright lines are drawn a little wider (as a real beam blooms). The effect fades out
// when the window is too small to show it cleanly. Needs linear filtering.

uniform float strength : hint_range(0.0, 1.0) = 0.6;

void fragment() {
	vec2 tex_size = 1.0 / TEXTURE_PIXEL_SIZE;
	vec2 texel = UV * tex_size;
	vec2 scale = max(1.0 / max(fwidth(texel), vec2(1e-5)), vec2(1.0));
	vec2 d = fract(texel) - 0.5;
	vec2 hold = 0.5 - 0.5 / scale;
	vec2 f = (d - clamp(d, -hold, hold)) * scale + 0.5;
	vec3 c = texture(TEXTURE, (floor(texel) + f) / tex_size).rgb;
	float luma = dot(c, vec3(0.299, 0.587, 0.114));
	float width = mix(0.28, 0.42, luma);
	float y = fract(texel.y) - 0.5;
	float beam = exp(-(y * y) / (2.0 * width * width));
	float k = strength * smoothstep(1.5, 3.0, scale.y);
	c *= mix(1.0, beam * 1.3, k);
	COLOR = vec4(c, 1.0);
}
