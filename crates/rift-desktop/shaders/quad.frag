#version 450
// Rift immediate-mode quad fragment shader.
//
// Draws one of:
//   mode 0 - solid (optionally rounded, optionally bordered) rectangle,
//   mode 1 - textured (optionally rounded) rectangle modulated by tint.
//
// Antialiasing: a signed-distance field for a rounded rectangle, converted to
// ~1-device-pixel coverage. Output is premultiplied linear-alpha; the pipeline
// blends with (One, OneMinusSrcAlpha). The render target is an *_SRGB format so
// the hardware performs the linear -> sRGB encode on write.

layout(set = 1, binding = 0) uniform sampler2D tex;

layout(location = 0) in vec2 v_uv;
layout(location = 1) in vec4 v_color;
layout(location = 2) in vec2 v_local;
layout(location = 3) flat in vec2 v_half;
layout(location = 4) flat in vec2 v_radius_border;
layout(location = 5) flat in vec4 v_border_color;
layout(location = 6) flat in vec2 v_mode;

layout(location = 0) out vec4 out_color;

// Signed distance to a rounded rectangle centered at the origin.
float sd_round_rect(vec2 p, vec2 half_size, float radius) {
    radius = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + vec2(radius);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

// Coverage from a signed distance in device pixels: 1 inside, 0 outside, a
// one-pixel antialiased band on the edge, independent of the DPI scale.
float coverage(float d) {
    float w = max(fwidth(d), 1e-4);
    return clamp(0.5 - d / w, 0.0, 1.0);
}

void main() {
    float radius = max(v_radius_border.x, 0.0);
    float border = max(v_radius_border.y, 0.0);
    bool textured = v_mode.x > 0.5;

    float a_outer = coverage(sd_round_rect(v_local, v_half, radius));

    vec4 premul_fill;
    if (textured) {
        vec4 t = texture(tex, v_uv);
        float a = t.a * v_color.a;
        premul_fill = vec4(t.rgb * v_color.rgb * a, a);
    } else {
        premul_fill = vec4(v_color.rgb * v_color.a, v_color.a);
    }

    vec4 color;
    if (border > 0.0) {
        // Border is stroked *inside* the rect: the interior keeps the fill, the
        // ring between the two SDF outlines takes the border color.
        vec2 inner_half = max(v_half - vec2(border), vec2(0.0));
        float inner_radius = max(radius - border, 0.0);
        float a_inner = coverage(sd_round_rect(v_local, inner_half, inner_radius));
        vec4 premul_border =
            vec4(v_border_color.rgb * v_border_color.a, v_border_color.a);
        color = premul_fill * a_inner + premul_border * (a_outer - a_inner);
    } else {
        color = premul_fill * a_outer;
    }

    if (color.a <= 0.0) {
        discard;
    }
    out_color = color;
}
