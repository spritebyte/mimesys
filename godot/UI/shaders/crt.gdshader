shader_type canvas_item;
// CRT: a slightly curved screen, soft horizontal blur (a TV's limited sharpness),
// scanline beams that bloom on bright colours, an RGB aperture-grille mask and darker
// corners. Needs linear filtering.

uniform float strength : hint_range(0.0, 1.0) = 0.6;   // scanlines + mask
uniform float curvature : hint_range(0.0, 0.25) = 0.04;
// the visible part of the frame texture (x, y, w, h in UV), so the curve fits the picture
uniform vec4 region = vec4(0.0, 0.0, 1.0, 1.0);

vec3 to_linear(vec3 c) { return c * c; }
vec3 to_screen(vec3 c) { return sqrt(max(c, vec3(0.0))); }

void fragment() {
	vec2 tex_size = 1.0 / TEXTURE_PIXEL_SIZE;
	vec2 scale = max(1.0 / max(fwidth(UV * tex_size), vec2(1e-5)), vec2(1.0));
	// bend the picture: 0..1 across the visible area, pushed outwards towards the corners
	vec2 p = (UV - region.xy) / region.zw * 2.0 - 1.0;
	p *= 1.0 + curvature * (p.yx * p.yx);
	vec2 q = p * 0.5 + 0.5;
	if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) {
		COLOR = vec4(0.0, 0.0, 0.0, 1.0);
	} else {
		vec2 uv = region.xy + q * region.zw;
		vec2 texel = uv * tex_size;
		// vertical: sample the line's centre; horizontal: three soft taps
		vec2 at = vec2(texel.x, floor(texel.y) + 0.5) / tex_size;
		vec2 dx = vec2(0.6 / tex_size.x, 0.0);
		vec3 c = to_linear(texture(TEXTURE, at).rgb) * 0.5
			+ to_linear(texture(TEXTURE, at - dx).rgb) * 0.25
			+ to_linear(texture(TEXTURE, at + dx).rgb) * 0.25;
		float luma = dot(c, vec3(0.299, 0.587, 0.114));
		float width = mix(0.24, 0.45, sqrt(luma));
		float y = fract(texel.y) - 0.5;
		float beam = exp(-(y * y) / (2.0 * width * width));
		float k = strength * smoothstep(1.5, 3.0, scale.y);
		c *= mix(1.0, beam * 1.45, k);
		// aperture grille: each screen column favours red, green or blue
		int col = int(mod(FRAGCOORD.x, 3.0));
		vec3 mask = vec3(0.55);
		mask[col] = 1.0;
		c *= mix(vec3(1.0), mask * 1.35, strength * 0.7);
		// darker towards the edges
		vec2 v = q * (1.0 - q);
		c *= pow(clamp(v.x * v.y * 16.0, 0.0, 1.0), 0.18);
		COLOR = vec4(to_screen(c), 1.0);
	}
}
