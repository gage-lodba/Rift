# Vulkano 0.35 + winit 0.30 recipe for Rift (historical M1 probe results)

Status: **verified on the target machine** (Windows 11, AMD Radeon RX 7800 XT,
AMD proprietary driver, Vulkan 1.3.281, Rust 1.88.0) during the conversion.
Everything below was executed, not inferred from docs. The proving ground was a
throwaway probe crate in the gitignored `scratch/vulkan-probe/`, which was
removed with the scratch space — **it is not shipped, and the probe commands
below cannot be re-run from a clean checkout**. The Vulkano lessons and the
working code skeleton are kept because `crates/rift-desktop/src/gfx/` is built
from them; the raw probe evidence lived in `scratch/vulkan-probe/proof/`.

Historical probe evidence (raw files removed; kept for reference):

| File (historical) | What it proved |
|---|---|
| `proof/window.png` | Real winit window + swapchain, live triangle rendered on the desktop (800x600, captured with `CopyFromScreen`) |
| `proof/window-resized.png` | Same process after `SetWindowPos(1000x700)`: swapchain recreated, still rendering |
| `proof/window.log` | Full log: device, formats, caps, `SWAPCHAIN_RECREATED`, `WINDOW_CLOSE_REQUESTED`, clean exit code 0 |
| `proof/offscreen.png` | Headless render (no window/swapchain) of a texture uploaded through a staging buffer, read back and saved as PNG |
| `proof/validation.log` | Same offscreen run with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`; no validation messages |
| `proof/run-window-proof.ps1` | The automated harness that produced the window evidence |

Historical probe commands (only meaningful if the probe were restored; kept so
the setup can be rebuilt if the renderer is ever re-probed):

```powershell
# NOT shipped: the probe crate was removed with the gitignored scratch space.
cd scratch/vulkan-probe
$env:CARGO_TARGET_DIR = "$PWD\target"      # keep away from the main target dir
cargo run --offline -- --offscreen proof/offscreen.png --size 640x480
cargo build
pwsh -NoProfile -File proof/run-window-proof.ps1   # window + resize + clean close
```

Current-tree reproduction uses only shipped tooling — see
[verification.md §Reproduce](verification.md#reproduce-current-tree):
`cargo test -p rift-desktop gfx`, the `--preview --screenshot` CLI,
`pwsh tools/window_shot.ps1`, `pwsh tools/verify.ps1` and
`pwsh tools/ci-local.ps1`.

## Production renderer verification (crates/rift-desktop/src/gfx/)

The renderer that ships in `crates/rift-desktop/src/gfx/` was verified the same way, through the
real app binary (and, while the rest of the app was still being written, through
an isolated harness that has since been removed with the scratch space). The
commands below all run from the current tree:

```powershell
# 1. Offscreen screenshot through the real app + real UI PaintList (1160x740)
cargo run -q -p rift-desktop -- --preview --screenshot artifacts\screens\home.png --size 1160x740 --view home
#    -> EXIT 0, "wrote 1160x740 screenshot"; compare with the archived old UI:
#       python tools/compare_images.py docs/vulkan-migration/reference-old-ui.png artifacts/screens/home.png
#       (first-acceptance run: mean |dc| = 3.13, pixels>16 = 3.61%;
#        final measured values are 2.14 / 2.97 %, see verification.md §7 and
#        screenshot-gallery.md §1)

# 2. Every preview view renders (home, liked, playlist, search, artist,
#    artist-songs, album, settings): all EXIT 0, no stderr.

# 3. Small-size screenshot (exercises clipping/overdraw at a size smaller than
#    the layout):
cargo run -q -p rift-desktop -- --preview --screenshot artifacts\screens\home-320.png --size 320x240 --view home

# 4. Windowed app (swapchain path), captured from the desktop:
pwsh tools/window_shot.ps1 -Out artifacts\screens\window.png
#    -> 1160x740 window capture, correct UI through the swapchain

# 5. Renderer unit/GPU tests (headless, no window):
cargo test -p rift-desktop gfx
#    -> passes: SDF rounded rect + border, texture UVs, clip/scissor layer,
#       empty paint list, texture update, multi-frame texture lifecycle,
#       atlas-growth stress, PNG helper (run it for the current count)
$env:RIFT_VK_VALIDATION = "1"; cargo test -p rift-desktop gfx   # zero validation errors

# 6. Windowed swapchain loop under the validation layer:
#    the isolated harness that did resize -> WM_CLOSE (scratch/gfx-check) was
#    removed with the scratch space. Current equivalent: launch the preview
#    window with RIFT_VK_VALIDATION=1 (winit window + swapchain), resize it
#    manually, and close it with the titlebar X / WM_CLOSE.
```

Evidence: the committed copies are
[`screenshots/`](screenshots/) (`01-home.png` … `08-settings.png` for the
preview views, `11-live-window-real-covers.png` for the live window) and
[`evidence/final-gate.log`](evidence/final-gate.log). Fresh runs write raw
captures to the gitignored `artifacts/screens/`.

Final acceptance (windowed live cover textures, resize/DPI/window-state,
300-frame performance, validation-layer runs) is documented in
[`verification.md`](verification.md).

## Pinned dependencies (both probe and `crates/rift-desktop/Cargo.toml`)

`crates/rift-desktop/Cargo.toml` pins exactly this; the historical probe used the
same lines. `default-features = false` means the Linux-only `x11` feature is
off; `macros` is needed for `vulkano::single_pass_renderpass!` (and for
`#[derive(Vertex)]`/`#[derive(BufferContents)]`).

```toml
vulkano            = { version = "0.35.2", default-features = false, features = ["macros"] }
winit              = "0.30"                # resolved 0.30.13
raw-window-handle  = "0.6"                 # resolved 0.6.2
image              = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
```

Resolved transitive versions of note: `vulkano-macros 0.35.0`, `ash
0.38.0+1.3.281`, `ash 0.38` is re-exported, never depend on it directly. Rust
1.88.0 (repo `rust-toolchain.toml`) is enough for vulkano 0.35.2 (its
`rust-version` is 1.75). `vulkano-shaders` is **not** needed: Rift commits
`.spv` files and loads them as bytes, so the proc-macro parsing/`shader!` path
is avoided entirely.

## GLSL -> SPIR-V workflow

Vulkan SDK 1.4.363 at `C:\VulkanSDK\1.4.363.0`:

```powershell
$glslc = "C:\VulkanSDK\1.4.363.0\Bin\glslc.exe"
$val   = "C:\VulkanSDK\1.4.363.0\Bin\spirv-val.exe"
cd crates\rift-desktop\shaders
& $glslc quad.vert -o quad.vert.spv      # produces SPIR-V 1.0, entry point "main"
& $glslc quad.frag -o quad.frag.spv
& $val quad.vert.spv; & $val quad.frag.spv
```

* `glslc` defaults to `--target-env=vulkan1.0` (SPIR-V 1.0), which is what we
  want: it runs on every Vulkan 1.0+ driver and avoids needing a 1.3 device.
* Commit the `.spv` files (`crates/rift-desktop/shaders/*.spv`). Do not compile at build time:
  it keeps `cargo build` independent of the Vulkan SDK, and the renderer can be
  checked in CI without glslc installed.
* `include_bytes!` path is relative to the **source file**: from
  `crates/rift-desktop/src/gfx/pipeline.rs` it is `include_bytes!("../../shaders/quad.vert.spv")`.

Loading raw SPIR-V in vulkano 0.35 (no `shader!` macro):

```rust
use vulkano::shader::{spirv::bytes_to_words, ShaderModule, ShaderModuleCreateInfo};

fn load_shader(device: &Arc<Device>, bytes: &[u8]) -> Result<Arc<ShaderModule>, Box<dyn Error>> {
    // bytes_to_words returns Cow<[u32]>; it errors if len % 4 != 0.
    let words = bytes_to_words(bytes)?;
    // SAFETY: the bytes are a committed, spirv-val-checked glslc output.
    let module = unsafe { ShaderModule::new(device.clone(), ShaderModuleCreateInfo::new(&words)) }?;
    Ok(module)
}

let vs_module = load_shader(device, include_bytes!("../../shaders/quad.vert.spv"))?;
let vs = vs_module.entry_point("main").ok_or("missing entry point")?;
```

Even without `vulkano-shaders`, vulkano parses the SPIR-V it is given, so
**reflection still works**: `Vertex2D::per_vertex().definition(&vs)` derives the
vertex input state and `PipelineDescriptorSetLayoutCreateInfo::from_stages`
derives descriptor set layouts. You only lose the generated Rust structs for
push constants (we use a UBO instead; see below).

## Winit 0.30 plumbing (`ApplicationHandler`)

Window creation must happen after `resumed`. The window must be behind an `Arc`
because `Surface::from_window` shares ownership of it (the `Surface` keeps the
window alive via `raw-window-handle`).

```rust
fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = winit::event_loop::EventLoop::new()?;
    let ctx = VulkanContext::new_window(&event_loop)?; // instance+device up front
    let mut app = App { ctx, rcx: None };
    event_loop.run_app(&mut app)?;                     // returns after exit()
    Ok(())
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.rcx.is_some() { return; }
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Rift")
                    .with_inner_size(LogicalSize::new(1160.0, 740.0))
                    .with_min_inner_size(LogicalSize::new(940.0, 620.0))
                    .with_decorations(false)          // Rift draws its own titlebar
            )?,
        );
        let surface = Surface::from_window(self.ctx.instance.clone(), window.clone())?;
        // ... swapchain, pipeline, framebuffers ...
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),   // clean exit, code 0
            WindowEvent::Resized(_) => { /* mark swapchain dirty, recreate in next frame */ }
            WindowEvent::RedrawRequested => { /* render one frame */ }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(rcx) = &self.rcx { rcx.window.request_redraw(); }
    }
}
```

Notes:
* `Surface::required_extensions(event_loop_or_window)` needs an
  `impl HasDisplayHandle`. Both `EventLoop`/`ActiveEventLoop` and `Window` work;
  use the window inside `Renderer::new`, or the event loop before creating it.
* On Windows this adds `khr_surface` + `khr_win32_surface` to the instance.
* Windows initial-size quirk: with `with_decorations(false)` + `with_inner_size`
  the probe observed several `Resized` events before the first frame
  (`1904x984`, `1920x1023`, then the requested `800x600`). Treat resize as a
  flag, never assume the first event is the final size; skip zero sizes
  (minimized) and recreate the swapchain lazily in the next frame.
* `event_loop.set_control_flow(ControlFlow::WaitUntil(...))` + `request_redraw`
  in `about_to_wait` gives a steady frame loop; when occluded, Windows stops
  delivering `RedrawRequested`.
* Testing "close exits cleanly" from a script: find the HWND with
  `(Get-Process -Id $pid).MainWindowHandle` (do **not** use `FindWindowW` from
  an agent harness process — it returned 0 even though `GetWindowText` on the
  handle showed the right title), then `PostMessage(hwnd, WM_CLOSE=0x0010, 0, 0)`.
  The winit loop delivers `CloseRequested`, `exit()` runs, exit code 0.

## Instance and device

```rust
let library = VulkanLibrary::new()?;
let extensions = Surface::required_extensions(&event_loop)?;     // windowed only
let instance = Instance::new(
    library,
    InstanceCreateInfo {
        flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,       // harmless on Windows
        enabled_extensions: extensions,
        ..Default::default()
    },
)?;
```

Device selection (windowed): require `khr_swapchain`, and a queue family that
supports `GRAPHICS` **and** can present to the surface; then score by
`PhysicalDeviceType` (Discrete=0 … Other=4).

```rust
let device_extensions = DeviceExtensions { khr_swapchain: true, ..DeviceExtensions::empty() };
let (physical_device, queue_family_index) = instance
    .enumerate_physical_devices()?
    .filter(|p| p.supported_extensions().contains(&device_extensions))
    .filter_map(|p| {
        p.queue_family_properties().iter().enumerate()
            .position(|(i, q)| {
                q.queue_flags.intersects(QueueFlags::GRAPHICS)
                    && p.presentation_support(i as u32, &event_loop).unwrap_or(false)
            })
            .map(|i| (p, i as u32))
    })
    .min_by_key(|(p, _)| match p.properties().device_type {
        PhysicalDeviceType::DiscreteGpu => 0, /* Integrated 1, Virtual 2, Cpu 3, Other 4 */
        _ => 9,
    })
    .ok_or("no suitable device")?;

let (device, mut queues) = Device::new(                          // returns an iterator!
    physical_device,
    DeviceCreateInfo {
        queue_create_infos: vec![QueueCreateInfo { queue_family_index, ..Default::default() }],
        enabled_extensions: device_extensions,
        ..Default::default()
    },
)?;
let queue = queues.next().unwrap();
```

Headless (screenshot) mode: **no surface extensions, no `khr_swapchain`**, just
require a graphics queue. Everything below is shared with the window path except
the surface/swapchain.

Allocators: one `StandardMemoryAllocator`, one
`StandardCommandBufferAllocator`, and one `StandardDescriptorSetAllocator` per
device, shared for the whole app lifetime.

```rust
let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
let command_buffer_allocator = Arc::new(StandardCommandBufferAllocator::new(device.clone(), Default::default()));
let descriptor_set_allocator = Arc::new(StandardDescriptorSetAllocator::new(device.clone(), Default::default()));
```

Optional validation layer: set `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`
before launching (the loader enables it without code changes; vulkano also
supports `InstanceCreateInfo { enabled_layers: vec![...], .. }`). The probe was
run this way with zero validation messages. The production renderer also honours
`RIFT_VK_VALIDATION=1` and installs a debug-utils callback that logs instead of
panicking.

## Swapchain

```rust
// Pick an sRGB format if the surface offers one; otherwise fall back and encode
// manually in the fragment shader. This machine offers:
//   R8G8B8A8_UNORM, B8G8R8A8_UNORM, A2B10G10R10_UNORM_PACK32, R8G8B8A8_SRGB, B8G8R8A8_SRGB
let formats = physical_device.surface_formats(&surface, Default::default())?; // Vec<(Format, ColorSpace)>
let image_format = [Format::B8G8R8A8_SRGB, Format::R8G8B8A8_SRGB]
    .into_iter()
    .find(|f| formats.iter().any(|(fmt, cs)| fmt == f && *cs == ColorSpace::SrgbNonLinear))
    .unwrap_or(formats[0].0);

let caps = physical_device.surface_capabilities(&surface, Default::default())?;
let (swapchain, images) = Swapchain::new(device.clone(), surface.clone(), SwapchainCreateInfo {
    min_image_count: caps.min_image_count.max(2),   // min_image_count can report 1; fullscreen needs >= 2
    image_format,
    image_extent: [w, h],                            // window.inner_size() in physical px
    image_usage: ImageUsage::COLOR_ATTACHMENT,
    composite_alpha: caps.supported_composite_alpha.into_iter().next().unwrap(), // OPAQUE here
    ..Default::default()                             // present_mode: Fifo, image_sharing: Exclusive, clipped: true
})?;
```

Resize: recreate from the old swapchain, then rebuild image views/framebuffers:

```rust
let (new_swapchain, new_images) = swapchain.recreate(SwapchainCreateInfo {
    image_extent: new_size,
    ..swapchain.create_info()        // keep format/composite alpha/usage/etc.
})?;
```

Handle `VulkanError::OutOfDate` from acquire/present by setting a
`recreate_swapchain` flag and retrying next frame; `is_suboptimal` should also
schedule a recreation. Never create a zero-sized swapchain (skip when
`inner_size` is 0).

## Render pass, framebuffers, pipeline

Classic render pass (not dynamic rendering) — a single color attachment that is
cleared and stored, recreated per format. The macro accepts a runtime `format`.

```rust
let render_pass = vulkano::single_pass_renderpass!(
    device.clone(),
    attachments: {
        color: { format: format, samples: 1, load_op: Clear, store_op: Store },
    },
    pass: { color: [color], depth_stencil: {} },
)?;

let framebuffers: Vec<_> = images.iter().map(|image| {
    let view = ImageView::new_default(image.clone())?;
    Framebuffer::new(render_pass.clone(), FramebufferCreateInfo {
        attachments: vec![view],          // extent is derived from the attachment
        ..Default::default()              // layers: 1
    }).map_err(Into::into)
}).collect::<Result<_, Box<dyn Error>>>()?;
```

Pipeline:

```rust
let stages = [
    PipelineShaderStageCreateInfo::new(vs_entry_point),
    PipelineShaderStageCreateInfo::new(fs_entry_point),
];

// Reflection-derived descriptor set layout (works with raw SPIR-V!).
// The probe used this; the production renderer builds the two set layouts by
// hand because set 0 uses a *dynamic* uniform buffer (see "Per-frame buffers").
let layout = PipelineLayout::new(
    device.clone(),
    PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
        .into_pipeline_layout_create_info(device.clone())?,
)?;

let subpass = Subpass::from(render_pass.clone(), 0).ok_or("no subpass 0")?;
let pipeline = GraphicsPipeline::new(device.clone(), None, GraphicsPipelineCreateInfo {
    stages: stages.into_iter().collect(),
    vertex_input_state: Some(Vertex2D::per_vertex().definition(&vs)?), // VertexDefinition from EntryPoint
    input_assembly_state: Some(InputAssemblyState::default()),
    viewport_state: Some(ViewportState::default()),   // 1 viewport + 1 full-screen scissor
    rasterization_state: Some(RasterizationState::default()),
    multisample_state: Some(MultisampleState::default()),
    color_blend_state: Some(ColorBlendState::with_attachment_states(
        subpass.num_color_attachments(),
        ColorBlendAttachmentState {
            // Straight-alpha source-over; Rift's shaders output premultiplied
            // and use (One, OneMinusSrcAlpha) instead.
            blend: Some(AttachmentBlend {
                src_color_blend_factor: BlendFactor::SrcAlpha,
                dst_color_blend_factor: BlendFactor::OneMinusSrcAlpha,
                color_blend_op: BlendOp::Add,
                src_alpha_blend_factor: BlendFactor::One,
                dst_alpha_blend_factor: BlendFactor::OneMinusSrcAlpha,
                alpha_blend_op: BlendOp::Add,
            }),
            ..Default::default()
        },
    )),
    dynamic_state: [DynamicState::Viewport, DynamicState::Scissor].into_iter().collect(),
    subpass: Some(subpass.into()),
    ..GraphicsPipelineCreateInfo::layout(layout.clone())
})?;
```

Vertex input with raw SPIR-V: derive `#[derive(BufferContents, Vertex)]` on a
`#[repr(C)]` struct with `#[format(R32G32_SFLOAT)]` etc., then
`MyVertex::per_vertex().definition(&vs_entry_point)` — the entry point does not
have to come from `vulkano-shaders`.

## Command buffers, acquire/present, fences

```rust
let mut builder = AutoCommandBufferBuilder::primary(
    command_buffer_allocator.clone(),
    queue.queue_family_index(),
    CommandBufferUsage::OneTimeSubmit,
)?;
builder.begin_render_pass(
    RenderPassBeginInfo {
        clear_values: vec![Some([r, g, b, a].into())],
        ..RenderPassBeginInfo::framebuffer(framebuffers[image_index as usize].clone())
    },
    SubpassBeginInfo { contents: SubpassContents::Inline, ..Default::default() },
)?;
builder
    .set_viewport(0, [viewport].into_iter().collect())?       // SmallVec param: `collect()`
    .set_scissor(0, [scissor].into_iter().collect())?
    .bind_pipeline_graphics(pipeline.clone())?
    .bind_vertex_buffers(0, vertex_buffer.clone())?
    .bind_descriptor_sets(PipelineBindPoint::Graphics, layout.clone(), 0, descriptor_set.clone())?;
unsafe { builder.draw(vertex_count, 1, 0, 0)? };              // unsafe: shader/resource contract
builder.end_render_pass(Default::default())?;
let command_buffer = builder.build()?;

let (image_index, suboptimal, acquire_future) =
    acquire_next_image(swapchain.clone(), None).map_err(Validated::unwrap)?; // blocks

let future = previous_frame_end.take().unwrap()
    .join(acquire_future)
    .then_execute(queue.clone(), command_buffer)?
    .then_swapchain_present(queue.clone(),
        SwapchainPresentInfo::swapchain_image_index(swapchain.clone(), image_index))
    .then_signal_fence_and_flush();

match future.map_err(Validated::unwrap) {
    Ok(f) => previous_frame_end = Some(f.boxed()),
    Err(VulkanError::OutOfDate) => { recreate_swapchain = true; previous_frame_end = Some(sync::now(device.clone()).boxed()); }
    Err(e) => return Err(e.into()),
}
```

Every frame starts with `previous_frame_end.as_mut().unwrap().cleanup_finished()`
to release resources the GPU has finished with.

## API gotchas that cost time (vulkano 0.35 specifics)

1. **Host-writing a resource the GPU may still read is a validation error.**
   The first probe version wrote one uniform buffer every frame and failed on
   frame 2 with:

   ```
   PROBE ERROR: frame failed: the resource is already in use in a conflicting way
   ```

   Fix: rotate per-frame-in-flight resources (3 uniform buffers + 3 descriptor
   sets, indexed by `frames % 3`), i.e. keep the CPU from touching a buffer that
   is inside `previous_frame_end`. This is why the production renderer keeps a
   ring of uniform buffers and vertex buffers.
2. `Device::new` returns `(Arc<Device>, impl ExactSizeIterator<Item = Arc<Queue>>)`.
   Queues are not a `Vec`; call `.next()`.
3. Builder method mixing: safe calls return `Result<&mut Self, Box<ValidationError>>`
   (use `?`), `unsafe` calls like `draw()` return `&mut Self` directly.
4. `set_viewport`/`set_scissor` take `SmallVec<[Viewport; 2]>`:
   `[vp].into_iter().collect()` infers it; no `smallvec` dependency needed.
5. `ViewportState::default()` is one full-size dynamic viewport + one full-screen
   scissor; add `DynamicState::Viewport` + `DynamicState::Scissor` and set both
   per frame. `Scissor::default()` is `i32::MAX` (whole framebuffer), not zero.
6. `DescriptorSet::new(allocator_arc, layout_arc, writes, copies)` replaced
   `PersistentDescriptorSet` in 0.35. The allocator argument is
   `Arc<dyn DescriptorSetAllocator>` (pass the `Arc<StandardDescriptorSetAllocator>`).
   `WriteDescriptorSet::buffer(0, subbuffer)` and
   `WriteDescriptorSet::image_view_sampler(1, view, sampler)` are the shortcuts.
7. Raw SPIR-V loading is `ShaderModule::new` (unsafe) with
   `ShaderModuleCreateInfo::new(&words)`; `bytes_to_words` returns `Cow<[u32]>`.
   `ShaderModule::from_bytes`/`from_words` exist but are deprecated.
8. `Subpass::from(render_pass, 0)` returns `Option`; pass it into
   `GraphicsPipelineCreateInfo::subpass` via `.into()`.
9. sRGB: with a `*_SRGB` render target the fixed-function blend runs in **linear**
   space and the store encodes to sRGB. Fragment shaders output **linear**
   colours. Clear values are also linear: the probe's clear
   `[0.02, 0.03, 0.07, 1.0]` landed in the PNG as bytes `(40, 48, 75)` —
   i.e. `srgb_encode(0.02..0.07)`, **not** raw 0.02*255=5. Do not pre-encode
   CPU colours, but *do* convert `PaintList::clear` (sRGB u8) to linear before
   handing it to the clear value.
10. Sampling: create textures with an `R8G8B8A8_SRGB` format and copy the raw
    sRGB-encoded RGBA8 bytes with `copy_buffer_to_image`; the sampler decodes to
    linear automatically. A linear-filtered 8x8 checker stretched over a quad
    produced the expected linear-space blend (`173,130,173` for the average of
    orange and blue), proving the pipeline is colour-correct.
11. Image upload path: `Buffer::from_iter(..., BufferUsage::TRANSFER_SRC, host-visible, bytes)`
    then `builder.copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(staging, image))`.
    Layout transitions/barriers are automatic in the auto command buffer builder.
12. Offscreen readback: image with `ImageUsage::COLOR_ATTACHMENT | TRANSFER_SRC`,
    then `copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(image, host_buffer))`,
    submit, `wait(None)`, read the buffer. `image::RgbaImage::from_raw(w, h, bytes)`
    then `.save(path)` gives a valid PNG. Use `Format::R8G8B8A8_SRGB` so the
    bytes in the PNG are directly sRGB-encoded.
13. `image` crate 0.25: `RgbaImage::from_raw(w,h,Vec<u8>)` + `.save(path)`;
    features needed are just `png` (+ `jpeg` for cover art loading elsewhere).
14. `vulkano::swapchain::acquire_next_image(swapchain, timeout)` is the free
    function to use; it returns `(index, is_suboptimal, SwapchainAcquireFuture)`
    and the future must be joined into the frame future.
15. `khr_swapchain` must be enabled on the device; `khr_surface` +
    `khr_win32_surface` on the instance. Forgetting either fails device creation
    or `Swapchain::new` validation.
16. `Surface::from_window(instance, Arc<Window>)` requires the window to be
    `HasWindowHandle + HasDisplayHandle + Any + Send + Sync`; winit's `Window`
    satisfies all. The Surface stores the `Arc`, so keep cloning it for the
    window's lifetime instead of dropping it.
17. `Framebuffer::new` derives its extent from the attachments; set
    `FramebufferCreateInfo::extent` only if you want validation to check it
    against your own size.
18. Render-pass attachments are `Vec<Arc<ImageView>>`; one framebuffer per
    swapchain image, rebuilt after every recreation.
19. `vulkano::format::Format` has no `A2B10G10R10_SRGB_PACK32` (it is
    `A2B10G10R10_UNORM_PACK32` with a separate `_SRGB` image view format).
    Only use format variants that actually exist in 0.35.
20. On this machine `surface_formats` lists UNORM variants first; pick the sRGB
    one explicitly (verified list in `proof/window.log`).
21. (Historical probe note) `cargo`/`rustup` honour the repo
    `rust-toolchain.toml` inside `scratch/` too (it is a child directory), so the
    probe built with Rust 1.88.0.
22. (Historical probe note) Give a scratch probe its own `CARGO_TARGET_DIR`
    (`scratch/vulkan-probe/target`) so it never waits on a lock of the main
    workspace's `target/` while other agents build.
23. In the screenshot harness, `CopyFromScreen` works from the agent's
    PowerShell process; capture the rect at the window's `outer_position()` with
    its `inner_size()` (with `decorations(false)` they coincide). Also capture
    from within the real desktop session, not a service session.
24. **Vertex inputs are matched by NAME, not by location.** vulkano 0.35's
    `VertexDefinition for VertexBufferDescription` iterates the shader's input
    variables, looks up `buffer.members.get(variable_name)`, and errors with
    *"the shader interface contains an input variable named \"in_pos\" … but no
    such attribute exists in the vertex definition"* if the names differ:
    locations are then assigned from the struct layout, not compared. So each
    `layout(location = N) in ...` in the GLSL must be named exactly like the
    `#[derive(Vertex)]` struct field (or the field gets a
    `#[name("shader_name")]` attribute). The probe passed by luck because
    its fields and GLSL variables happened to share names (`pos`, `uv`,
    `color`); the production shader initially used `in_pos`, `in_uv`, … and
    failed once the real pipeline was built.
25. **Vulkan clip space is y-down.** NDC `y = -1` is the *top* of the
    framebuffer and `+1` the bottom (the opposite of OpenGL). Since the Rift UI
    is also y-down/top-left, the ortho mapping is a plain scale/shift:
    `ndc.y = pos.y / viewport.y * 2 - 1`. The first version used the OpenGL
    formula (`1 - pos.y/H*2`) and rendered the scene upside down — caught by
    the pixel smoke tests (a clip layer's scissor and the mirrored geometry no
    longer overlapped, so the layer vanished).
26. `Renderer` smoke tests: while the rest of the app was still being written, a
    scratch harness (`scratch/gfx-check`, since removed) included
    `crates/rift-desktop/src/gfx/mod.rs` via `#[path]` so the renderer's GPU
    tests could run before the app crate compiled. Today they run headless with
    `cargo test -p rift-desktop gfx`; `RIFT_VK_VALIDATION=1` + that command is
    validation-clean (only two harmless loader warnings about duplicate
    `VK_LAYER_*` JSONs installed by the AMD driver and OBS).

## Minimal working skeleton (condensed from the probe)

```rust
// Cargo.toml: vulkano 0.35.2 (default-features=false, features=["macros"]),
// winit 0.30, raw-window-handle 0.6, image 0.25 (png).

#[repr(C)]
#[derive(Clone, Copy, vulkano::buffer::BufferContents, vulkano::pipeline::graphics::vertex_input::Vertex)]
struct Vertex2D {
    #[format(R32G32_SFLOAT)] pos: [f32; 2],
    #[format(R32G32_SFLOAT)] uv: [f32; 2],
    #[format(R32G32B32A32_SFLOAT)] color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, vulkano::buffer::BufferContents)]
struct ViewportUbo { viewport: [f32; 2], _padding: [f32; 2] } // std140: vec2 -> 16 bytes

fn render_frame(
    device: &Arc<Device>, queue: &Arc<Queue>,
    swapchain: &Arc<Swapchain>, framebuffers: &[Arc<Framebuffer>],
    pipeline: &Arc<GraphicsPipeline>, layout: &Arc<PipelineLayout>,
    vertex_buffer: &Subbuffer<[Vertex2D]>,
    frames_data: &[(Subbuffer<ViewportUbo>, Arc<DescriptorSet>)],
    previous_frame_end: &mut Option<Box<dyn GpuFuture>>,
    frame_index: u64, size: [u32; 2],
) -> Result<(), Box<dyn Error>> {
    previous_frame_end.as_mut().unwrap().cleanup_finished();
    let (image_index, _suboptimal, acquire_future) =
        vulkano::swapchain::acquire_next_image(swapchain.clone(), None).map_err(Validated::unwrap)?;

    let slot = (frame_index as usize) % frames_data.len();     // ring buffer, see gotcha 1
    frames_data[slot].0.write()?.viewport = [size[0] as f32, size[1] as f32];

    let mut builder = AutoCommandBufferBuilder::primary(
        /* command_buffer_allocator */ unreachable!(), queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit)?;
    builder.begin_render_pass(/* clear + framebuffer */ unreachable!(), /* inline subpass */ unreachable!())?;
    builder
        .set_viewport(0, [Viewport { offset: [0.0, 0.0], extent: [size[0] as f32, size[1] as f32], depth_range: 0.0..=1.0 }].into_iter().collect())?
        .set_scissor(0, [Scissor { offset: [0, 0], extent: size }].into_iter().collect())?
        .bind_pipeline_graphics(pipeline.clone())?
        .bind_vertex_buffers(0, vertex_buffer.clone())?
        .bind_descriptor_sets(PipelineBindPoint::Graphics, layout.clone(), 0, frames_data[slot].1.clone())?;
    unsafe { builder.draw(vertex_buffer.len() as u32, 1, 0, 0)? };
    builder.end_render_pass(Default::default())?;
    let cb = builder.build()?;

    let future = previous_frame_end.take().unwrap()
        .join(acquire_future)
        .then_execute(queue.clone(), cb)?
        .then_swapchain_present(queue.clone(), SwapchainPresentInfo::swapchain_image_index(swapchain.clone(), image_index))
        .then_signal_fence_and_flush();
    match future.map_err(Validated::unwrap) {
        Ok(f) => *previous_frame_end = Some(f.boxed()),
        Err(VulkanError::OutOfDate) => *previous_frame_end = Some(sync::now(device.clone()).boxed()),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
```

The full working version (instance/device/surface/swapchain/render pass/
pipeline/textures/offscreen readback) was the probe's
`scratch/vulkan-probe/src/main.rs` — historical, removed with the scratch space
and not shipped; the production version of all of it is
`crates/rift-desktop/src/gfx/`.
