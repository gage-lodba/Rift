//! Converts one [`PaintList`] into a flat vertex array plus draw batches.
//!
//! Painter order is preserved by construction: vertices are appended in list
//! order and each clipping layer becomes a scissor change plus one or more
//! draw calls; a draw call only spans a run of consecutive primitives that share
//! the same texture, so switching textures can never reorder anything.
//!
//! All input coordinates are logical pixels; everything is multiplied by the
//! scale factor here, so the GPU always works in device pixels.

use crate::gfx::{
    paint::{Color, PaintList, Prim, Rect, TextureId, NO_TEXTURE},
    pipeline::{color_to_linear, Vertex2D},
};

/// One frame's worth of geometry and batching decisions.
pub(crate) struct FrameBatch {
    pub vertices: Vec<Vertex2D>,
    pub layers: Vec<LayerBatch>,
    /// Attachment clear colour in **linear** light (the target is an sRGB image).
    pub clear_linear: [f32; 4],
}

/// One clipped group of primitives (one scissor rectangle).
pub(crate) struct LayerBatch {
    /// `[x, y, width, height]` in device pixels, already clamped to the target.
    pub scissor: [u32; 4],
    pub draws: Vec<DrawBatch>,
}

/// A run of consecutive primitives sharing one texture (one draw call).
pub(crate) struct DrawBatch {
    pub texture: TextureId,
    pub first_vertex: u32,
    pub vertex_count: u32,
}

impl FrameBatch {
    /// Builds this frame's geometry. Primitives referencing a texture the
    /// renderer has not seen are **kept**: they are drawn with the renderer's
    /// 1x1 white fallback (tinted like any other image), which makes a texture
    /// lifetime bug visible on screen instead of silently dropping content.
    /// The renderer logs each unknown id once, so the root cause stays findable.
    pub fn build(target_width: u32, target_height: u32, scale: f32, paint: &PaintList) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let target = [target_width as f32, target_height as f32];

        let prim_count: usize = paint.layers.iter().map(|layer| layer.prims.len()).sum();
        let mut vertices = Vec::with_capacity(prim_count * 6);
        let mut layers = Vec::with_capacity(paint.layers.len());

        for layer in &paint.layers {
            if layer.prims.is_empty() {
                continue;
            }
            let Some(scissor) = scissor_for(layer.clip, scale, target) else {
                continue; // fully clipped away
            };

            let mut draws: Vec<DrawBatch> = Vec::new();
            for prim in &layer.prims {
                if !layer_contains(layer.clip, prim.bounds(), target, scale) {
                    continue;
                }
                let texture = match prim {
                    Prim::Rect { .. } => NO_TEXTURE,
                    Prim::Image { texture, .. } => *texture,
                };

                let first_vertex = vertices.len() as u32;
                push_prim(&mut vertices, prim, scale);
                let vertex_count = vertices.len() as u32 - first_vertex;

                match draws.last_mut() {
                    Some(draw) if draw.texture == texture => {
                        draw.vertex_count += vertex_count;
                    }
                    _ => draws.push(DrawBatch {
                        texture,
                        first_vertex,
                        vertex_count,
                    }),
                }
            }

            if !draws.is_empty() {
                layers.push(LayerBatch { scissor, draws });
            }
        }

        Self {
            vertices,
            layers,
            clear_linear: clear_to_linear(paint.clear),
        }
    }
}

/// Clear colour: the UI's sRGB bytes converted to linear light. Vulkan treats
/// floating-point clear values for an `*_SRGB` attachment as linear and encodes
/// them on store (verified on the probe: 0.02 -> byte 40, not byte 5).
fn clear_to_linear(color: Color) -> [f32; 4] {
    let linear = color_to_linear(color);
    // The attachment is opaque; keep alpha at 1 so present/composition is trivial
    // even if the compositor honours alpha.
    [linear[0], linear[1], linear[2], 1.0]
}

/// The scissor for a layer: `None` clip = whole target, `Some` = rounded device
/// rect; returns `None` when the clip has no visible area.
fn scissor_for(clip: Option<Rect>, scale: f32, target: [f32; 2]) -> Option<[u32; 4]> {
    let Some(clip) = clip else {
        return Some([0, 0, target[0] as u32, target[1] as u32]);
    };
    if clip.is_empty() {
        return None;
    }

    // Scissors cover whole pixels: round outwards so antialiased edges at the
    // clip boundary are not cut in half.
    let x0 = (clip.x * scale).floor().clamp(0.0, target[0]);
    let y0 = (clip.y * scale).floor().clamp(0.0, target[1]);
    let x1 = (clip.right() * scale).ceil().clamp(0.0, target[0]);
    let y1 = (clip.bottom() * scale).ceil().clamp(0.0, target[1]);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

/// Cheap CPU culling: skip primitives that fall entirely outside the layer clip.
fn layer_contains(clip: Option<Rect>, bounds: Rect, target: [f32; 2], scale: f32) -> bool {
    let viewport = Rect::new(0.0, 0.0, target[0] / scale, target[1] / scale);
    let area = match clip {
        Some(clip) => clip.intersect(viewport),
        None => viewport,
    };
    !bounds.intersect(area).is_empty()
}

/// Appends the six vertices (two triangles) of one primitive.
fn push_prim(out: &mut Vec<Vertex2D>, prim: &Prim, scale: f32) {
    match *prim {
        Prim::Rect {
            rect,
            color,
            radius,
            border,
            border_color,
        } => {
            let fill = color_to_linear(color);
            let stroke = color_to_linear(border_color);
            push_quad(
                out,
                rect,
                scale,
                [0.0, 0.0, 0.0, 0.0],
                fill,
                [radius.max(0.0) * scale, border.max(0.0) * scale, 0.0, 0.0],
                stroke,
            );
        }
        Prim::Image {
            rect,
            texture: _,
            uv,
            tint,
            radius,
        } => {
            let tint = color_to_linear(tint);
            push_quad(
                out,
                rect,
                scale,
                [uv.x, uv.y, uv.x + uv.w, uv.y + uv.h],
                tint,
                [radius.max(0.0) * scale, 0.0, 1.0, 0.0],
                [0.0; 4],
            );
        }
    }
}

/// Emits a quad with per-corner UVs; `rect` is logical px.
fn push_quad(
    out: &mut Vec<Vertex2D>,
    rect: Rect,
    scale: f32,
    uv: [f32; 4], // [u0, v0, u1, v1]; ignored (all zero) for solid prims
    color: [f32; 4],
    params: [f32; 4],
    border_color: [f32; 4],
) {
    let x0 = rect.x * scale;
    let y0 = rect.y * scale;
    let x1 = (rect.x + rect.w) * scale;
    let y1 = (rect.y + rect.h) * scale;

    let center = [(x0 + x1) * 0.5, (y0 + y1) * 0.5];
    let half = [(x1 - x0).abs() * 0.5, (y1 - y0).abs() * 0.5];
    let rect_params = [center[0], center[1], half[0], half[1]];

    let [u0, v0, u1, v1] = uv;
    let corners = [
        ([x0, y0], [u0, v0]),
        ([x1, y0], [u1, v0]),
        ([x0, y1], [u0, v1]),
        ([x1, y0], [u1, v0]),
        ([x1, y1], [u1, v1]),
        ([x0, y1], [u0, v1]),
    ];
    for (pos, uv) in corners {
        out.push(Vertex2D {
            pos,
            uv,
            color,
            rect: rect_params,
            params,
            border_color,
        });
    }
}
