# Rift: native Vulkan rewrite report

**Status:** complete. Rift is a single native Rust binary that opens a `winit`
window, renders with [`vulkano`](https://crates.io/crates/vulkano), and draws its
entire interface through a hand-written immediate-mode GUI. There is no webview
and no WASM frontend.

This document records the resulting tree, how the pieces fit, and the evidence
that the rewrite preserves the look and behaviour of the previous app. The
detailed contracts live beside it in this directory:
[`architecture.md`](architecture.md), [`backend-api.md`](backend-api.md),
[`imgui-api.md`](imgui-api.md), [`ui-spec.md`](ui-spec.md),
[`vulkano-recipe.md`](vulkano-recipe.md), [`controller-notes.md`](controller-notes.md),
[`native-platform.md`](native-platform.md).

## Before / after

| Concern | Legacy app | This app (native) |
|---|---|---|
| Application shell | legacy desktop shell + web UI frontend (Rust→WASM) | `crates/rift-desktop/` (`rift-desktop`): `winit` 0.30 window + `vulkano` 0.35 renderer + hand-written immediate-mode GUI |
| UI model | retained virtual DOM, HTML/CSS | Immediate mode: every frame is rebuilt from application state and painted as a `PaintList` of primitives |
| Styling | the old stylesheet (1,158 lines), web fonts, flexbox | `crates/rift-desktop/src/theme.rs` design tokens + explicit pixel layout derived from a 1,100-line visual spec |
| Icons | inline SVG components | Material Icons font glyphs (embedded, Apache-2.0) with the same shapes/weights |
| Backend coupling | ~55 IPC command handlers, JS bridge globals, app-side emitted events | `crates/rift/` plain library: 43 plain methods on `BackendHandle` + one `BackendEvent` channel, called in-process |
| Window | WebView2/WKWebView/WebKitGTK, decorations off, 1160×740 | `winit` undecorated window, 1160×740, custom titlebar/controls/drag/resize |
| Window persistence | the old shell's window-state plugin | `crates/rift-desktop/src/window_state.rs` (same `.window-state.json` schema) |
| Single instance | the old shell's single-instance plugin | `crates/rift-desktop/src/single_instance.rs` (Windows named mutex / Unix lock file; second launch exits) |
| File dialogs | the old shell's dialog plugin | `rfd` in the backend, run off the UI thread |
| Open URL | the old shell's opener | native `explorer.exe` / `xdg-open` / `open`, `http(s)` only |
| Webview APIs used by UI | `web_sys` (navigator online, storage, DOM events) | winit input events, controller connectivity probe, native state |
| Build | web UI bundler + desktop-shell CLI (WASM target, Node test shim) | `cargo build -p rift-desktop` |

Dependencies removed outright: the legacy desktop-shell framework and its
plugins, the web-view frontend stack (`wasm-bindgen` family, `js-sys`,
`web-sys`, `gloo-timers`, web bundler). `cargo tree -p rift-desktop` contains no
legacy webview/shell crate.

## Architecture

```
Cargo workspace (members under crates/)
├─ crates/rift-desktop/  rift-desktop  the binary: event loop, renderer, IMGUI, screens, controller
├─ crates/rift/          rift          native backend library (audio, player, library, downloads,
│                                      settings, yt-dlp, media keys, Discord RPC, event bus)
├─ crates/rift-ytmusic/  rift-ytmusic  innertube metadata client
└─ crates/rift-types/    rift-types    shared data models
```

Frame loop (all on the UI thread):

1. winit events → `ui::InputState` (mouse/keys/scroll/DPI).
2. `App::tick()` drains `BackendEvents` from the backend channel and applies them
   to `State`, expires toasts, and starts cover-art fetches.
3. `Ui::begin_frame`; `screens::root` lays out and paints the whole window from
   scratch (titlebar → sidebar → main view → queue panel → player bar → popups →
   toast/banner), returning a `Vec<Action>`.
4. `Ui::end_frame()` produces a `gfx::paint::PaintList` (layers, quads, glyphs,
   texture uploads); the `gfx::Renderer` uploads textures, records one command
   buffer and presents.
5. `App` applies the collected actions: UI-local navigation/dialog changes
   immediately, backend commands through `Controller::dispatch`.

The renderer is one SDF-based quad pipeline (rounded rectangles, borders,
anti-aliased corners, textured images, per-layer scissors) rendering into an
`*_SRGB` swapchain with ring-buffered per-frame uniforms/vertices; text and icons
are glyph quads from one packed font atlas. Offscreen rendering to PNG (`--
screenshot`) shares the same pipeline and drives the acceptance screenshots.

## UI parity

The acceptance target was the old app's screenshot at 1160×740
(`docs/vulkan-migration/reference-old-ui.png`), and the new app keeps the same
layout at pixel level: 46 px titlebar, 335/216 px resizable sidebar, 84 px player
bar, 235 px collapsible "UP NEXT" panel, 4-across library cards, 54 px track
rows, 40/34 px covers, the same colors, radii, 10 px scrollbars, hover/focus
states, context menus, toasts and update banner.

Screens reimplemented 1:1: Home, Liked Songs, Playlist (rename/delete/kebab,
remove rows), Search (Songs/Artists/Albums tabs with lazy loading), Artist
(top songs, Show all, albums/singles grids), ArtistSongs, Album, Settings
(Discord RPC, crossfade, yt-dlp status/path/install, update checks), plus the
collapsible queue panel with drag reorder, track context menus, menubar/dialogs,
toasts, update banner and the custom titlebar with window controls.

Interactions: click/hover/focus, keyboard focus ring and Tab navigation, text
inputs with caret/selection/clipboard trait, sliders (seek/volume/crossfade),
switches, menus with submenu and Escape/outside dismissal, drag-reorder for the
queue and sidebar playlists, Space / Ctrl+Arrow shortcuts, window drag, edge
resize, double-click maximize and min/max/close buttons.

Visual acceptance numbers for the reference screen (1160×740, preview data):
the final comparison is recorded in the verification section below; the diff is
dominated by glyph-shape differences between the old capture's Segoe UI and the
embedded Inter (the layout/ink positions match to 0–1 px), not by layout errors.

HiDPI: the UI lays out in logical pixels while glyphs are rasterized at
`logical × scale_factor`; `--screenshot ... --scale 1.25|1.5` produces crisp
physical-resolution captures (verified at 1450×925 and 1740×1110).

## Backend parity

All pre-existing backend logic was preserved (the diff is glue removal, not a
rewrite): audio thread + rodio playback, queue/navigation/shuffle/repeat,
crossfade, persisted library (likes/playlists/recents), offline downloads,
settings, ytmusic metadata, yt-dlp detection/install, OS media keys / SMTC /
MPRIS / now-playing, Discord Rich Presence, update check.

Legacy glue was replaced:

* `RiftBackend::new(BackendConfig) -> (RiftBackend, BackendEvents)` owns the
  data directory, runtime, and background threads; `RiftBackend` derefs to a
  cloneable `BackendHandle` with every former command as a plain method.
* All `app.emit("rift://…")` events are now one unbounded `BackendEvent`
  channel drained once per frame (`Track`, `State`, `Queue`, `Progress`,
  `Library`, `Downloads`, `Error`, `Notice`, `OpenPlaylist`) with the old
  payload shapes preserved 1:1.
* Window commands left the backend entirely (winit owns the window).
* File import/export and URL opening are native (`rfd`, platform opener).

Live evidence (this machine, 2026-10): an end-to-end pipeline smoke test
(`cargo run -p rift --example smoke`) resolved yt-dlp 2026.08.19, searched
"YouTube Music for Daft Punk — Get Lucky", fetched 5,978,549 bytes and decoded
369.66 s of audio with rodio. A windowed launch against the real user data
directory renders the actual library, queue, playback state and network-loaded
cover art through the Vulkan swapchain.

## Verification

Reproduce (PowerShell 7, repo root):

```powershell
cargo check -p rift-desktop
cargo build -p rift-desktop
cargo test -p rift -p rift-types -p rift-ytmusic -p rift-desktop
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home.png --size 1160x740 --view home
python tools/compare_images.py docs/vulkan-migration/reference-old-ui.png artifacts/screens/home.png --label home
pwsh tools/verify.ps1
```

The final results, on this machine (Windows 11, Rust 1.88.0, AMD Radeon RX 7800
XT, Vulkan 1.4):

| Check | Result |
|---|---|
| `cargo check` (app, default + `--no-default-features`) | pass, zero warnings |
| `cargo test -p rift-desktop` (lib) | **297 passed, 0 failed**, 7 ignored (2 GPU gallery + 5 manual GPU visual tests) |
| `cargo test -p rift-desktop --test ui_flow` | **10 passed, 0 failed** (headless interaction flows; default features already include `backend`) |
| `cargo test -p rift` (native backend) | **179 passed, 0 failed** |
| `cargo clippy -p … --all-targets -- -D warnings` | **0 errors / 0 diagnostics** |
| `cargo fmt --all -- --check` | clean |
| Offscreen screenshots, all 8 views | exit 0, `docs/vulkan-migration/screenshots/*` |
| Home vs old reference (1160×740) | **mean abs channel delta 2.14 / 2.97% of pixels > 16** |
| HiDPI offscreen (`--scale 1.25`/`1.5`) | 1450×925 / 1740×1110, crisp physical-pixel text |
| Windowed launch + desktop capture (preview) | pass, correct UI through a real swapchain |
| Windowed launch + live backend (real library, queue, cover art) | pass, real covers from the network render in the window |
| Vulkan validation layer (`RIFT_VK_VALIDATION=1`) | zero VUID/validation messages (live + windowed + resize runs) |
| Window resize / maximize / restore | 940×620 min, 1600×900, maximize, restore all render correctly; clean exit |
| Window-state persistence + single instance | verified end to end (same legacy JSON schema; second launch exits) |
| Renderer performance | 3.08 ms average frame, p95 3.45–3.74 ms, 60 fps with ~5× headroom |
| `cargo tree -p rift-desktop` contains no legacy webview/shell crates | verified empty |
| Full local gate (`tools/ci-local.ps1`) | **PASSED** — 14 summary rows: **13 steps PASS** + `cargo deny check all` reported SKIP locally because `cargo-deny` is not installed (CI's dedicated `deny` job runs it), **556 tests passed / 0 failed**; log [`evidence/final-gate.log`](evidence/final-gate.log) |

The full gate runs, in CI order: toolchain consistency → `cargo fmt --all
--check` → `cargo clippy … -D warnings` (all four packages) → `cargo doc` for the
three library crates with rustdoc warnings denied → `cargo test`
(all four packages, `--locked`, 556 passed: backend 179, app lib 297, app bin 1,
headless interaction flows 10, types 16, ytmusic 53) → the app again in the UI-only
`--no-default-features` configuration (296 lib + 1 bin + 10 flows) → the shipped
release build (`--release -p rift-desktop --features backend --locked`:
`target/release/rift-music.exe`, 18,417,152 bytes; ~139 s from a cold target) →
preview-stripped check on the shipped binary (it rejects `--preview`) →
acceptance-only release build with the explicit `preview` feature →
offscreen release screenshot → reference comparison → legacy-dependency-tree
guard → manifest scan → duplicate-dependency baseline → `cargo deny check all`
(supply-chain policy from `deny.toml`; skipped only where the tool is not
installed).

Raw gate logs and screenshot outputs lived in the gitignored `artifacts/`
scratch directory and were cleaned up after acceptance; the final gate log is
committed at [`evidence/final-gate.log`](evidence/final-gate.log), and every
command above regenerates its output.

An interactive window was additionally exercised end to end during acceptance:
preview UI and live backend data render correctly through a real swapchain and
window resize, cover art loads from the network, window geometry persists and
restores, a second launch exits via the single-instance guard, and real
keyboard/clipboard input works in the search field (automated Ctrl+C/V test).

The curated, reproducible screenshot set is committed in
[`screenshot-gallery.md`](screenshot-gallery.md) /
[`screenshots/`](screenshots/) — including the archived old-UI capture, the new
Home view, HiDPI 1.25×/1.5× renders, a live-window capture with real cover art,
the side-by-side/diff/overlay comparison and a labeled contact sheet.

## Audit hardening (2026-10 pass)

A best-practice/audit pass ran on top of the conversion. It changed no product
behaviour, but the tree is now checked more strictly and the last known
robustness gaps are closed:

* **Lints / `unsafe` policy.** `[workspace.lints]` denies `unsafe_op_in_unsafe_fn`,
  `unused_must_use`, `dbg_macro`, `todo!`, `unimplemented!` and
  `undocumented_unsafe_blocks`; the three library crates are `#![forbid(unsafe_code)]`.
  The only `unsafe` left is the app crate's Win32/GPU FFI, where every block
  carries a `// SAFETY:` note (the lint makes an undocumented one a build error).
* **Panic-free runtime paths + `missing_docs`.** Poisoned locks recover through
  `LockExt` instead of cascading panics, fallible startup returns a typed
  `BackendError`, and corrupt/unreadable state files degrade with a logged
  warning rather than aborting. The library crates also carry
  `#![warn(missing_docs)]`, which the CI `-D warnings` gate enforces.
* **Durable writer.** `library.json`/`settings.json` writes are queued to a
  background `util::DurableWriter` (atomic temp+rename, FIFO, last write wins)
  and flushed on `RiftBackend::shutdown`/`Drop`, so UI-thread commands never
  block on disk (see [`backend-api.md`](backend-api.md) §4).
* **Release / CI hygiene.** The release profile is LTO + one codegen unit +
  `strip = "symbols"`; a `cargo deny check all` job enforces the `deny.toml`
  advisories/licenses/bans/sources policy; the CI feature matrix tests both the
  default (backend) and `--no-default-features` (UI-only) app configurations.
  Preview/screenshot support is development-only: debug builds include it
  automatically, while the shipped release build compiles the placeholder data
  and `--preview`/`--screenshot` CLI out (a non-default `preview` feature is the
  explicit opt-in used only by the acceptance gate). Both CI and the release
  workflow verify that the shipped binary rejects the dev flags.
* **Feature catch-up — sidebar playlist drag-to-reorder.** The old UI's shared
  `use_reorder` hook had no native port (only the queue panel had drag). Sidebar
  playlist rows are now drag-reorderable with the same **live, animated preview**:
  the dragged row takes the hovered slot (marked by a 1 px accent outline) and
  rows in between slide one slot toward the vacated one while the pointer is
  held, so the post-drop order is shown in real time (`drag_visual_index` is the
  native equivalent of the old `drag_shift_style`). Row slots are animated with a
  short per-row spring (`spring_slot`, K=500/C=25: ~8 % overshoot, peak ~0.15 s,
  settled to 2 % by ~0.32 s at 60 Hz) instead of jumping, and a committed-order
  hold keeps the previewed order while the `MovePlaylist` event is in flight, so
  a drop never snaps back. A plain click still navigates, the kebab menu is
  unaffected, and Alt+Up/Down keyboard reordering is supported. A drop commits
  one `Action::MovePlaylist { from, to }`, which the controller resolves to the
  playlist id and passes to the backend's `move_playlist(id, to)`
  (remove-then-insert), so the list updates when the library event returns.
  Covered by headless drag/click/keyboard tests, spring/settling tests and a
  held-drag paint assertion.
* **Legacy library metadata repair.** Liked/recents rows written by the old
  app can lack `duration` and artist credits (album-page likes carried
  neither). On startup the backend first merges complete copies from the
  restored playback queue and the other library lists (offline), then runs a
  bounded background repair — at most 20 `search_tracks` lookups per launch,
  stopping after 3 consecutive errors, preferring an exact video-id match and
  otherwise a complete `same_song` result — and emits one `Library` event when
  rows change, so an open Liked view refreshes in place. A track that is played
  also adopts its fetched stream duration and backfills every stored copy. On
  the migrated local library this filled all 10 incomplete rows (9 missing
  durations, 1 missing artist) on the first launch. Matching is deliberately
  conservative: offline launches retry later, and mojibake legacy titles are
  left untouched rather than rewritten.
* **Review hardening (final pass).** The post-migration code review fixed the
  interaction edge cases it found: playlist/queue drags are cancelled on window
  focus loss without committing, a press with small pointer drift still
  navigates, playlist rows only accept clicks once the spring preview has
  settled, the menu backdrop cursor is pinned after all content is drawn,
  metadata repair got its match invariants tightened (dedupe order, empty-id
  guards, conservative network fallback, visible-lists-only startup merge,
  injectable repair tests), and the release workflow's macOS job now builds
  both Apple architectures and combines them with `lipo` instead of naming a
  nonexistent `universal-apple-darwin` target.
* **Module split.** The former ~1,900-line screen module was split into
  `screens/{chrome,cards,track_list,menu,settings}` + `screens/pages.rs`; the
  largest extracted file is `pages.rs` at 594 lines, instead of one monolith.

## Known limitations

* **Packaging/installers are out of scope** for this rewrite: the release
  workflow builds and uploads the native binaries; `.msi`/`.dmg`/`.AppImage`
  packaging (previously produced by the old shell) is not reimplemented.
* **Fonts**: the old capture was rendered with Segoe UI; the native app embeds
  Inter (plus Material Icons). Layout and ink positions match, glyph outlines
  differ slightly. Segoe UI is not embedded because it is not redistributable.
* **IME** (e.g. CJK composition) is not wired to `winit::Ime`; typed text from
  `KeyEvent::text` works, composition does not. The old webview had it for free.
* **Second instance** exits instead of focusing the running window (the old
  shell plugin's cross-process focus would need an IPC channel; documented in
  [`native-platform.md`](native-platform.md)).
* **Cross-platform verification**: developed and verified on Windows 11 / AMD
  Vulkan 1.4. Linux (Wayland/X11) and macOS are wired through the same crates and
  CI builds them, but were not run on hardware in this session.
