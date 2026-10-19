# Rift architecture and contracts

> **Status: executed.** This is the design the native rewrite was built against;
> it describes the completed tree (the old web frontend and desktop-shell
> backend no longer exist). For the outcome, measurements and acceptance
> evidence see [`migration-report.md`](migration-report.md) and
> [`screenshot-gallery.md`](screenshot-gallery.md).

This directory documents the design of the **native Vulkan-rendered Rust
application** with a **hand-written immediate-mode GUI**, built on the
[`vulkano`](https://crates.io/crates/vulkano) crate.

Nothing in this app uses a webview, a retained-mode widget tree, or an external
GUI framework. Every pixel is produced by our own immediate-mode UI layer and
rasterised by our own Vulkano renderer. The visual target is the old UI
pixel-for-pixel: the archived reference screenshot is
[`reference-old-ui.png`](reference-old-ui.png) at 1160x740, and the extracted
design source of truth is [`ui-spec.md`](ui-spec.md) (derived from the removed
stylesheet and web components, which remain available in git history).

## Workspace layout

```
Cargo.toml                workspace root (members under crates/)
crates/
  rift-types/             rift-types       shared data models
  rift-ytmusic/           rift-ytmusic     YouTube Music innertube client
  rift/                   rift             native backend library (audio, player, library,
                                           downloads, settings, yt-dlp, media keys, Discord
                                           RPC, event bus)
  rift-desktop/           rift-desktop     THE APP: winit + vulkano + immediate-mode UI
    assets/fonts/                          embedded Inter + Material Icons (permissive licences)
    assets/logo/                           resolved logo SVG + prerasterized PNG
    shaders/                               GLSL sources + committed .spv binaries
docs/                     contracts, design notes, verification, screenshot gallery
tools/                    local CI gate + screenshot / compare scripts
```

The legacy web frontend and desktop-shell backend were deleted when the native
rewrite landed; nothing in the workspace depends on them.

## Module ownership (parallel work)

| Path | Owner | Responsibility |
|---|---|---|
| `crates/rift-desktop/src/main.rs`, `crates/rift-desktop/src/app.rs`, `crates/rift-desktop/src/theme.rs`, `crates/rift-desktop/src/state.rs`, `crates/rift-desktop/src/controller.rs`, `crates/rift-desktop/src/preview.rs` | Lead | entry point, winit event loop, root frame composition, app/UI state model, backend bridge |
| `crates/rift-desktop/src/gfx/**`, `crates/rift-desktop/shaders/**` | `renderer` | Vulkano renderer, swapchain, pipelines, textures, screenshot mode |
| `crates/rift-desktop/src/ui/**`, `crates/rift-desktop/src/text/**` | `imgui` | immediate-mode framework, layout, input, widgets, fonts/atlas, icons |
| `crates/rift-desktop/src/screens/**` | `screens-chrome` + `screens-pages` | the screens themselves, matching the old UI |
| `crates/rift/**` | `backend` | native command API, event bus |

`crates/rift-desktop/src/gfx/paint.rs` is the frozen interface between the two halves of the UI
stack: **UI produces `PaintList`, renderer consumes `PaintList`.** Do not change
it without pinging the Lead.

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

### Immediate-mode UI API (contract)

`docs/vulkan-migration/imgui-api.md` (published by `imgui`) is the detailed
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
downloads, settings, Discord RPC, media keys — and replaces the old shell's
plumbing with:

* a native `RiftBackend` (or `AppState`) constructed once by `app.rs`;
* one channel of `BackendEvent` values (track/state/queue/progress/library/
  downloads/error/notice/open-playlist) drained per frame;
* plain methods for the former IPC command functions.

See `docs/vulkan-migration/backend-api.md` for exact signatures. The app crate
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
  plus 4-6 px invisible hit zones;
* window icon from `crates/rift-desktop/assets/icons/128x128.png` (embedded at
  build time).

## Preview / screenshot mode (the visual verification loop)

The old frontend had a debug-only `RIFT_PREVIEW=1` mode: neutral placeholder
data (Song Title N / Artist Name / Playlist N) rendered instead of the personal
library. The archived old-UI capture (`reference-old-ui.png`) and the current
README preview (`Previews/image.png`) were both produced this way. The new app
supports the exact same idea — and keeps it out of the shipped release binary:
the `preview` module and the `--preview`/`--screenshot` CLI are compiled into
debug builds automatically and into release builds only with the explicit,
non-default `preview` feature (the local acceptance gate enables it for its
screenshot binary). A normal `cargo build --release -p rift-desktop` rejects
those flags at argument parsing.

```powershell
# one-shot offscreen render, no window, deterministic (debug build):
cargo run -p rift-desktop -- --preview --screenshot target/preview.png --size 1160x740
# interactive window with placeholder data:
$env:RIFT_PREVIEW = "1"; cargo run -p rift-desktop
# release-profile acceptance capture (maintainers; dev-only feature):
cargo run -p rift-desktop --release --features preview -- --screenshot target/release-preview.png
```

`crates/rift-desktop/src/preview.rs` contains the ported placeholder data
(`preview_track(n)`, `preview_library()`, `preview_queue()`); the current track is
`Song Title 1`, progress 83/214 s, playback state Playing, and the layout is the
one in `reference-old-ui.png` / `Previews/image.png`.

Acceptance: `--screenshot` output compared side by side against the archived
old-UI capture (`reference-old-ui.png`) at 1160x740 (titlebar, sidebar, library
cards, recently played, queue panel, player bar); the committed gallery lives in
`screenshot-gallery.md`.

## Build & run

```powershell
cargo run -p rift-desktop                       # windowed app (live backend by default)
cargo run -p rift-desktop --release
cargo test -p rift-desktop                      # pure logic tests (layout, state, paint)
cargo check --workspace
cargo test --workspace
```

The old app's build steps (web bundler, desktop-shell CLI) are removed from the
README/CI as part of the native rewrite. `cargo build` is the whole build.

## Milestones

1. **M1 – renderer probe**: winit + vulkano window draws a triangle; offscreen
   PNG works. (renderer)
2. **M2 – paint pipeline**: PaintList renders rects/images with SDF rounded
   corners, text atlas, clip layers, resize + swapchain recreation. (renderer)
3. **M3 – IMGUI core**: input, layout, text, widgets, popups, scroll; screenshot
   at 1160x740 renders the dark shell. (imgui)
4. **M4 – backend port**: `cargo check -p rift`, no legacy webview/shell
   crates, event bus and native methods documented. (backend)
5. **M5 – chrome/home**: titlebar, sidebar, home cards/recently played, player
   bar, queue panel; preview screenshot matches `reference-old-ui.png`.
   (screens-chrome)
6. **M6 – remaining screens**: search (songs/artists/albums), artist, album,
   playlist, liked, settings, menus, dialogs, toasts. (screens-pages)
7. **M7 – wiring**: state/controller integration, real backend data, window
   controls, keyboard shortcuts, cover-art loading. (Lead + controller)
8. **M8 – cleanup**: remove the old web frontend, remove the legacy shell's
   config/build files/deps from Cargo.lock, README + CI + docs updated, release
   build, final screenshots. (Lead)

All milestones are complete; see [`migration-report.md`](migration-report.md) for
the final measurements and acceptance evidence, and
[`screenshot-gallery.md`](screenshot-gallery.md) for the captured screens.
