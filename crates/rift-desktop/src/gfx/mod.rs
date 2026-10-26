//! The Vulkan renderer: everything between a [`PaintList`] and the swapchain.
//!
//! Architecture
//! ------------
//! `Renderer` owns the device-wide Vulkan state for one output target:
//!
//! * **Windowed** (`Renderer::new`) — instance/device with `khr_swapchain`, a
//!   `Surface` for the winit window, a FIFO swapchain, one framebuffer per
//!   swapchain image, and a present loop. Swaps are recreated lazily on resize.
//! * **Headless** (`Renderer::new_headless` / [`screenshot`]) — no window or
//!   swapchain: one offscreen `R8G8B8A8_SRGB` image is rendered and read back
//!   into a PNG. This is the automated visual-verification path used by
//!   `rift-music --screenshot out.png --size 1160x740`.
//!
//! One frame is: process the paint list's texture updates, convert all
//! primitives to one vertex buffer, record one command buffer (render pass +
//! scissor changes + one draw per texture run), submit, present. There is a ring
//! of [`FRAMES_IN_FLIGHT`] uniform buffers and vertex buffers so the CPU never
//! rewrites a buffer the GPU may still be reading (vulkano rejects that with
//! "resource is already in use in a conflicting way").
//!
//! Colour management decision
//! --------------------------
//! The UI emits **sRGB-encoded, straight-alpha** bytes (`PaintList`). The render
//! target is always an `*_SRGB` format — `B8G8R8A8_SRGB`/`R8G8B8A8_SRGB` for the
//! swapchain, `R8G8B8A8_SRGB` offscreen. Therefore:
//! * the CPU converts colours to **linear** light before putting them in
//!   vertices / the clear value (except alpha, which is linear already);
//! * images (font atlas, cover art) are uploaded as sRGB RGBA8, so the sampler
//!   decodes them to linear;
//! * the fragment shader outputs **premultiplied linear** colour and the fixed-
//!   function blend is `(One, OneMinusSrcAlpha)`;
//! * the hardware performs the linear -> sRGB encode when storing the pixel.
//!
//! If this machine ever reports no `*_SRGB` surface format, the code falls back
//! to a UNORM target — in that case the framebuffer would be blended/encoded
//! incorrectly and the fragment shader would need a manual sRGB encode. The
//! fallback is logged as a warning; it does not happen on the reference machine.
//!
//! Coordinate space
//! ----------------
//! `PaintList` is in logical points (top-left origin, +y down). The renderer
//! multiplies by its scale factor when writing vertices, so the SDF
//! antialiasing is one **device** pixel wide at any DPI; the ortho projection
//! (a viewport uniform) maps device pixels to Vulkan clip space.

pub mod paint;

mod batch;
mod gpu;
mod pipeline;
#[cfg(test)]
mod tests;
mod texture;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, CopyImageToBufferInfo,
        PrimaryCommandBufferAbstract, RenderPassBeginInfo, SubpassBeginInfo, SubpassContents,
    },
    descriptor_set::{DescriptorSet, WriteDescriptorSet},
    format::Format,
    image::{view::ImageView, Image, ImageCreateInfo, ImageUsage},
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter},
    pipeline::graphics::viewport::{Scissor, Viewport},
    pipeline::PipelineBindPoint,
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
    swapchain::{
        acquire_next_image, ColorSpace, CompositeAlpha, PresentMode, Surface, SurfaceCapabilities,
        Swapchain, SwapchainCreateInfo, SwapchainPresentInfo,
    },
    sync::{self, GpuFuture},
    Validated, VulkanError,
};
use winit::window::Window;

use self::{
    batch::FrameBatch,
    gpu::GfxContext,
    paint::{Prim, NO_TEXTURE},
    pipeline::{create_pipeline, PipelineBundle, Vertex2D, ViewportUbo},
    texture::TextureManager,
};

pub use paint::PaintList;

/// Uniform buffers + vertex buffers are ring-buffered so that at most one frame
/// in flight can reference each slot. 3 is the standard choice: acquire blocks
/// at FIFO, so the CPU is never more than a frame ahead of the GPU.
const FRAMES_IN_FLIGHT: usize = 3;

/// Initial per-frame vertex capacity (80 bytes each); grows on demand.
const INITIAL_VERTEX_CAPACITY: usize = 16 * 1024;

/// How many frames a retired buffer is kept alive before it is dropped.
const RETIRE_FRAMES: u64 = 4;

/// Number of frames averaged for one `RIFT_GFX_STATS` report line.
const STATS_WINDOW: usize = 300;

/// Optional per-frame timings, enabled with `RIFT_GFX_STATS=1`.
///
/// Every [`STATS_WINDOW`] presented frames the renderer logs one line with
/// average/p95/max frame time (which, under FIFO present, includes the vsync
/// wait), the acquire wait, vertex/draw counts and live texture count. Used for
/// acceptance/performance evidence without changing the app's hot path when the
/// environment variable is unset.
#[derive(Default)]
struct FrameStats {
    enabled: bool,
    samples_ms: Vec<f32>,
    acquire_ms: Vec<f32>,
    vertices: Vec<u32>,
    draws: Vec<u32>,
}

/// One frame's counters, fed to [`FrameStats::record`].
struct FrameSample {
    total: std::time::Duration,
    acquire: std::time::Duration,
    vertices: usize,
    draws: usize,
    textures: usize,
    created_total: u64,
    dropped_total: u64,
    updated_total: u64,
}

impl FrameStats {
    fn new() -> Self {
        Self {
            enabled: std::env::var("RIFT_GFX_STATS").is_ok_and(|v| v == "1"),
            ..Default::default()
        }
    }

    fn record(&mut self, sample: FrameSample) {
        if !self.enabled {
            return;
        }
        self.samples_ms.push(sample.total.as_secs_f32() * 1000.0);
        self.acquire_ms.push(sample.acquire.as_secs_f32() * 1000.0);
        self.vertices.push(sample.vertices as u32);
        self.draws.push(sample.draws as u32);

        if self.samples_ms.len() < STATS_WINDOW {
            return;
        }

        fn percentile(values: &mut [f32], fraction: f32) -> f32 {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let index = ((values.len() - 1) as f32 * fraction).round() as usize;
            values[index]
        }

        let count = self.samples_ms.len();
        let avg = self.samples_ms.iter().sum::<f32>() / count as f32;
        let mut sorted = self.samples_ms.clone();
        let p95 = percentile(&mut sorted, 0.95);
        let max = *sorted.last().unwrap_or(&0.0);
        let avg_acquire = self.acquire_ms.iter().sum::<f32>() / count as f32;
        let avg_vertices = self.vertices.iter().sum::<u32>() as f32 / count as f32;
        let avg_draws = self.draws.iter().sum::<u32>() as f32 / count as f32;
        tracing::info!(
            window = count,
            avg_ms = %format!("{avg:.2}"),
            p95_ms = %format!("{p95:.2}"),
            max_ms = %format!("{max:.2}"),
            fps = %format!("{:.1}", 1000.0 / avg.max(0.001)),
            acquire_avg_ms = %format!("{avg_acquire:.2}"),
            avg_vertices = %format!("{avg_vertices:.0}"),
            avg_draws = %format!("{avg_draws:.0}"),
            textures = sample.textures,
            created_total = sample.created_total,
            dropped_total = sample.dropped_total,
            updated_total = sample.updated_total,
            "gfx frame stats"
        );
        self.samples_ms.clear();
        self.acquire_ms.clear();
        self.vertices.clear();
        self.draws.clear();
    }
}

/// A Vulkano-rendered window (or a headless offscreen target for screenshots).
pub struct Renderer {
    ctx: GfxContext,
    target: Target,
    format: Format,
    pipeline: PipelineBundle,
    textures: TextureManager,
    /// Per-frame-in-flight viewport uniforms + their descriptor sets.
    uniforms: Vec<(Subbuffer<ViewportUbo>, Arc<DescriptorSet>)>,
    /// Per-frame-in-flight dynamic vertex buffers.
    vertex_buffers: Vec<Subbuffer<[Vertex2D]>>,
    vertex_capacity: usize,
    /// Buffers/descriptors replaced by a growth or format change, kept alive
    /// until the GPU has provably finished with them.
    retired_uniforms: Vec<(u64, Subbuffer<ViewportUbo>, Arc<DescriptorSet>)>,
    retired_vertex_buffers: Vec<(u64, Subbuffer<[Vertex2D]>)>,
    previous_frame_end: Option<Box<dyn GpuFuture>>,
    frame_index: u64,
    width: u32,
    height: u32,
    scale_factor: f32,
    recreate_swapchain: bool,
    /// Texture ids referenced by a `PaintList` that the renderer has never seen;
    /// each one is logged once (at warn level under `RIFT_VK_VALIDATION`) and
    /// drawn with the white fallback, so a texture-lifetime bug is visible.
    missing_textures: HashSet<u64>,
    stats: FrameStats,
}

enum Target {
    Window {
        window: Arc<Window>,
        /// Kept alive so the surface outlives the swapchain images; never read.
        _surface: Arc<Surface>,
        swapchain: Arc<Swapchain>,
        framebuffers: Vec<Arc<Framebuffer>>,
    },
    Offscreen {
        image: Arc<Image>,
        framebuffer: Arc<Framebuffer>,
    },
}

impl Renderer {
    /// Create a renderer that presents to `window`'s surface.
    ///
    /// # Errors
    ///
    /// Returns an error when no Vulkan instance/device with graphics and
    /// presentation support for the window is available, or when creating the
    /// surface, swapchain, pipeline or any per-frame resource fails.
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let scale_factor = window.scale_factor() as f32;

        let startup = std::time::Instant::now();
        let ctx = GfxContext::for_window(&*window)
            .context("initialising the Vulkan renderer for the window")?;
        tracing::debug!(
            "startup: Vulkan instance/device {:.1} ms",
            startup.elapsed().as_secs_f64() * 1000.0
        );
        let stage = std::time::Instant::now();
        let surface = Surface::from_window(ctx.instance.clone(), window.clone())
            .context("creating a Vulkan surface for the window")?;
        let format = pick_surface_format(&ctx, &surface)?;
        let pipeline = create_pipeline(&ctx.device, format)?;
        tracing::debug!(
            "startup: Vulkan surface/format/pipeline {:.1} ms",
            stage.elapsed().as_secs_f64() * 1000.0
        );
        let stage = std::time::Instant::now();
        let (swapchain, framebuffers, extent) = create_swapchain_surface(
            &ctx,
            &surface,
            [width, height],
            format,
            &pipeline.render_pass,
        )?;
        tracing::debug!(
            "startup: Vulkan swapchain {:.1} ms",
            stage.elapsed().as_secs_f64() * 1000.0
        );
        // The swapchain extent is authoritative: the surface may have clamped it
        // (fixed-size surface such as Wayland, or a driver min/max bound), and
        // the viewport uniform must match the images that are actually drawn to.
        let width = extent[0];
        let height = extent[1];
        let stage = std::time::Instant::now();
        let textures = TextureManager::new(&ctx, pipeline.texture_set_layout.clone())?;
        tracing::debug!(
            "startup: Vulkan texture manager {:.1} ms",
            stage.elapsed().as_secs_f64() * 1000.0
        );
        let stage = std::time::Instant::now();
        let uniforms = create_uniform_ring(&ctx, &pipeline)?;
        let vertex_buffers = create_vertex_ring(&ctx, INITIAL_VERTEX_CAPACITY)?;
        tracing::debug!(
            "startup: Vulkan ring buffers {:.1} ms",
            stage.elapsed().as_secs_f64() * 1000.0
        );

        Ok(Self {
            ctx,
            target: Target::Window {
                window,
                _surface: surface,
                swapchain,
                framebuffers,
            },
            format,
            pipeline,
            textures,
            uniforms,
            vertex_buffers,
            vertex_capacity: INITIAL_VERTEX_CAPACITY,
            retired_uniforms: Vec::new(),
            retired_vertex_buffers: Vec::new(),
            previous_frame_end: None,
            frame_index: 0,
            width,
            height,
            scale_factor,
            recreate_swapchain: false,
            missing_textures: HashSet::new(),
            stats: FrameStats::new(),
        })
    }

    /// Create a renderer that draws into an offscreen image instead of a
    /// window; used by `--screenshot` and tests.
    ///
    /// # Errors
    ///
    /// Returns an error when no Vulkan device with a graphics queue is
    /// available, or when creating the device, pipeline or offscreen target
    /// fails.
    pub fn new_headless(width: u32, height: u32, scale_factor: f32) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };

        let startup = std::time::Instant::now();
        let ctx = GfxContext::headless().context("initialising the headless Vulkan renderer")?;
        tracing::debug!(
            "startup: headless Vulkan instance/device {:.1} ms",
            startup.elapsed().as_secs_f64() * 1000.0
        );
        let stage = std::time::Instant::now();
        let format = Format::R8G8B8A8_SRGB;
        let pipeline = create_pipeline(&ctx.device, format)?;
        let (image, framebuffer) =
            create_offscreen_target(&ctx, &pipeline.render_pass, [width, height])?;
        let textures = TextureManager::new(&ctx, pipeline.texture_set_layout.clone())?;
        let uniforms = create_uniform_ring(&ctx, &pipeline)?;
        let vertex_buffers = create_vertex_ring(&ctx, INITIAL_VERTEX_CAPACITY)?;
        tracing::debug!(
            "startup: headless pipeline/target/resources {:.1} ms",
            stage.elapsed().as_secs_f64() * 1000.0
        );

        Ok(Self {
            ctx,
            target: Target::Offscreen { image, framebuffer },
            format,
            pipeline,
            textures,
            uniforms,
            vertex_buffers,
            vertex_capacity: INITIAL_VERTEX_CAPACITY,
            retired_uniforms: Vec::new(),
            retired_vertex_buffers: Vec::new(),
            previous_frame_end: None,
            frame_index: 0,
            width,
            height,
            scale_factor,
            recreate_swapchain: false,
            missing_textures: HashSet::new(),
            stats: FrameStats::new(),
        })
    }

    /// Physical pixel size of the target.
    pub fn window_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Layout size in logical pixels: `window_size / scale_factor`.
    pub fn logical_size(&self) -> (f32, f32) {
        (
            self.width as f32 / self.scale_factor,
            self.height as f32 / self.scale_factor,
        )
    }

    /// Physical pixels per logical pixel, as last reported by the window (or the
    /// value passed to [`Renderer::new_headless`]).
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Resize the output target. For a window this recreates the swapchain;
    /// for a headless renderer it recreates the offscreen image. Either way the
    /// actual recreation happens lazily at the start of the next frame render,
    /// which is where errors can be reported cleanly (and where the event loop
    /// is not mid-dispatch).
    pub fn resize(&mut self, physical: (u32, u32)) {
        let (width, height) = (physical.0.max(1), physical.1.max(1));
        if (width, height) == (self.width, self.height) && !self.recreate_swapchain {
            return;
        }
        self.width = width;
        self.height = height;
        self.recreate_swapchain = true;
    }

    /// Upload any textures the paint list created, draw it, present.
    ///
    /// # Errors
    ///
    /// Returns an error if a texture upload, command-buffer recording or
    /// submission fails. Transient swapchain conditions (resize/`OutOfDate`)
    /// are not errors: the swapchain is recreated and the frame retried
    /// implicitly on the next call.
    pub fn frame(&mut self, paint: &PaintList) -> Result<()> {
        match self.target {
            Target::Window { .. } => self.frame_window(paint),
            Target::Offscreen { .. } => {
                // The public API's frame() is for the window; for a headless
                // renderer render one frame into the offscreen image (used by
                // tests) and discard the pixels.
                let _ = self.render_offscreen(paint)?;
                Ok(())
            }
        }
    }

    /// Wait for all in-flight GPU work. Safe to call more than once; `main`
    /// calls this on shutdown before the window and renderer are dropped.
    pub fn shutdown(&mut self) {
        self.wait_idle();
    }

    /// Block until the GPU has finished all submitted work.
    pub fn wait_idle(&mut self) {
        if let Some(mut future) = self.previous_frame_end.take() {
            future.cleanup_finished();
        }
        let _ = self.ctx.queue.with(|mut queue| queue.wait_idle());
    }

    /// Reports `PaintList` prims that reference textures the renderer has never
    /// seen. Each id is logged once, and the primitive is still drawn with the
    /// 1x1 white fallback (tinted), so a texture-lifetime regression shows up on
    /// screen instead of leaving a silent blank tile.
    fn note_missing_textures(&mut self, paint: &PaintList) {
        for layer in &paint.layers {
            for prim in &layer.prims {
                let Prim::Image { texture, .. } = *prim else {
                    continue;
                };
                if texture == NO_TEXTURE || self.textures.contains(texture) {
                    continue;
                }
                if self.missing_textures.insert(texture) {
                    if gpu::validation_enabled() {
                        tracing::warn!(
                            texture,
                            "PaintList references a texture the renderer has never seen; \
                             drawing the white fallback (likely a texture lifetime bug)"
                        );
                    } else {
                        tracing::debug!(
                            texture,
                            "PaintList references an unknown texture; using the white fallback"
                        );
                    }
                }
            }
        }
    }

    // -- windowed frame ----------------------------------------------------

    fn frame_window(&mut self, paint: &PaintList) -> Result<()> {
        let frame_started = std::time::Instant::now();
        let Target::Window { window, .. } = &self.target else {
            return Ok(());
        };

        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            // Minimised: nothing to draw, and a zero-sized swapchain is invalid.
            return Ok(());
        }
        // Follow DPI changes (window dragged to a monitor with different
        // scaling) without needing the caller to notify us.
        let scale_factor = window.scale_factor() as f32;
        if (scale_factor - self.scale_factor).abs() > f32::EPSILON {
            tracing::info!(
                old = self.scale_factor,
                new = scale_factor,
                "window scale factor changed"
            );
            self.scale_factor = scale_factor;
        }
        if (size.width, size.height) != (self.width, self.height) {
            self.width = size.width;
            self.height = size.height;
            self.recreate_swapchain = true;
        }
        if self.recreate_swapchain {
            self.recreate_swapchain = false;
            if let Err(error) = self.recreate_window_swapchain() {
                // A failed recreation (e.g. window is being moved between
                // monitors) recovers on the next frame.
                tracing::warn!("swapchain recreation failed (will retry): {error:#}");
                self.recreate_swapchain = true;
                return Ok(());
            }
        }

        self.drain_retired();
        if let Some(future) = self.previous_frame_end.as_mut() {
            future.cleanup_finished();
        }

        self.textures.sync(paint)?;
        self.note_missing_textures(paint);

        let batch = FrameBatch::build(self.width, self.height, self.scale_factor, paint);

        let slot = self.prepare_slot(&batch)?;

        let Target::Window {
            window,
            swapchain,
            framebuffers,
            ..
        } = &self.target
        else {
            return Ok(());
        };

        let acquire_started = std::time::Instant::now();
        let (image_index, suboptimal, acquire_future) =
            match acquire_next_image(swapchain.clone(), None).map_err(Validated::unwrap) {
                Ok(result) => result,
                // Both mean the swapchain no longer matches the surface: drop the
                // frame and rebuild the swapchain on the next one. (A surface can
                // be lost when a monitor/window changes in a way the driver
                // reports rather than as OutOfDate.)
                Err(VulkanError::OutOfDate) => {
                    self.recreate_swapchain = true;
                    return Ok(());
                }
                Err(VulkanError::SurfaceLost) => {
                    tracing::warn!("swapchain surface lost; recreating it");
                    self.recreate_swapchain = true;
                    return Ok(());
                }
                // Not recoverable without recreating the device; report it instead
                // of pretending the next frame will work.
                Err(VulkanError::DeviceLost) => {
                    return Err(anyhow!(
                        "Vulkan device lost while acquiring a swapchain image"
                    ));
                }
                Err(error) => return Err(anyhow!("acquiring a swapchain image: {error}")),
            };
        let acquire_elapsed = acquire_started.elapsed();
        if suboptimal {
            self.recreate_swapchain = true;
        }

        let framebuffer = framebuffers
            .get(image_index as usize)
            .ok_or_else(|| anyhow!("swapchain image index {image_index} out of range"))?
            .clone();

        let mut builder = AutoCommandBufferBuilder::primary(
            self.ctx.command_buffer_allocator.clone(),
            self.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("recording the frame command buffer")?;
        self.record_draws(
            &mut builder,
            &framebuffer,
            &batch,
            [self.width, self.height],
            slot,
        )?;
        let command_buffer = builder
            .build()
            .context("building the frame command buffer")?;

        // winit 0.30's frame-pacing contract: notify the windowing system that
        // a buffer is about to be submitted, immediately before present. On
        // Wayland this schedules the frame callback that throttles
        // `RedrawRequested` to the compositor's refresh (other platforms: this
        // is a no-op), so the present rate stays in step with the compositor
        // instead of stuttering.
        window.pre_present_notify();

        let future = self
            .previous_frame_end
            .take()
            .unwrap_or_else(|| sync::now(self.ctx.device.clone()).boxed())
            .join(acquire_future)
            .then_execute(self.ctx.queue.clone(), command_buffer)
            .context("submitting the frame")?
            .then_swapchain_present(
                self.ctx.queue.clone(),
                SwapchainPresentInfo::swapchain_image_index(swapchain.clone(), image_index),
            )
            .then_signal_fence_and_flush();

        match future.map_err(Validated::unwrap) {
            Ok(future) => self.previous_frame_end = Some(future.boxed()),
            Err(VulkanError::OutOfDate) => {
                // The swapchain became invalid between acquire and present; the
                // frame is simply dropped and recreated next frame.
                tracing::debug!("present returned OutOfDate; recreating the swapchain");
                self.previous_frame_end = None;
                self.recreate_swapchain = true;
            }
            Err(VulkanError::SurfaceLost) => {
                tracing::warn!("present returned SurfaceLost; recreating the swapchain");
                self.previous_frame_end = None;
                self.recreate_swapchain = true;
            }
            Err(VulkanError::DeviceLost) => {
                return Err(anyhow!("Vulkan device lost while presenting a frame"));
            }
            Err(error) => return Err(anyhow!("presenting a frame: {error}")),
        }

        self.frame_index = self.frame_index.wrapping_add(1);
        let vertices = batch.vertices.len();
        let draws = batch
            .layers
            .iter()
            .map(|layer| layer.draws.len())
            .sum::<usize>();
        let textures = self.textures.len();
        let created_total = self.textures.created_total();
        let dropped_total = self.textures.dropped_total();
        let updated_total = self.textures.updated_total();
        self.stats.record(FrameSample {
            total: frame_started.elapsed(),
            acquire: acquire_elapsed,
            vertices,
            draws,
            textures,
            created_total,
            dropped_total,
            updated_total,
        });
        Ok(())
    }

    fn recreate_window_swapchain(&mut self) -> Result<()> {
        let Target::Window {
            window,
            _surface: surface,
            swapchain,
            framebuffers,
        } = &mut self.target
        else {
            return Ok(());
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        // Re-query the capabilities: moving between monitors (or the compositor
        // itself) can change the fixed/current extent and the min/max bounds, and
        // an out-of-range image extent fails swapchain creation.
        let caps = self
            .ctx
            .physical_device
            .surface_capabilities(surface, Default::default())
            .context("querying surface capabilities while recreating the swapchain")?;
        let extent = clamp_swapchain_extent(&caps, [size.width, size.height]);

        let (new_swapchain, new_images) = swapchain
            .recreate(SwapchainCreateInfo {
                image_extent: extent,
                ..swapchain.create_info()
            })
            .context("recreating the swapchain after a resize")?;

        if new_swapchain.image_format() != self.format {
            // Extremely rare (moving to a different surface); rebuild everything
            // that is format-dependent.
            tracing::warn!(
                old = ?self.format,
                new = ?new_swapchain.image_format(),
                "swapchain format changed; rebuilding the pipeline"
            );
            self.format = new_swapchain.image_format();
            self.pipeline = create_pipeline(&self.ctx.device, self.format)?;
            self.uniforms = create_uniform_ring(&self.ctx, &self.pipeline)?;
            // The texture descriptor sets were allocated against the old
            // pipeline's set layout; without reallocating them every texture
            // bind below would be incompatible with the new pipeline layout.
            self.textures
                .rebuild_descriptor_sets(self.pipeline.texture_set_layout.clone())?;
        }

        *framebuffers = create_framebuffers(&self.pipeline.render_pass, &new_images)?;
        *swapchain = new_swapchain;
        self.width = extent[0];
        self.height = extent[1];
        Ok(())
    }

    /// Reallocate the headless render target for the current
    /// [`Renderer::width`]/[`Renderer::height`]. Called from the render path so
    /// that a failed allocation is reported through `Result` (unlike
    /// [`Renderer::resize`], which only records the new size).
    fn recreate_offscreen_target(&mut self) -> Result<()> {
        if !matches!(self.target, Target::Offscreen { .. }) {
            return Ok(());
        }
        let (image, framebuffer) = create_offscreen_target(
            &self.ctx,
            &self.pipeline.render_pass,
            [self.width, self.height],
        )
        .context("recreating the offscreen render target after a resize")?;
        self.target = Target::Offscreen { image, framebuffer };
        Ok(())
    }

    // -- shared frame preparation -----------------------------------------

    /// Writes this frame's vertices + viewport uniform into `slot`, growing the
    /// ring buffers when the frame needs more vertices.
    fn prepare_slot(&mut self, batch: &FrameBatch) -> Result<usize> {
        let slot = self.frame_index as usize % FRAMES_IN_FLIGHT;

        if batch.vertices.len() > self.vertex_capacity {
            let new_capacity = batch
                .vertices
                .len()
                .next_power_of_two()
                .max(INITIAL_VERTEX_CAPACITY);
            self.grow_vertex_buffers(new_capacity)?;
        }

        // Ring-buffer writes can only fail if the GPU is still reading that
        // slot; wait for the previous frame and retry once.
        let write_vertices = |renderer: &mut Self| -> Result<()> {
            let buffer = renderer.vertex_buffers[slot].clone();
            let write = match buffer.write() {
                Ok(mut guard) => {
                    guard[..batch.vertices.len()].copy_from_slice(&batch.vertices);
                    Ok(())
                }
                Err(error) => Err(anyhow!("{error}")),
            };
            write
        };
        if let Err(first_error) = write_vertices(self) {
            self.wait_idle();
            write_vertices(self)
                .with_context(|| format!("writing the vertex buffer slot ({first_error})"))?;
        }

        let write_uniform = |renderer: &mut Self| -> Result<()> {
            let buffer = renderer.uniforms[slot].0.clone();
            let write = match buffer.write() {
                Ok(mut ubo) => {
                    ubo.viewport = [renderer.width as f32, renderer.height as f32];
                    Ok(())
                }
                Err(error) => Err(anyhow!("{error}")),
            };
            write
        };
        if let Err(first_error) = write_uniform(self) {
            self.wait_idle();
            write_uniform(self)
                .with_context(|| format!("writing the viewport uniform slot ({first_error})"))?;
        }

        Ok(slot)
    }

    fn grow_vertex_buffers(&mut self, new_capacity: usize) -> Result<()> {
        let new_buffers = create_vertex_ring(&self.ctx, new_capacity)?;
        let old = std::mem::replace(&mut self.vertex_buffers, new_buffers);
        let retired_at = self.frame_index;
        for buffer in old {
            self.retired_vertex_buffers.push((retired_at, buffer));
        }
        self.vertex_capacity = new_capacity;
        tracing::debug!(new_capacity, "grew the frame vertex ring");
        Ok(())
    }

    fn drain_retired(&mut self) {
        let frame = self.frame_index;
        self.retired_vertex_buffers
            .retain(|(retired_at, _)| frame.saturating_sub(*retired_at) < RETIRE_FRAMES);
        self.retired_uniforms
            .retain(|(retired_at, _, _)| frame.saturating_sub(*retired_at) < RETIRE_FRAMES);
    }

    /// Records the render pass and all draw calls for one frame.
    fn record_draws(
        &self,
        builder: &mut AutoCommandBufferBuilder<vulkano::command_buffer::PrimaryAutoCommandBuffer>,
        framebuffer: &Arc<Framebuffer>,
        batch: &FrameBatch,
        target: [u32; 2],
        slot: usize,
    ) -> Result<()> {
        builder
            .begin_render_pass(
                RenderPassBeginInfo {
                    clear_values: vec![Some(batch.clear_linear.into())],
                    ..RenderPassBeginInfo::framebuffer(framebuffer.clone())
                },
                SubpassBeginInfo {
                    contents: SubpassContents::Inline,
                    ..Default::default()
                },
            )
            .context("beginning the render pass")?;

        let viewport = Viewport {
            offset: [0.0, 0.0],
            extent: [target[0] as f32, target[1] as f32],
            depth_range: 0.0..=1.0,
        };
        builder
            .set_viewport(0, [viewport].into_iter().collect())
            .context("setting the viewport")?;
        let scissor = Scissor {
            offset: [0, 0],
            extent: target,
        };
        builder
            .set_scissor(0, [scissor].into_iter().collect())
            .context("setting the scissor")?;

        if batch.vertices.is_empty() || batch.layers.is_empty() {
            builder
                .end_render_pass(Default::default())
                .context("ending the render pass")?;
            return Ok(());
        }

        builder
            .bind_pipeline_graphics(self.pipeline.pipeline.clone())
            .context("binding the quad pipeline")?;
        builder
            .bind_vertex_buffers(0, self.vertex_buffers[slot].clone())
            .context("binding the frame vertex buffer")?;
        builder
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                self.pipeline.layout.clone(),
                0,
                self.uniforms[slot].1.clone(),
            )
            .context("binding the viewport uniform")?;

        for layer in &batch.layers {
            builder
                .set_scissor(
                    0,
                    [Scissor {
                        offset: [layer.scissor[0], layer.scissor[1]],
                        extent: [layer.scissor[2], layer.scissor[3]],
                    }]
                    .into_iter()
                    .collect(),
                )
                .context("setting a clip-layer scissor")?;

            for draw in &layer.draws {
                builder
                    .bind_descriptor_sets(
                        PipelineBindPoint::Graphics,
                        self.pipeline.layout.clone(),
                        1,
                        self.textures.descriptor_set(draw.texture),
                    )
                    .context("binding a texture descriptor set")?;
                // SAFETY: the shader safety requirements are satisfied because
                // everything is validated at binding/creation time and nothing
                // is indexed dynamically:
                // * the bound pipeline is our single quad pipeline, and the
                //   vertex input state was built from that vertex shader's
                //   reflection, so it matches `Vertex2D` exactly;
                // * descriptor set 0 (viewport uniform) and set 1 (one combined
                //   image sampler, non-arrayed) were bound above and are
                //   compatible with the pipeline layout (vulkano validates
                //   descriptor-set/layout compatibility on bind);
                // * the vertex buffer bound above holds this frame's
                //   `batch.vertices`, and the `first_vertex`/`vertex_count`
                //   ranges come from that same batch, so every draw stays
                //   inside its range;
                // * the dynamic viewport and scissor are set before the draw,
                //   and the shaders use no dynamic indexing or pointers.
                unsafe {
                    builder.draw(draw.vertex_count, 1, draw.first_vertex, 0)?;
                }
            }
        }

        builder
            .end_render_pass(Default::default())
            .context("ending the render pass")?;
        Ok(())
    }

    // -- offscreen screenshot ---------------------------------------------

    /// Renders one frame into the offscreen image and returns tightly packed
    /// RGBA8 bytes (already sRGB-encoded, so directly PNG-able).
    fn render_offscreen(&mut self, paint: &PaintList) -> Result<Vec<u8>> {
        if self.previous_frame_end.is_some() {
            self.wait_idle();
        }
        if self.recreate_swapchain {
            // A headless renderer has no swapchain; the flag means "the target
            // size changed" and the offscreen image must be reallocated before
            // it can be drawn into or read back at the new size.
            self.recreate_swapchain = false;
            self.recreate_offscreen_target()?;
        }

        self.textures.sync(paint)?;
        self.note_missing_textures(paint);
        let batch = FrameBatch::build(self.width, self.height, self.scale_factor, paint);
        let slot = self.prepare_slot(&batch)?;

        let Target::Offscreen { image, framebuffer } = &self.target else {
            return Err(anyhow!("render_offscreen called on a windowed renderer"));
        };

        let readback = Buffer::from_iter(
            self.ctx.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                ..Default::default()
            },
            (0..(self.width as usize * self.height as usize * 4)).map(|_| 0u8),
        )
        .context("allocating the screenshot readback buffer")?;

        let mut builder = AutoCommandBufferBuilder::primary(
            self.ctx.command_buffer_allocator.clone(),
            self.ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .context("recording the screenshot command buffer")?;
        self.record_draws(
            &mut builder,
            framebuffer,
            &batch,
            [self.width, self.height],
            slot,
        )?;
        builder
            .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(
                image.clone(),
                readback.clone(),
            ))
            .context("recording the offscreen readback")?;

        let command_buffer = builder
            .build()
            .context("building the screenshot command buffer")?;
        command_buffer
            .execute(self.ctx.queue.clone())
            .context("submitting the screenshot")?
            .then_signal_fence_and_flush()
            .context("flushing the screenshot")?
            .wait(None)
            .context("waiting for the screenshot")?;

        let bytes = readback
            .read()
            .context("reading back the screenshot pixels")?;
        if bytes.len() != self.width as usize * self.height as usize * 4 {
            return Err(anyhow!(
                "readback returned {} bytes, expected {}",
                bytes.len(),
                self.width as usize * self.height as usize * 4
            ));
        }
        Ok(bytes.to_vec())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if self.previous_frame_end.is_some() {
            self.wait_idle();
        }
        let _ = self.ctx.queue.with(|mut queue| queue.wait_idle());
    }
}

/// Render one frame offscreen to a PNG (used by `--screenshot` and tests).
///
/// # Errors
///
/// Returns an error when the headless Vulkan renderer cannot be created, when
/// the paint list cannot be drawn/read back, or when the PNG cannot be written
/// to `path`.
pub fn screenshot(
    width: u32,
    height: u32,
    scale_factor: f32,
    paint: &PaintList,
    path: &Path,
) -> Result<()> {
    let mut renderer = Renderer::new_headless(width, height, scale_factor)
        .context("creating the headless screenshot renderer")?;
    let pixels = renderer.render_offscreen(paint)?;
    write_png(path, renderer.width, renderer.height, &pixels)?;
    renderer.wait_idle();
    Ok(())
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating the screenshot directory {}", parent.display())
            })?;
        }
    }
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| anyhow!("screenshot buffer is not exactly {width}x{height}x4 bytes"))?;
    image
        .save(path)
        .with_context(|| format!("writing the screenshot to {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Vulkan resource helpers
// ---------------------------------------------------------------------------

fn create_uniform_ring(
    ctx: &GfxContext,
    pipeline: &PipelineBundle,
) -> Result<Vec<(Subbuffer<ViewportUbo>, Arc<DescriptorSet>)>> {
    let mut ring = Vec::with_capacity(FRAMES_IN_FLIGHT);
    for _ in 0..FRAMES_IN_FLIGHT {
        let buffer = Buffer::from_data(
            ctx.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::UNIFORM_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            ViewportUbo {
                viewport: [1.0, 1.0],
                _padding: [0.0; 2],
            },
        )
        .context("allocating a viewport uniform buffer")?;
        let set = DescriptorSet::new(
            ctx.descriptor_set_allocator.clone(),
            pipeline.ubo_set_layout.clone(),
            [WriteDescriptorSet::buffer(0, buffer.clone())],
            [],
        )
        .context("allocating the viewport uniform descriptor set")?;
        ring.push((buffer, set));
    }
    Ok(ring)
}

fn create_vertex_ring(ctx: &GfxContext, capacity: usize) -> Result<Vec<Subbuffer<[Vertex2D]>>> {
    (0..FRAMES_IN_FLIGHT)
        .map(|_| {
            Buffer::new_slice(
                ctx.memory_allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::VERTEX_BUFFER,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    // These are the only vertex buffers and every frame rewrites
                    // all of them from the CPU, so the streaming-friendly
                    // host-visible write-combined memory is the right home for
                    // them (the same memory type — and the same allocator block —
                    // the uniform ring and texture staging already use).
                    //
                    // `PREFER_DEVICE` would put them in a device-local host-visible
                    // type on GPUs that expose one (ReBAR). That type has no other
                    // user at startup, so the allocator had to make a whole new
                    // device memory block for it: one `vkAllocateMemory` of driver
                    // latency (~16 ms on the reference machine) on the startup
                    // path, for buffers whose content is re-uploaded every frame
                    // anyway. A UI vertex stream of a few hundred KiB per frame
                    // reads fine from system memory; the choice is measurable in
                    // the `startup:` logs, and a 300-frame benchmark showed
                    // identical frame times (4.07 ms avg, ~246 fps) either way.
                    memory_type_filter: MemoryTypeFilter::PREFER_HOST
                        | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                    ..Default::default()
                },
                capacity as u64,
            )
            .context("allocating a frame vertex buffer")
        })
        .collect()
}

fn create_framebuffers(
    render_pass: &Arc<RenderPass>,
    images: &[Arc<Image>],
) -> Result<Vec<Arc<Framebuffer>>> {
    images
        .iter()
        .map(|image| {
            let view =
                ImageView::new_default(image.clone()).context("creating a swapchain image view")?;
            Framebuffer::new(
                render_pass.clone(),
                FramebufferCreateInfo {
                    attachments: vec![view],
                    ..Default::default()
                },
            )
            .context("creating a swapchain framebuffer")
        })
        .collect()
}

/// Clamp a requested swapchain extent into what the surface actually supports.
///
/// Vulkan requires `imageExtent` to lie within `minImageExtent..=maxImageExtent`,
/// and a surface with a fixed `currentExtent` (some Wayland compositors report
/// one) must be created at exactly that size — otherwise swapchain creation
/// fails. Window sizes are passed through this before every creation and
/// recreation.
fn clamp_swapchain_extent(caps: &SurfaceCapabilities, requested: [u32; 2]) -> [u32; 2] {
    if let Some(current) = caps.current_extent {
        return [current[0].max(1), current[1].max(1)];
    }
    [
        requested[0]
            .clamp(caps.min_image_extent[0], caps.max_image_extent[0])
            .max(1),
        requested[1]
            .clamp(caps.min_image_extent[1], caps.max_image_extent[1])
            .max(1),
    ]
}

/// How many swapchain images to request: one more than the driver's minimum
/// (the usual Vulkan guidance — it gives the presentation engine a spare image
/// so acquire/present do not stall the CPU), capped when the driver reports a
/// maximum. The spec guarantees FIFO is supported with at least this many.
fn swapchain_image_count(caps: &SurfaceCapabilities) -> u32 {
    let wanted = caps.min_image_count.saturating_add(1).max(2);
    let capped = caps.max_image_count.map_or(wanted, |max| wanted.min(max));
    capped.max(caps.min_image_count)
}

/// Pick a composite alpha mode: `Opaque` when the surface supports it (cheapest
/// for the compositor, and this window has no translucent areas), otherwise the
/// first mode the driver offers.
fn pick_composite_alpha(caps: &SurfaceCapabilities) -> Result<CompositeAlpha> {
    let supported: Vec<CompositeAlpha> = caps.supported_composite_alpha.into_iter().collect();
    supported
        .iter()
        .copied()
        .find(|alpha| matches!(alpha, CompositeAlpha::Opaque))
        .or_else(|| supported.first().copied())
        .ok_or_else(|| anyhow!("the surface supports no composite alpha mode"))
}

/// A newly created swapchain: the swapchain, its framebuffers, and the extent
/// actually used (the surface may have clamped the requested window size).
type SurfaceSwapchain = (Arc<Swapchain>, Vec<Arc<Framebuffer>>, [u32; 2]);

fn create_swapchain_surface(
    ctx: &GfxContext,
    surface: &Arc<Surface>,
    size: [u32; 2],
    format: Format,
    render_pass: &Arc<RenderPass>,
) -> Result<SurfaceSwapchain> {
    let caps = ctx
        .physical_device
        .surface_capabilities(surface, Default::default())
        .context("querying surface capabilities")?;
    let composite_alpha = pick_composite_alpha(&caps)?;
    let image_extent = clamp_swapchain_extent(&caps, size);
    if image_extent != size {
        tracing::debug!(
            requested = ?size,
            actual = ?image_extent,
            "surface clamped the requested swapchain extent"
        );
    }

    let (swapchain, images) = Swapchain::new(
        ctx.device.clone(),
        surface.clone(),
        SwapchainCreateInfo {
            min_image_count: swapchain_image_count(&caps),
            image_format: format,
            image_extent,
            image_usage: ImageUsage::COLOR_ATTACHMENT,
            composite_alpha,
            // Vsync present mode: a music player should not burn GPU time or
            // tear to shave latency. FIFO is the one mode every surface is
            // required to support, so this needs no present-mode query and is
            // stable across drivers.
            present_mode: PresentMode::Fifo,
            ..Default::default()
        },
    )
    .context("creating the swapchain")?;

    let framebuffers = create_framebuffers(render_pass, &images)?;
    Ok((swapchain, framebuffers, image_extent))
}

fn create_offscreen_target(
    ctx: &GfxContext,
    render_pass: &Arc<RenderPass>,
    size: [u32; 2],
) -> Result<(Arc<Image>, Arc<Framebuffer>)> {
    let image = Image::new(
        ctx.memory_allocator.clone(),
        ImageCreateInfo {
            format: Format::R8G8B8A8_SRGB,
            usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
            extent: [size[0], size[1], 1],
            ..Default::default()
        },
        AllocationCreateInfo::default(),
    )
    .context("allocating the offscreen render target")?;
    let view =
        ImageView::new_default(image.clone()).context("creating the offscreen image view")?;
    let framebuffer = Framebuffer::new(
        render_pass.clone(),
        FramebufferCreateInfo {
            attachments: vec![view],
            ..Default::default()
        },
    )
    .context("creating the offscreen framebuffer")?;
    Ok((image, framebuffer))
}

/// Picks an sRGB surface format when the driver offers one: the whole pipeline
/// is built around hardware sRGB encoding (see the module docs).
fn pick_surface_format(ctx: &GfxContext, surface: &Arc<Surface>) -> Result<Format> {
    let formats = ctx
        .physical_device
        .surface_formats(surface, Default::default())
        .context("querying surface formats")?;
    let preferred = [Format::B8G8R8A8_SRGB, Format::R8G8B8A8_SRGB];
    if let Some(format) = preferred.into_iter().find(|format| {
        formats.iter().any(|(candidate, color_space)| {
            candidate == format && *color_space == ColorSpace::SrgbNonLinear
        })
    }) {
        return Ok(format);
    }

    let fallback = formats
        .first()
        .map(|(format, _)| *format)
        .ok_or_else(|| anyhow!("the window surface reports no supported formats"))?;
    tracing::warn!(
        format = ?fallback,
        "no sRGB surface format is available; colours will be composited incorrectly"
    );
    Ok(fallback)
}
