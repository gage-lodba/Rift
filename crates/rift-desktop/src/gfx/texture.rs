//! GPU texture registry driven by [`PaintList`].
//!
//! The UI owns the id space and tells the renderer what changed:
//! * `PaintList::created` — new `TextureUpload`s (glyph atlas, cover art, …);
//! * `PaintList::updated` — replacement pixels for an existing id (the font
//!   atlas grows 1024 -> 2048 when glyphs run out, so this is a full replace);
//! * `PaintList::dropped`  — ids the UI will never reference again.
//!
//! For each id the renderer keeps an `R8G8B8A8_SRGB` device-local image, an
//! image view, and a descriptor set with a combined image sampler. Uploads go
//! through a host-visible staging buffer and a one-shot command buffer that is
//! waited on; uploads are infrequent (texture creation / atlas growth), so the
//! simple synchronous path is worth more than a frame of latency. Pixel data is
//! sRGB-encoded RGBA8, exactly what the PNG/JPEG decode and the font rasteriser
//! produce; the sRGB image format makes the sampler decode it to linear light.
//!
//! Dropped/replaced textures are retired for a few frames before their Vulkan
//! handles are released, so a command buffer that is still executing on the GPU
//! cannot observe a destroyed image.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result};
use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage},
    command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage, CopyBufferToImageInfo,
        PrimaryCommandBufferAbstract,
    },
    descriptor_set::{layout::DescriptorSetLayout, DescriptorSet, WriteDescriptorSet},
    format::Format,
    image::{
        sampler::{Filter, Sampler, SamplerAddressMode, SamplerCreateInfo},
        view::ImageView,
        Image, ImageCreateInfo, ImageUsage,
    },
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    sync::GpuFuture,
};

use crate::gfx::{
    gpu::GfxContext,
    paint::{PaintList, TextureId, TextureUpload, NO_TEXTURE},
};

/// How many frames a replaced/dropped texture is kept alive (must be at least
/// the number of frames that can be in flight).
const RETIRE_FRAMES: u64 = 4;

/// A texture's GPU resources plus the descriptor set used to bind it.
pub(crate) struct GpuTexture {
    pub set: Arc<DescriptorSet>,
    // Kept alive for as long as `set`/frames may reference them.
    _view: Arc<ImageView>,
    _image: Arc<Image>,
}

pub(crate) struct TextureManager {
    memory_allocator: Arc<StandardMemoryAllocator>,
    command_buffer_allocator:
        Arc<vulkano::command_buffer::allocator::StandardCommandBufferAllocator>,
    descriptor_set_allocator:
        Arc<vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator>,
    queue: Arc<vulkano::device::Queue>,
    set_layout: Arc<DescriptorSetLayout>,
    sampler: Arc<Sampler>,
    textures: HashMap<TextureId, GpuTexture>,
    retired: Vec<(u64, GpuTexture)>,
    /// 1x1 opaque white texture bound for solid-color prims (`NO_TEXTURE`).
    white: GpuTexture,
    frame: u64,
    rasterized_pixels: u64,
    /// Monotonic counters, for acceptance logs/diagnostics.
    created_total: u64,
    dropped_total: u64,
    updated_total: u64,
}

impl TextureManager {
    pub fn new(ctx: &GfxContext, set_layout: Arc<DescriptorSetLayout>) -> Result<Self> {
        let sampler = Sampler::new(
            ctx.device.clone(),
            SamplerCreateInfo {
                // Linear filtering makes atlas growth / sub-pixel glyph placement
                // smooth; cover art downscales far nicer than nearest.
                mag_filter: Filter::Linear,
                min_filter: Filter::Linear,
                address_mode: [SamplerAddressMode::ClampToEdge; 3],
                ..Default::default()
            },
        )
        .context("creating the UI texture sampler")?;

        let white = Self::create_gpu_texture_static(
            ctx,
            &set_layout,
            &sampler,
            1,
            1,
            &[255, 255, 255, 255],
        )
        .context("creating the 1x1 white texture")?;

        Ok(Self {
            memory_allocator: ctx.memory_allocator.clone(),
            command_buffer_allocator: ctx.command_buffer_allocator.clone(),
            descriptor_set_allocator: ctx.descriptor_set_allocator.clone(),
            queue: ctx.queue.clone(),
            set_layout,
            sampler,
            textures: HashMap::new(),
            retired: Vec::new(),
            white,
            frame: 0,
            rasterized_pixels: 0,
            created_total: 0,
            dropped_total: 0,
            updated_total: 0,
        })
    }

    /// The descriptor set to bind for `id` (falls back to the white texture).
    pub fn descriptor_set(&self, id: TextureId) -> Arc<DescriptorSet> {
        if id == NO_TEXTURE {
            return self.white.set.clone();
        }
        self.textures
            .get(&id)
            .map(|texture| texture.set.clone())
            .unwrap_or_else(|| self.white.set.clone())
    }

    pub fn contains(&self, id: TextureId) -> bool {
        id == NO_TEXTURE || self.textures.contains_key(&id)
    }

    #[allow(dead_code)] // used by renderer diagnostics / future stats HUD
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// Total number of textures registered since startup (diagnostics only).
    pub fn created_total(&self) -> u64 {
        self.created_total
    }

    /// Total number of textures dropped since startup (diagnostics only).
    pub fn dropped_total(&self) -> u64 {
        self.dropped_total
    }

    /// Total number of texture pixel replacements since startup (diagnostics).
    pub fn updated_total(&self) -> u64 {
        self.updated_total
    }

    /// Applies one frame's `created` / `updated` / `dropped` lists.
    ///
    /// All uploads of the frame are batched into one command buffer and the CPU
    /// waits for it; the paint list is small and uploads are rare, so this keeps
    /// the in-flight tracking trivial and bug-free.
    pub fn sync(&mut self, paint: &PaintList) -> Result<()> {
        self.frame += 1;
        self.dropped_total += paint.dropped.len() as u64;
        self.updated_total += paint.updated.len() as u64;

        for id in &paint.dropped {
            if let Some(texture) = self.textures.remove(id) {
                self.retired.push((self.frame, texture));
            }
        }

        // `updated` replaces the whole texture (atlas growth); `created` inserts.
        // An id may appear in both if the UI re-created it this frame.
        let mut uploads: Vec<&TextureUpload> = Vec::new();
        for upload in paint.created.iter().chain(paint.updated.iter()) {
            if upload.width == 0 || upload.height == 0 {
                continue;
            }
            let expected = upload.width as usize * upload.height as usize * 4;
            if upload.rgba.len() != expected {
                tracing::warn!(
                    id = upload.id,
                    width = upload.width,
                    height = upload.height,
                    got = upload.rgba.len(),
                    expected,
                    "ignoring texture upload with the wrong pixel buffer size"
                );
                continue;
            }
            // Last upload for an id wins (created before updated, per contract).
            uploads.retain(|other| other.id != upload.id);
            uploads.push(upload);
        }

        if !uploads.is_empty() || !paint.dropped.is_empty() {
            tracing::debug!(
                created = paint.created.len(),
                updated = paint.updated.len(),
                dropped = paint.dropped.len(),
                uploaded = uploads.len(),
                upload_ids = ?uploads.iter().map(|upload| upload.id).collect::<Vec<_>>(),
                dropped_ids = ?paint.dropped,
                live = self.textures.len(),
                "texture sync"
            );
        }

        if !uploads.is_empty() {
            let mut staging = Vec::with_capacity(uploads.len());
            let mut builder = AutoCommandBufferBuilder::primary(
                self.command_buffer_allocator.clone(),
                self.queue.queue_family_index(),
                CommandBufferUsage::OneTimeSubmit,
            )
            .context("recording the texture upload command buffer")?;

            for upload in &uploads {
                let image = upload_image(&self.memory_allocator, upload.width, upload.height)?;
                let buffer = Buffer::from_iter(
                    self.memory_allocator.clone(),
                    BufferCreateInfo {
                        usage: BufferUsage::TRANSFER_SRC,
                        ..Default::default()
                    },
                    AllocationCreateInfo {
                        memory_type_filter: MemoryTypeFilter::PREFER_HOST
                            | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                        ..Default::default()
                    },
                    upload.rgba.iter().copied(),
                )
                .context("allocating the texture staging buffer")?;
                builder
                    .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                        buffer.clone(),
                        image.clone(),
                    ))
                    .context("recording a staging -> image copy")?;
                staging.push((image, buffer));
                self.rasterized_pixels += upload.width as u64 * upload.height as u64;
            }

            let command_buffer = builder
                .build()
                .context("building the upload command buffer")?;
            command_buffer
                .execute(self.queue.clone())
                .context("submitting the texture upload")?
                .then_signal_fence_and_flush()
                .context("flushing the texture upload")?
                .wait(None)
                .context("waiting for the texture upload")?;
            // `staging` can be dropped now: the upload has completed and the
            // device-local images are the only remaining references.

            for (upload, (image, _buffer)) in uploads.iter().zip(staging) {
                let texture = self.create_gpu_texture(image)?;
                let previous = self.textures.insert(upload.id, texture);
                self.created_total += 1;
                if let Some(previous) = previous {
                    self.retired.push((self.frame, previous));
                }
            }
        }

        // Release textures that no in-flight frame can reference any more.
        self.retired
            .retain(|(frame, _)| self.frame.saturating_sub(*frame) < RETIRE_FRAMES);
        Ok(())
    }

    fn create_gpu_texture(&self, image: Arc<Image>) -> Result<GpuTexture> {
        build_gpu_texture(
            &self.descriptor_set_allocator,
            &self.set_layout,
            &self.sampler,
            image,
        )
        .context("creating GPU texture resources")
    }

    fn create_gpu_texture_static(
        ctx: &GfxContext,
        set_layout: &Arc<DescriptorSetLayout>,
        sampler: &Arc<Sampler>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<GpuTexture> {
        let image = upload_image(&ctx.memory_allocator, width, height)?;
        let buffer = Buffer::from_iter(
            ctx.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            rgba.iter().copied(),
        )?;
        let mut builder = AutoCommandBufferBuilder::primary(
            ctx.command_buffer_allocator.clone(),
            ctx.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )?;
        builder.copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(buffer, image.clone()))?;
        let command_buffer = builder.build()?;
        command_buffer
            .execute(ctx.queue.clone())?
            .then_signal_fence_and_flush()?
            .wait(None)?;
        build_gpu_texture(&ctx.descriptor_set_allocator, set_layout, sampler, image)
    }
}

/// Allocates a sampled device-local RGBA8 sRGB image with transfer-dst usage.
fn upload_image(
    memory_allocator: &Arc<StandardMemoryAllocator>,
    width: u32,
    height: u32,
) -> Result<Arc<Image>> {
    Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format: Format::R8G8B8A8_SRGB,
            usage: ImageUsage::SAMPLED | ImageUsage::TRANSFER_DST,
            extent: [width, height, 1],
            ..Default::default()
        },
        AllocationCreateInfo::default(),
    )
    .context("allocating a texture image")
}

fn build_gpu_texture(
    descriptor_set_allocator: &Arc<
        vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator,
    >,
    set_layout: &Arc<DescriptorSetLayout>,
    sampler: &Arc<Sampler>,
    image: Arc<Image>,
) -> Result<GpuTexture> {
    let view = ImageView::new_default(image.clone()).context("creating a texture image view")?;
    let set = DescriptorSet::new(
        descriptor_set_allocator.clone(),
        set_layout.clone(),
        [WriteDescriptorSet::image_view_sampler(
            0,
            view.clone(),
            sampler.clone(),
        )],
        [],
    )
    .context("allocating a texture descriptor set")?;
    Ok(GpuTexture {
        set,
        _view: view,
        _image: image,
    })
}
