//! The one graphics pipeline everything is drawn with: a batched, antialiased
//! rounded-rectangle / textured-quad pipeline.
//!
//! Design notes
//! ------------
//! * All geometry for a frame is written into a single dynamic vertex buffer;
//!   each quad is two triangles and carries every per-quad parameter on each
//!   vertex (they are `flat` interpolated in the fragment shader).
//! * Times are in device pixels: the CPU multiplies the UI's logical points by
//!   the window scale factor, so the shader's SDF antialiasing is always ~1
//!   device pixel wide, at any DPI.
//! * The render target is an `*_SRGB` image (swapchain or offscreen), so the
//!   fragment shader emits **linear** premultiplied-alpha colour and the
//!   hardware performs the final linear -> sRGB encode. The blur, blending and
//!   filtering all happen in linear space, which is what makes antialiased text
//!   and rounded corners look right.
//! * Descriptor set 0 = uniform buffer (viewport, one set per frame in flight);
//!   descriptor set 1 = combined image sampler (one per texture, all sharing
//!   one immutable linear-clamp sampler).

use std::sync::Arc;

use anyhow::{Context as _, Result};
use vulkano::{
    buffer::BufferContents,
    descriptor_set::layout::{
        DescriptorSetLayout, DescriptorSetLayoutBinding, DescriptorSetLayoutCreateInfo,
        DescriptorType,
    },
    device::Device,
    format::Format,
    pipeline::{
        graphics::{
            color_blend::{
                AttachmentBlend, BlendFactor, BlendOp, ColorBlendAttachmentState, ColorBlendState,
            },
            input_assembly::InputAssemblyState,
            multisample::MultisampleState,
            rasterization::RasterizationState,
            vertex_input::{Vertex, VertexDefinition},
            viewport::ViewportState,
            GraphicsPipelineCreateInfo,
        },
        layout::PipelineLayoutCreateInfo,
        DynamicState, GraphicsPipeline, PipelineLayout, PipelineShaderStageCreateInfo,
    },
    render_pass::{RenderPass, Subpass},
    shader::{spirv::bytes_to_words, ShaderModule, ShaderModuleCreateInfo, ShaderStages},
};

/// Vertex format of the immediate-mode batcher. 80 bytes per vertex.
///
/// | location | attribute      | meaning |
/// |---|---|---|
/// | 0 | `pos`    | device pixels, top-left origin, +y down |
/// | 1 | `uv`     | normalized texture coordinates |
/// | 2 | `color`  | straight-alpha linear RGB tint/fill |
/// | 3 | `rect`   | `[center_x, center_y, half_w, half_h]` device px (for the SDF) |
/// | 4 | `params` | `[radius, border, mode, 0]`; mode 0 = solid, 1 = textured |
/// | 5 | `border_color` | straight-alpha linear RGB |
#[repr(C)]
#[derive(Clone, Copy, BufferContents, Vertex)]
pub(crate) struct Vertex2D {
    #[format(R32G32_SFLOAT)]
    pub pos: [f32; 2],
    #[format(R32G32_SFLOAT)]
    pub uv: [f32; 2],
    #[format(R32G32B32A32_SFLOAT)]
    pub color: [f32; 4],
    #[format(R32G32B32A32_SFLOAT)]
    pub rect: [f32; 4],
    #[format(R32G32B32A32_SFLOAT)]
    pub params: [f32; 4],
    #[format(R32G32B32A32_SFLOAT)]
    pub border_color: [f32; 4],
}

/// Viewport uniform: physical target size in pixels (std140 pads vec2 to 16 bytes).
#[repr(C)]
#[derive(Clone, Copy, BufferContents)]
pub(crate) struct ViewportUbo {
    pub viewport: [f32; 2],
    pub _padding: [f32; 2],
}

/// Everything created from a render-target format; rebuilt if the swapchain
/// format ever changes (it does not on this driver, but be defensive).
pub(crate) struct PipelineBundle {
    pub render_pass: Arc<RenderPass>,
    pub pipeline: Arc<GraphicsPipeline>,
    pub layout: Arc<PipelineLayout>,
    pub ubo_set_layout: Arc<DescriptorSetLayout>,
    pub texture_set_layout: Arc<DescriptorSetLayout>,
}

/// Creates the single-pass (clear + store) color render pass used for both the
/// swapchain and the offscreen screenshot target.
pub(crate) fn create_render_pass(device: &Arc<Device>, format: Format) -> Result<Arc<RenderPass>> {
    Ok(vulkano::single_pass_renderpass!(
        device.clone(),
        attachments: {
            color: {
                format: format,
                samples: 1,
                load_op: Clear,
                store_op: Store,
            },
        },
        pass: {
            color: [color],
            depth_stencil: {},
        },
    )?)
}

pub(crate) fn create_pipeline(device: &Arc<Device>, format: Format) -> Result<PipelineBundle> {
    let vs_module = load_shader(device, include_bytes!("../../shaders/quad.vert.spv"))?;
    let fs_module = load_shader(device, include_bytes!("../../shaders/quad.frag.spv"))?;
    let vs = vs_module
        .entry_point("main")
        .context("quad.vert.spv has no 'main' entry point")?;
    let fs = fs_module
        .entry_point("main")
        .context("quad.frag.spv has no 'main' entry point")?;

    // Reflection from the raw SPIR-V gives us the vertex input description,
    // exactly as if it had come from vulkano-shaders.
    let vertex_input_state = Vertex2D::per_vertex()
        .definition(&vs)
        .context("building the vertex input state from quad.vert")?;

    // Descriptor set 0: one uniform buffer per frame in flight (not dynamic:
    // the ring itself is what avoids host/GPU write conflicts).
    let ubo_set_layout = DescriptorSetLayout::new(
        device.clone(),
        DescriptorSetLayoutCreateInfo {
            bindings: [(
                0,
                DescriptorSetLayoutBinding {
                    descriptor_count: 1,
                    stages: ShaderStages::VERTEX,
                    ..DescriptorSetLayoutBinding::descriptor_type(DescriptorType::UniformBuffer)
                },
            )]
            .into(),
            ..Default::default()
        },
    )?;
    // Descriptor set 1: a combined image sampler per texture (atlas, cover art).
    let texture_set_layout = DescriptorSetLayout::new(
        device.clone(),
        DescriptorSetLayoutCreateInfo {
            bindings: [(
                0,
                DescriptorSetLayoutBinding {
                    descriptor_count: 1,
                    stages: ShaderStages::FRAGMENT,
                    ..DescriptorSetLayoutBinding::descriptor_type(
                        DescriptorType::CombinedImageSampler,
                    )
                },
            )]
            .into(),
            ..Default::default()
        },
    )?;
    let layout = PipelineLayout::new(
        device.clone(),
        PipelineLayoutCreateInfo {
            set_layouts: vec![ubo_set_layout.clone(), texture_set_layout.clone()],
            ..Default::default()
        },
    )?;

    let render_pass = create_render_pass(device, format)?;
    let subpass = Subpass::from(render_pass.clone(), 0).context("subpass 0 missing")?;

    let stages = [
        PipelineShaderStageCreateInfo::new(vs),
        PipelineShaderStageCreateInfo::new(fs),
    ];

    let pipeline = GraphicsPipeline::new(
        device.clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(vertex_input_state),
            input_assembly_state: Some(InputAssemblyState::default()),
            // The real viewport/scissor are dynamic (set per frame/layer).
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState::default()),
            multisample_state: Some(MultisampleState::default()),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                subpass.num_color_attachments(),
                ColorBlendAttachmentState {
                    // The shader outputs premultiplied linear-alpha colour.
                    blend: Some(AttachmentBlend {
                        src_color_blend_factor: BlendFactor::One,
                        dst_color_blend_factor: BlendFactor::OneMinusSrcAlpha,
                        color_blend_op: BlendOp::Add,
                        src_alpha_blend_factor: BlendFactor::One,
                        dst_alpha_blend_factor: BlendFactor::OneMinusSrcAlpha,
                        alpha_blend_op: BlendOp::Add,
                    }),
                    ..Default::default()
                },
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(subpass.into()),
            ..GraphicsPipelineCreateInfo::layout(layout.clone())
        },
    )
    .context("creating the quad graphics pipeline")?;

    tracing::info!(format = ?format, "quad pipeline ready");

    Ok(PipelineBundle {
        render_pass,
        pipeline,
        layout,
        ubo_set_layout,
        texture_set_layout,
    })
}

fn load_shader(device: &Arc<Device>, bytes: &[u8]) -> Result<Arc<ShaderModule>> {
    let words = bytes_to_words(bytes).map_err(|error| {
        anyhow::anyhow!("embedded SPIR-V is not a multiple of 4 bytes: {error:?}")
    })?;
    // SAFETY: the embedded .spv files are committed build artefacts generated by
    // the documented `glslc` command lines in docs/vulkan-migration/vulkano-recipe.md
    // and checked with spirv-val.
    unsafe { ShaderModule::new(device.clone(), ShaderModuleCreateInfo::new(&words)) }
        .context("creating a shader module from embedded SPIR-V")
}

/// Converts one sRGB-encoded byte to linear light (IEC 61966-2-1).
pub(crate) fn srgb_to_linear(byte: u8) -> f32 {
    let value = byte as f32 / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB straight-alpha colour -> linear straight-alpha RGBA.
pub(crate) fn color_to_linear(color: crate::gfx::paint::Color) -> [f32; 4] {
    [
        srgb_to_linear(color.r),
        srgb_to_linear(color.g),
        srgb_to_linear(color.b),
        color.a as f32 / 255.0,
    ]
}
