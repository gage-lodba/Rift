#version 450
// Rift immediate-mode quad vertex shader.
//
// One vertex per corner of a quad. Every quad is drawn as two triangles; all
// per-quad values (rect/radius/border/mode) are replicated on all four vertices
// and marked `flat` downstream, so there is no extra indirection.
//
// IMPORTANT (vulkano 0.35): `VertexDefinition` matches the shader's input
// variables to the `#[derive(Vertex)]` struct members **by name**, not by
// location. Each `layout(location = N) in ...` below must therefore be named
// exactly like the corresponding field of `Vertex2D` in
// app/src/gfx/pipeline.rs (pos, uv, color, rect, params, border_color).
//
// Coordinates are physical device pixels with the origin at the top-left and
// +y pointing down (the CPU already multiplied logical UI coordinates by the
// window scale factor). The viewport uniform converts to Vulkan clip space.

layout(set = 0, binding = 0) uniform ViewportUbo {
    vec2 viewport; // physical pixels
} ubo;

layout(location = 0) in vec2 pos;   // device px, top-left origin
layout(location = 1) in vec2 uv;    // normalized texture coords
layout(location = 2) in vec4 color; // straight-alpha, linear RGB
layout(location = 3) in vec4 rect;  // center.xy, half_size.xy (device px)
layout(location = 4) in vec4 params;        // radius, border, mode, unused
layout(location = 5) in vec4 border_color;  // straight-alpha, linear RGB

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;
layout(location = 2) out vec2 v_local;      // fragment position relative to quad center
layout(location = 3) flat out vec2 v_half;  // half size, device px
layout(location = 4) flat out vec2 v_radius_border;
layout(location = 5) flat out vec4 v_border_color;
layout(location = 6) flat out vec2 v_mode;  // x = 0 solid, 1 textured

void main() {
    // Vulkan clip space has +y pointing DOWN (y = -1 is the top of the
    // framebuffer, +1 the bottom), unlike OpenGL. The UI is y-down too, so the
    // mapping is a straight scale/shift with no flip.
    vec2 ndc = vec2(
        (pos.x / ubo.viewport.x) * 2.0 - 1.0,
        (pos.y / ubo.viewport.y) * 2.0 - 1.0
    );
    gl_Position = vec4(ndc, 0.0, 1.0);

    v_uv = uv;
    v_color = color;
    v_local = pos - rect.xy;
    v_half = rect.zw;
    v_radius_border = params.xy;
    v_border_color = border_color;
    v_mode = params.zz;
}
