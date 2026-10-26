# Rift architecture and contracts

This documents the architecture of the **native Vulkan-rendered Rust
application** with a **hand-written immediate-mode GUI**, built on the
[`vulkano`](https://crates.io/crates/vulkano) crate.

Nothing in this app uses a webview, a retained-mode widget tree, or an external
GUI framework. Every pixel is produced by our own immediate-mode UI layer and
rasterised by our own Vulkano renderer. The screen-by-screen specification is
[`ui-spec.md`](ui-spec.md).

## Workspace layout

```
Cargo.toml                workspace root (members under crates/)
crates/
  rift-types/             rift-types       shared data models
  rift-ytmusic/           rift-ytmusic     YouTube Music innertube client
  rift-discord/           rift-discord     in-repo Discord Rich Presence IPC client
  rift/                   rift             native backend library (audio, player, library,
                                           downloads, settings, native streaming, media
                                           keys, Discord RPC, event bus)
  rift-desktop/           rift-desktop     THE APP: winit + vulkano + immediate-mode UI
    assets/fonts/                          embedded Inter + Material Icons (permissive licences)
    assets/icons/                          app icon set; icon.ico is the embedded file icon
    assets/logo/                           resolved logo SVG + prerasterized PNG
    shaders/                               GLSL sources + committed .spv binaries
docs/                     developer documentation
tools/                    local CI gate + screenshot / compare scripts
```

## Module layout

| Path | Responsibility |
|---|---|
| `crates/rift-desktop/src/main.rs`, `crates/rift-desktop/src/app.rs`, `crates/rift-desktop/src/theme.rs`, `crates/rift-desktop/src/state.rs`, `crates/rift-desktop/src/controller.rs`, `crates/rift-desktop/src/preview.rs` | entry point, winit event loop, root frame composition, app/UI state model, backend bridge |
| `crates/rift-desktop/src/gfx/**`, `crates/rift-desktop/shaders/**` | Vulkano renderer, swapchain, pipelines, textures, screenshot mode |
| `crates/rift-desktop/src/ui/**`, `crates/rift-desktop/src/text/**` | immediate-mode framework, layout, input, widgets, fonts/atlas, icons |
| `crates/rift-desktop/src/screens/**` | application screens |
| `crates/rift/**` | native command API, event bus |

`crates/rift-desktop/src/gfx/paint.rs` is the frozen interface between the two halves of the UI
stack: **UI produces `PaintList`, renderer consumes `PaintList`.**

## Rendering model (immediate mode)

One frame:

1. `main.rs` translates winit events into `ui::InputState` (mouse pos/buttons,
   wheel, key presses, text, modifiers, time delta).
2. `state.rs` drains backend events (`controller.poll_events()`) and applies them
   to `State`.
3. `app.rs` calls `ui.begin_frame(input, viewport)`, then runs the root screen
   (`screens::root`) which lays the UI out **from scratch this frame** and
   returns an `Vec<Action>`.
4. `ui.end_frame()` returns a `gfx::paint::PaintList` (layers + primitives +
   texture uploads).
5. `gfx::Renderer::frame(&paint)` uploads textures, records one command buffer,
   draws the swapchain image, presents.
6. `app.rs` applies the returned actions (navigate, play, search, open menu, …)
   through `controller`; state changes are visible on the next frame.

There is no retained widget DOM: widgets are `fn(&mut Ui, &mut State) -> Response`
style calls that paint and hit-test the same rectangle in the same pass.
Persistent UI state that must survive frames (scroll offsets, text-input
contents, popup identities) lives in `State` (or a small `UiMemory` owned by
`Ui` and keyed by stable hashed ids). Everything else is recomputed.

### Coordinates and DPI

* Layout is in **logical px**, origin top-left, `+y` down.
* `Renderer` knows the window's scale factor; it multiplies logical positions by
  the scale factor when writing vertices and sets the viewport to the physical
  size. The UI's viewport is `physical_size / scale_factor`.
* The reference screenshot is rendered at `scale_factor = 1.0`, 1160x740, so
  numbers from `styles.css` map 1:1.
* Windows on this machine report 96 DPI (100% scaling), but never hard-code it.

### Paint contract (summary)

See `crates/rift-desktop/src/gfx/paint.rs` for the authoritative definitions:

* `Rect { x, y, w, h }`, `Color { r,g,b,a }` (sRGB, straight alpha).
* `Prim::Rect { rect, color, radius, border, border_color }` — solid rounded
  rectangles, antialiased via a signed-distance field in the fragment shader.
* `Prim::Image { rect, texture, uv, tint, radius }` — textured (and rounded)
  quads: cover art, glyphs from the font atlas, icons.
* `Layer { clip: Option<Rect>, prims }` — painter-ordered; clip becomes a
  Vulkan scissor rectangle.
* `TextureId` is allocated by the UI (monotonic); `0 = NO_TEXTURE`.
  The UI reports `PaintList::created` / `updated` / `dropped`; the renderer owns
  the `VkImage`s, keeps a `HashMap<TextureId, GpuTexture>`, and reclaims after
  the frame. Cover art and the font atlas use exactly this path.
* `PaintList::clear` is the attachment clear color.

Text is drawn as one `Prim::Image` per glyph from a white-glyph alpha atlas,
tinted per `TextStyle`. Icons are glyphs from the embedded Material Icons fonts
with the same mechanism.

### Renderer API (contract)

```rust
// crates/rift-desktop/src/gfx/mod.rs
pub struct Renderer { /* ... */ }
impl Renderer {
    pub fn new(window: std::sync::Arc<winit::Window>) -> anyhow::Result<Self>;
    /// Headless target for `--screenshot`; no window or swapchain required.
    pub fn new_headless(width: u32, height: u32, scale_factor: f32) -> anyhow::Result<Self>;
    pub fn window_size(&self) -> (u32, u32);       // physical px
    pub fn logical_size(&self) -> (f32, f32);      // layout size: physical / scale
    pub fn scale_factor(&self) -> f32;
    pub fn resize(&mut self, physical: (u32, u32));
    pub fn frame(&mut self, paint: &PaintList) -> anyhow::Result<()>; // presents
    pub fn wait_idle(&mut self);
}
/// Render one frame offscreen to a PNG (used by `--screenshot` and tests).
pub fn screenshot(
    width: u32,
    height: u32,
    scale_factor: f32,
    paint: &PaintList,
    path: &std::path::Path,
) -> anyhow::Result<()>;
```

The renderer is allowed to add internals, but these entry points are consumed by
`app.rs`/`main.rs`, so changes need a Lead ping.

### Vulkan practices

The renderer follows the standard Vulkan contract rather than only "whatever the
driver accepts":

* **Validation layer in debug.** Debug builds enable `VK_LAYER_KHRONOS_validation`
  with a `tracing`-routed debug messenger when the layer is installed
  (`RIFT_VK_VALIDATION=0` opts out; `=1` forces it in release). The layer is
  never a hard dependency: without it the renderer starts normally. All GPU
  tests and the `--screenshot` path run under validation in debug builds.
* **Device selection.** Instance extensions come from the window surface; devices
  without `VK_KHR_swapchain` or a queue family that supports both graphics and
  presentation are rejected, and the chosen device/API version is logged.
* **Swapchain.** sRGB surface format preferred (with a warning on the non-sRGB
  fallback), explicit `Fifo` present mode (a music player should not burn GPU
  time to shave latency), `Opaque` composite alpha when supported, one image more
  than the driver minimum (capped by `max_image_count`), and the requested extent
  clamped into `minImageExtent..=maxImageExtent` / a fixed `currentExtent` on both
  creation and recreation. `OutOfDate`/`SurfaceLost` recreate the swapchain and
  drop the frame; `DeviceLost` is reported as an error (there is no device
  recreation path — the app requires a restart).
* **Synchronization & lifetime.** Three frames in flight via one `GpuFuture`
  chain; substituted buffers are retired by frame number and only freed after the
  fence signals; `wait_idle` runs before resize/drop (and in `Drop`), so no object
  is destroyed while in flight. The `Surface` is kept alive alongside the window
  it was created from. Textures follow the `PaintList` created/updated/dropped
  contract, with descriptor-set rebuild on a surface-format change.
* **Allocators.** Vulkano's standard memory/command-buffer/descriptor-set
  allocators; per-frame vertex and uniform data live in persistent host-visible
  ring buffers (streaming data that is rewritten every frame) instead of a
  staging round trip. Texture uploads are batched into one command buffer per
  frame.

Deliberate gaps, kept because their cost/benefit does not justify them here: no
persistent `VkPipelineCache` (drivers keep their own on-disk shader cache; the
warm pipeline costs ~17 ms), no device-lost recovery, and the UI repaints every
frame (immediate-mode animations) rather than idling when nothing changes.

### Immediate-mode UI API (contract)

`docs/imgui-api.md` (published by `imgui`) is the detailed
reference; the shape `app.rs` depends on:

```rust
let mut ui = ui::Ui::new();
// per frame:
ui.begin_frame(input, viewport);
//   ... screens draw here ...
let paint: PaintList = ui.end_frame();
```

`InputState` is winit-agnostic (defined in `crates/rift-desktop/src/ui/input.rs`):
`mouse_pos`, button state + pressed edges, scroll delta, modifiers, a queue of
key events (key, text, repeat), pointer position, viewport, scale factor,
`time`/`delta_time`. `main.rs` fills it from winit events; `app.rs` forwards it.

## State and actions

`crates/rift-desktop/src/state.rs` owns everything the UI renders (a port of the
previous web app's `use_state` set): library, queue, playback state, current
track, progress, volume, settings, downloads, update banner, network status,
current `View`, search state/results, artist/album pages, queue panel
open/collapsed, active dialog/menu, toasts, scroll offsets, cover-art texture
cache.

Screens never call the backend directly. They mutate nothing outside their local
`Ui` id-space; instead they push `Action` values (navigation, playback, library,
settings, download, window control, …) into a `Vec<Action>`. `app.rs` drains the
vec after `ui.end_frame()` and calls `controller.dispatch(action)`, which either
applies UI-local state immediately (navigation, dialog open) or hands an async
command to the backend. Backend events come back through
`controller.poll_events()` and update `State` for the next frame.

## Backend bridge

`crates/rift/` keeps all existing Rift logic — audio thread, player core, library
persistence (durable `library.json`/`settings.json` writes are queued to a
background writer and flushed on `RiftBackend::shutdown`/Drop), ytmusic client,
native audio streaming, downloads, settings, Discord RPC, media keys — and
replaces the old shell's plumbing with:

* a native `RiftBackend` (or `AppState`) constructed once by `app.rs`;
* one channel of `BackendEvent` values (track/state/queue/progress/library/
  downloads/error/notice/open-playlist) drained per frame;
* plain methods for the former IPC command functions.

See `docs/backend-api.md` for exact signatures. The app crate
depends on it through the `backend` cargo feature (enabled by default), so
`--no-default-features` builds the UI/renderer without the platform stack.

## Windowing (matches the old window)

From the old window configuration (removed with the legacy shell): title
"Rift", 1160x740, centered, minimum 940x620, `decorations: false`, resizable.
Therefore:

* winit window: `decorations: false`, `resizable: true`, min inner size
  `(940, 620)`, initial inner size `(1160, 740)`, title "Rift";
* the app draws its own 46 px titlebar (logo, search box, window controls);
* dragging the titlebar calls `Window::drag_window()`, double-click toggles
  maximize, the three buttons call `set_minimized`, maximize/restore, and close
  (exit event loop); edge/corner resize uses `Window::drag_resize_window(...)`
  plus 4-6 px invisible hit zones.

### Window icon vs executable file icon

The app ships two icons, and they are set in different places:

* **Runtime window icon** — `main.rs::window_icon()` loads
  `crates/rift-desktop/assets/icons/128x128.png` and hands it to
  `Window::set_window_icon`. It applies only while the app is running: the
  window and its taskbar button.
* **Executable file icon** — the icon Explorer, the Start menu, shortcuts and
  "pin to taskbar" draw is a Windows PE resource (`RT_GROUP_ICON` plus its
  `RT_ICON` images) inside `rift-music.exe`. The shell reads it from the file
  without starting the app, so `window_icon()` cannot supply it. It is embedded
  by `crates/rift-desktop/build.rs`, which compiles
  `crates/rift-desktop/assets/icons/rift.rc` (`1 ICON "icon.ico"`) through the
  Windows SDK resource compiler (`rc.exe`) using the Windows-only
  `embed-resource` build dependency
  (`[target.'cfg(windows)'.build-dependencies]`; other hosts have no resource
  compiler and must not try to run one). `build.rs` also reruns when the
  `.rc`/`.ico` files change and uses `manifest_required()`, so a missing
  resource compiler fails the Windows build instead of silently shipping an
  icon-less executable.
* App icon assets live under `crates/rift-desktop/assets/icons/` — the crate
  that owns the only binary now owns its whole icon set (the native rewrite had
  parked the old shell's icons in `crates/rift/icons/`). `icon.ico` is the
  file-icon source, and `128x128.png`, `icon.png`, the `Square*Logo`/
  `StoreLogo` set and `icon.icns` are the same mark in the other sizes/formats
  the platforms use.
* The resource is checked, not assumed. The Windows leg of the CI `build`
  matrix runs `.github/scripts/check-windows-icon.ps1` on
  `target/release/rift-music.exe` after the release build, and
  `tools/ci-local.ps1` runs the same check locally (skipped with the rest of
  the release steps under `-SkipRelease`). The script parses the PE resource
  directory itself, with no SDK tooling.

The guard exists because the failure is silent: the previous desktop shell
embedded `icon.ico` as part of its build step, the native rewrite replaced that
step with a plain `cargo build`, and the executable shipped with the generic
Windows application icon. Nothing failed — not the compile, not the tests, not
the app — so the resource is now checked explicitly.

## Preview / screenshot mode

Development builds support deterministic placeholder data for UI work and
screenshots. The mode is kept out of shipped release binaries: the `preview`
module and the `--preview`/`--screenshot` CLI are compiled into debug builds
automatically and into release builds only with the explicit, non-default
`preview` feature. A normal `cargo build --release -p rift-desktop` rejects the
dev flags at argument parsing.

```powershell
# one-shot offscreen render, no window, deterministic (debug build):
cargo run -p rift-desktop -- --preview --screenshot target/preview.png --size 1160x740
# interactive window with placeholder data:
$env:RIFT_PREVIEW = "1"; cargo run -p rift-desktop
# release-profile capture (maintainers; dev-only feature):
cargo run -p rift-desktop --release --features preview -- --preview --screenshot target/release-preview.png
```

`crates/rift-desktop/src/preview.rs` contains the placeholder data
(`preview_track(n)`, `preview_library()`, `preview_queue()`); the preview library
uses neutral placeholder titles, artists and playlists.

For capturing the native-streaming download UI without a backend or network,
`RIFT_PREVIEW_DOWNLOAD=<permille>` (0–1000) marks two placeholder tracks as
in-flight in preview state: the second shows the determinate progress bar (plus
the in-flight spinner) at that fraction, the third shows the spinner only
(queued, no fraction). It is unset by default, so every existing reference
screenshot is unchanged. Example:

```powershell
$env:RIFT_PREVIEW_DOWNLOAD = "640"
cargo run -p rift-desktop -- --preview --screenshot artifacts/native-stream/download-bar-liked.png --view liked --size 1160x740
```

That is how `artifacts/native-stream/download-bar-liked.png` (the acceptance
capture for the download bar) was produced.

## Build & run

```powershell
cargo run -p rift-desktop                       # windowed app (live backend by default)
cargo run -p rift-desktop --release
cargo test -p rift-desktop                      # pure logic tests (layout, state, paint)
cargo check --workspace
cargo test --workspace
```

The app is built entirely with Cargo: `cargo build` is the whole build.

## Startup path and timing

`main()` records a monotonic clock as its first statement (before logging init) and
the startup path emits one `startup:` log line per milestone: winit event loop and
window, Vulkan instance/device, surface/pipeline, swapchain, texture manager, ring
buffers, backend construction (data directory, library load, downloads scan,
settings, thread starts, TLS client, runtime), UI font parsing and icon prewarm,
the bootstrap snapshot — then `first frame presented +N ms` (the frame is in the
swapchain) and `window shown (painted) +N ms`, which is when a launched app
actually becomes visible. Sub-milestones are `debug!` (the default filter already
enables debug for `rift`/`rift_desktop`); the window-created, first-frame and
window-shown lines are `info!`.

Read the timeline with the default log filter, or narrow it:

```powershell
$env:RUST_LOG = "rift=debug,rift_desktop=debug"; cargo run -p rift-desktop --release
```

Startup is split into three separately-measured parts, because they have
different causes:

1. **OS/cargo overhead before `main`.** The in-app clock starts in `main`, so the
   logs exclude process creation, the Windows loader and (for `cargo run`)
   Cargo itself. Measured on the reference machine: ~10–20 ms for the stripped
   release exe, ~300 ms for the unoptimised debug exe (loader + AV scan of a much
   larger file), and ~300 ms extra for `cargo run`'s freshness check in the big
   target directory.
2. **In-app startup** (the `startup:` timeline). Reference release build, warm
   driver/file caches: ~200–220 ms to the first painted frame — Vulkan
   instance/device ~75–85 ms, swapchain ~55 ms, winit window ~30 ms, white
   fallback texture ~28 ms (first host-visible memory block + upload round trip),
   pipeline ~17 ms (cached runs ~1 ms), fonts ~15 ms (hidden under the renderer,
   see below), backend ~3 ms, first frame compose ~14 ms. The driver-bound share
   is the floor for this architecture.
3. **`cargo run` adds Cargo's own ~0.3 s startup** on top; running the built
   binary (or `cargo run --release`, whose release exe is much cheaper to load)
   removes most of the difference.

Two app-side costs were worth fixing: the six embedded font faces used to be
parsed serially (~52 ms release, ~420 ms debug), and are now parsed on scoped
threads (release ~15 ms) and overlapped with Vulkan construction entirely; the
per-frame vertex ring buffers used to trigger a second device-memory block
allocation (~16 ms) and now share the host-visible streaming memory the uniform
ring already uses — a 300-frame benchmark showed identical frame times either
way.

**Windows reveal.** A visible winit window has no paint to present for the first
~200 ms, and Windows composites that as a bright white rectangle. The window is
therefore created hidden and only shown after the first frame is in the
swapchain (`HIDE_WINDOW_UNTIL_FIRST_FRAME` in `main.rs`), so it appears already
painted; the total time to content is unchanged and the white flash is gone. On
other platforms the window is shown immediately (the swapchain-on-hidden-window
behaviour is not ported without a way to verify it there).

**Dev profile.** Unoptimised debug executables are ~2.4× the release file
(42 MB before this; the loader/AV scan of that file alone cost several hundred
ms), and the startup CPU stages (fontdue, first-frame layout/rasterising) are
much slower unoptimised. `[profile.dev]` therefore uses line-tables-only debug
info (32 MB, backtrace/line info kept) and bumps `fontdue`/`rift-desktop` to a
low opt-level without rebuilding every dependency at a higher level. `cargo test`
also benefits.
