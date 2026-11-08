shader_type canvas_item;
// Sharp-smooth: hard pixels at any window size. Only the screen pixels that straddle two
// game pixels are blended, so every game pixel comes out the same width (no uneven
// columns at odd sizes) without the blur of plain smoothing. Needs linear filtering.

void fragment() {
	vec2 tex_size = 1.0 / TEXTURE_PIXEL_SIZE;
	vec2 texel = UV * tex_size;
	// screen pixels per game pixel
	vec2 scale = max(1.0 / max(fwidth(texel), vec2(1e-5)), vec2(1.0));
	vec2 d = fract(texel) - 0.5;
	vec2 hold = 0.5 - 0.5 / scale;
	vec2 f = (d - clamp(d, -hold, hold)) * scale + 0.5;
	COLOR = texture(TEXTURE, (floor(texel) + f) / tex_size);
}
