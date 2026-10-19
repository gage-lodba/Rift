# Renderer acceptance verification (task-9)

> **Evidence retention.** The acceptance runs recorded below used throwaway
> harnesses and raw captures in the gitignored `scratch/` and `artifacts/`
> directories during the conversion. **Those harnesses and raw artifacts are not
> shipped** — `scratch/` was removed and `artifacts/` is scratch space — so the
> measured tables and numbers in this document are kept as historical acceptance
> evidence, not as runnable commands. What survives a clean checkout is committed
> under [`screenshots/`](screenshots/) and
> [`evidence/final-gate.log`](evidence/final-gate.log).
>
> Current-tree reproduction uses only shipped tooling — `tools/verify.ps1`,
> `tools/ci-local.ps1`, `tools/screenshot.ps1`, `tools/window_shot.ps1` and the
> `--preview --screenshot` CLI. See
> [Reproduce (current tree)](#reproduce-current-tree) at the end.

Final acceptance pass for the Vulkano renderer (`crates/rift-desktop/src/gfx/`, `crates/rift-desktop/shaders/`)
on the reference machine: **Windows 11, AMD Radeon RX 7800 XT (proprietary
driver, Vulkan 1.3.281), 1160x740 window at 100 % DPI, Rust 1.88**. All numbers
below were measured, not estimated; each is backed by a capture/log from the
acceptance runs, with the surviving/curated evidence committed under
[`screenshots/`](screenshots/) and [`evidence/`](evidence/).

The probe/API recipe is [`vulkano-recipe.md`](vulkano-recipe.md); this file is the
acceptance report. The final-tree pass (final binary, default features,
pixel-identical reference screenshot, validation+resize windowed run) is
**§7**; it found no regressions.

## 1. Windowed live-app verification (runtime-created textures)

**Open question:** do runtime-created textures (cover art delivered by
`ControllerEvent::Cover`, ids >= 1024, uploaded through
`ui.upload_texture` → `PaintList::created` on one frame and referenced on later
frames) render in the windowed swapchain path?

**Answer: yes.** The earlier blank cover tiles were an *upload-lifetime* bug in
the UI layer, not the renderer: `Ui::begin_frame` used to clear the pending
texture-upload lists, discarding covers decoded between frames. imgui now
drains them in `end_frame`; the renderer uploads any `PaintList::created`
textures before building the frame's geometry and draws them in the same frame.

Evidence (three independent checks):

1. **Real live app** (`--features backend`, real user data, 12 s): the run was
   captured as `artifacts/screens/window-live.png` / `window-live3.png` (raw
   scratch artifacts, not shipped); the curated copy is
   [`screenshots/11-live-window-real-covers.png`](screenshots/11-live-window-real-covers.png).
   It shows real cover art in the library cards, "Recently played", the queue
   rows and the player bar — the tiles that used to be blank. Renderer
   diagnostics from that run:
   ```
   texture sync lines: 5; uploaded ids: 15
   cover ids (>=1024): 1024,1025,1026,1027,1028,1029,1030,1031,1032,1033,1034,1000000,1000001,1000002
   unknown-texture fallback warnings: 0
   gfx frame stats window=300 ... textures=15 created_total=15 dropped_total=0 updated_total=0
   ```
   (ids 1000000+ are the UI's app-texture id range; ids 1024–1034 are cover
   art; the five sync lines prove they arrived spread across several frames,
   not all at once.)
2. **Controlled windowed phase harness** (historical: `scratch/gfx-check`, run
   against the real swapchain, removed with the scratch space — the phases below
   are results, not a shipped script): the harness drove six ~3 s phases and
   captured the window at each, asserting exact pixels:
   | phase | scenario | result (historical capture) |
   |---|---|---|
   | 1 | no textures registered (dark tiles) | PASS `texture-live-phase1.png` |
   | 2 | texture 1024 created **and drawn the same frame** | PASS (red) `phase2.png` |
   | 3 | second texture 2048 created, both drawn | PASS (red+green) `phase3.png` |
   | 4 | 1024 replaced with yellow (same id) | PASS `phase4.png` |
   | 5 | 2048 dropped; prim references never-created id 424242 | PASS (white fallback, no crash) `phase5.png` |
   | 6 | window resized 800x600 → 900x620 mid-run | PASS (texture still renders) `phase6.png`, `texture-live-resized.png` |

   Run again with `-Validation`: same result, **zero validation-layer
   messages**, and the unknown-id fallback logged exactly once as a warning
   (by design, see below).
3. **Gfx unit test** (`cargo test -p rift-desktop gfx`):
   `multi_frame_runtime_texture_lifecycle` renders six frames through the same
   `TextureManager`/batching code: create-on-frame-N-and-draw-it, accumulate two
   created textures, replace pixels by id, large id (999_999), drop and keep
   drawing survivors, unknown-id fallback, and an update that changes the
   texture dimensions. Pixel assertions on every step; passes.

**Fallback policy (new):** a `Prim::Image` that references an id the renderer
has never seen is no longer silently skipped. It is drawn with the renderer's
1x1 white fallback (tinted like any image) so a lifetime bug is visible, and the
id is logged **once** — at warn level when `RIFT_VK_VALIDATION=1`, debug
otherwise. Live runs currently produce zero such warnings.

## 2. Resize / DPI / window-state robustness

Harness (historical: `scratch/gfx-check/proof/run-resize-window.ps1`, removed
with the scratch space): it used a backend build, preview data and a throwaway
`RIFT_DATA_DIR` so the real user `.window-state.json` was never touched. It
resized via Win32 `SetWindowPos`, captured each size, asserted anchors,
maximized/restored, closed with `WM_CLOSE`, and checked the persisted state file.
The raw PNG names in the table below are historical (not shipped). For a current
tree, take captures with `pwsh tools/window_shot.ps1 -Out artifacts/screens/window.png`
and resize interactively (there is no shipped programmatic-resize harness).

| scenario | requested | actual client | capture | result |
|---|---|---|---|---|
| minimum size | 940x620 | 940x620 | `artifacts/screens/window-resize-940x620.png` | PASS |
| large | 1600x900 | 1600x900 | `artifacts/screens/window-resize-1600x900.png` | PASS |
| tiny (below the app's 940x620 minimum, forced programmatically) | 320x200 | 320x200 | `artifacts/screens/window-resize-320x200.png` | PASS (no crash; see note) |
| maximized | work area | 2576x1408 | `artifacts/screens/window-maximized.png` | PASS |
| restored | previous size | 320x200 | `artifacts/screens/window-restored.png` | PASS |

* Anchors asserted on every capture: bottom-centred transport button, titlebar
  window controls at the top-right, and (>= 940 px wide) the sidebar "Home"
  pill. No stretched or misplaced UI, no renderer warnings, no stderr output,
  swapchain recreated on every size change (`window scale factor changed` /
  `swapchain recreation failed` log counters stayed 0).
* Exit code 0 after `WM_CLOSE`, and the throwaway data dir received
  `.window-state.json` with the expected shape (`width: 1160, height: 740,
  decorated: false, maximized: false, …`).
* **320x200 note:** that is far below the product minimum (940x620). The
  renderer does the right thing (correct clipping, painter order, no crash or
  stretch), but the *layout* legitimately overflows/clips at that size. The
  window manager honours the min-size constraint for user-driven resizes; the
  test forces it programmatically.
* **DPI:** this machine has a single 96 DPI monitor (`scale_factor = 1.0`), so a
  real cross-monitor DPI-change event cannot be produced. The renderer follows
  `window.scale_factor()` every frame (logs `window scale factor changed` and
  reprojects at the new scale), and the offscreen path takes an explicit
  `--scale` parameter. On the final tree, scaled offscreen renders were verified
  at 125 % and 150 % (`--scale 1.25` / `--scale 1.5`, physical 1450x925 /
  1740x1110); the committed copies are
  [`screenshots/09-home-hidpi-1.25x.png`](screenshots/09-home-hidpi-1.25x.png)
  and
  [`screenshots/10-home-hidpi-1.5x.png`](screenshots/10-home-hidpi-1.5x.png).
  Layout scales correctly and text stays crisp.

## 3. Performance (300-frame windows, preview window, 1160x740)

The renderer logs one stats window per 300 presented frames when
`RIFT_GFX_STATS=1` is set. The numbers below were collected by a historical
harness (`scratch/gfx-check/proof/run-perf.ps1`, removed with the scratch space);
to re-measure on the current tree run the preview window with `RIFT_GFX_STATS=1`
and read the `gfx frame stats` lines from the log, e.g.
`$env:RIFT_GFX_STATS="1"; cargo run -p rift-desktop -- --preview`. Frame time is
measured around `Renderer::frame`, i.e. it includes the FIFO acquire wait.

```
mean over 6 x 300-frame windows: 3.08 ms per frame
p95 per window: 3.45 - 3.74 ms      max per window: 3.88 - 37.84 ms (startup/resize spikes)
acquire wait: 1.18 - 1.52 ms        avg vertices/frame: 2718    avg draw calls/frame: 55
textures: 4, created_total: 4, dropped_total: 0, updated_total: 0   (stable across 1800 frames)
```

* **60 fps holds with large headroom**: the preview UI frame costs ~3 ms of the
  16.67 ms budget on this GPU. (The windowed present path on this machine is not
  refresh-throttled because the window is borderless/DWM-composited, so the
  measured rate is ~300-330 fps; the meaningful number is the 3 ms frame cost.)
* Live app (real backend, more prims/covers): avg 2.29–2.52 ms, p95 3.15–3.17 ms,
  3018 vertices / 47 draw calls per frame, 15 textures stable, 0 dropped.
* Memory/texture sanity: the per-frame vertex ring is 3 × capacity (80 B per
  vertex, grows from 16k to the next power of two when needed), the uniform ring
  is 3 × 16 B; live texture count stayed at 15 with `created_total` equal to
  `textures` and no drops/updates over 300+ frames — i.e. no per-frame texture
  or buffer churn. The synthetic 12.8k-prim stress test
  (`stress_10k_prims_many_clips_and_atlas_growth`) also shows vertex-ring growth
  and a 1024→2048 atlas replacement running without validation errors.

## 4. Validation layer

* `RIFT_VK_VALIDATION=1` is honoured by the renderer (Khronos validation layer +
  debug-utils callback routed through `tracing`).
* **Final result: zero validation messages** in all four contexts:
  * live window (real backend), 10 s → exit 0, 0 `VUID-` messages; the only
    stderr output is the audio backend's benign `Dropping DeviceSink` note;
  * controlled windowed multi-frame texture phases (create/update/drop/resize;
    historical harness, removed with `scratch/`) → 0 validation messages,
    unknown-id fallback logged once as a warning by design;
  * headless GPU test suite, `RIFT_VK_VALIDATION=1 cargo test -p rift-desktop gfx`
    → 9/9 pass, 0 validation messages;
  * 12.8k-prim stress test (mixed scissor layers, 2000 draw-call switches,
    1024→2048 atlas replacement) → 0 validation messages.

## 5. Remaining renderer limitations (not blocking acceptance)
1. **Synchronous texture uploads.** New/replaced textures are uploaded on the
   frame they appear and the CPU waits for that small transfer command buffer to
   finish (staging buffer → image + one fence). It is invisible for normal cover
   art (single-digit ms on first load, zero when nothing changes) but a
   dedicated transfer queue / staging ring would be the next step for very large
   images or high churn.
2. **Single graphics pipeline / single color target.** Rounded-rect SDF and
   textured quads share one pipeline; there is no depth, stencil or MSAA. That
   is sufficient for the 2D UI and keeps batching trivial, but popups/shadows
   rely on painter order and alpha blending only.
3. **Full-image texture replacement.** `PaintList::updated` replaces the whole
   texture (atlas growth 1024→2048 is a full re-upload). The UI only sends an
   update when the atlas actually changes, so steady state is zero traffic.
4. **DPI on the window follows `window.scale_factor()` every frame** (logs
   `window scale factor changed` and reprojects at the new scale); the offscreen
   path takes an explicit `--scale`. On the **final tree** scaled offscreen
   renders were captured at `--scale 1.25` (1450x925) and `--scale 1.5`
   (1740x1110); the committed copies are
   [`screenshots/09-home-hidpi-1.25x.png`](screenshots/09-home-hidpi-1.25x.png)
   and
   [`screenshots/10-home-hidpi-1.5x.png`](screenshots/10-home-hidpi-1.5x.png),
   with correct layout and crisp text. A live cross-monitor DPI-change event is
   still not producible on this single-monitor machine.
5. **Format-change edge case.** If a surface ever changed its swapchain format
   at runtime, the renderer rebuilds the pipeline and uniform ring (implemented,
   never observed on this driver).
6. **Minimized window** skips rendering (zero-size swapchains are invalid);
   restore is handled by the normal resize path.

## 6. CI hygiene (clippy `-D warnings`, rustfmt)

**Final tree: both are clean crate-wide.**

```powershell
cargo clippy -p rift-desktop --all-targets -- -D warnings   # exit 0
cargo fmt -p rift-desktop -- --check                        # exit 0
cargo test -p rift-desktop gfx                              # 9/9 pass
```

* The renderer-owned cleanups that got us here: `FrameStats::record` now takes a
  `FrameSample` struct (was 9 arguments), a needless `mut` and a `useless_vec`
  in the GPU tests were removed, and every `crates/rift-desktop/src/gfx/**` file (including the
  frozen `paint.rs`, whitespace only) is rustfmt-clean.
* Earlier in the conversion these gates were red because of other owners'
  screens/UI/controller files; on the final tree all of that is fixed and the
  whole `rift-desktop` package is warning-free.

## 7. FINAL-TREE acceptance (final binary, default features)

Final tree: `ui/` and `dist/` deleted, `Cargo.lock` pruned, `rift-desktop`'s
**default feature set includes `backend`**, everything green.

```powershell
cargo build -p rift-desktop
target\debug\rift-music.exe --preview --screenshot artifacts\screens\final-renderer-home.png --size 1160x740 --view home
```

| check | result |
|---|---|
| default-feature build | exit 0 |
| offscreen 1160x740 home screenshot | exit 0; the render is **pixel-identical** to `Previews/image.png` — `mean abs diff = 0.00`, `max 0`, `pixels>16 = 0.00%` (via `tools/compare_images.py`). The committed copy is [`screenshots/01-home.png`](screenshots/01-home.png). |
| windowed preview, 10 s, `RIFT_VK_VALIDATION=1` | **0 validation-layer messages**, 0 renderer failures, 0 unknown-texture fallbacks; full UI renders (titlebar, sidebar, cover tiles, recently played, queue, player bar). Captured during the run as `artifacts/screens/final-renderer-window-preview.png` (raw scratch artifact, not shipped). |
| resize to 1000x700 in the same live window | exact client size, graphics intact (sidebar fixed, content reflows, player bar anchored), no renderer warnings; raw capture `final-renderer-window-resized.png` (not shipped). |
| clean close | `WM_CLOSE` → exit code 0 |
| final-tree HiDPI | 1.25x / 1.5x scaled offscreen renders correct; committed as [`screenshots/09-home-hidpi-1.25x.png`](screenshots/09-home-hidpi-1.25x.png) (1450x925) and [`screenshots/10-home-hidpi-1.5x.png`](screenshots/10-home-hidpi-1.5x.png) (1740x1110) |

Harness (historical: `scratch/gfx-check/proof/run-final-window.ps1`, removed with
the scratch space; preview window, validation layer, capture → resize → capture →
close with a throwaway `RIFT_DATA_DIR`). **No regressions found** relative to the
task-9 acceptance runs above. Current-tree equivalents are in
[Reproduce (current tree)](#reproduce-current-tree).

## Reproduce (current tree)

The historical harnesses referenced above were scratch tools and are **not
shipped**. Everything below runs from a clean checkout using only the repo's own
tooling (`tools/`, the CLI, cargo).

```powershell
# Build the app; default features include the backend.
cargo build -p rift-desktop

# One offscreen frame (deterministic, no window, no network) + reference compare.
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home.png --size 1160x740 --view home
python tools/compare_images.py docs/vulkan-migration/reference-old-ui.png artifacts/screens/home.png
python tools/compare_images.py Previews/image.png artifacts/screens/home.png   # pipeline self-check -> 0.00

# HiDPI offscreen render (physical size = logical x scale).
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home@1.5.png --size 1160x740 --scale 1.5 --view home

# Capture helper for one view (offscreen, live backend, or desktop-window grab).
pwsh tools/screenshot.ps1 -View settings
pwsh tools/screenshot.ps1 -View home -Live
pwsh tools/screenshot.ps1 -View home -Window

# Windowed swapchain capture: preview data / real backend (real cover art).
pwsh tools/window_shot.ps1 -Out artifacts/screens/window.png
pwsh tools/window_shot.ps1 -Live -Out artifacts/screens/window-live.png

# Visual + behaviour verification loop (fmt, both feature-set checks, tests,
# all eight preview screenshots, home-vs-reference compare).
pwsh tools/verify.ps1

# Full local gate: fmt, clippy -D warnings, rustdoc -D warnings, both test
# configurations, locked release build, screenshot + reference compare,
# legacy-shell dependency guards, duplicate-deps baseline, cargo-deny if installed.
pwsh tools/ci-local.ps1

# Renderer GPU tests (headless): SDF rounded rects/borders, texture UVs, clip
# layers, multi-frame texture lifecycle, atlas-growth stress, PNG readback.
cargo test -p rift-desktop gfx

# Vulkan validation layer is enabled by RIFT_VK_VALIDATION=1; expect zero
# validation messages on the commands above (e.g. for the screenshot run).
$env:RIFT_VK_VALIDATION = "1"
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home-validated.png --size 1160x740 --view home
```

Committed evidence: [`screenshots/`](screenshots/) (the eight preview views, the
two HiDPI captures, the live window and the contact sheet) and
[`evidence/final-gate.log`](evidence/final-gate.log). Fresh runs write to the
gitignored `artifacts/`.
