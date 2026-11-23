//! Rift desktop — a Vulkan-rendered, immediate-mode rendition of the Rift UI.
//!
//! Crate layout:
//! * [`gfx`] — Vulkano renderer (window/swapchain/pipelines/textures/screenshots)
//! * [`ui`] — hand-written immediate-mode GUI framework (layout, widgets, input)
//! * [`text`] — font rasterisation and atlas
//! * [`screens`] — the actual application screens, matching the old UI
//! * [`state`] — UI/application state model and the actions screens emit
//! * [`controller`] — bridge to the native backend
//! * [`cover`] — cover-art decode/downscale and fetch de-duplication
//! * [`app`] — root object that composes one frame end-to-end
//! * [`theme`] — design tokens (mirrors the old styles.css)
//! * [`window_state`] — window size/position/maximized persistence
//! * [`single_instance`] — process-wide single-instance guard
//! * [`preview`] — deterministic placeholder data for screenshots
//!   (`RIFT_PREVIEW`; development builds and the explicit `preview` feature)

/// The Linux launcher identity of the application: the `.desktop` entry's
/// basename (without the `.desktop` suffix), the Wayland `app_id`, and the X11
/// `WM_CLASS` *class* name.
///
/// All three have to agree, or the compositor cannot group the window under the
/// launcher's icon/taskbar entry. `crates/rift-desktop/assets/linux/Rift.desktop`
/// pins the entry side and `tests/desktop_entry.rs` guards the agreement;
/// `main.rs` applies this identity to the window on Linux.
pub const APP_ID: &str = "Rift";

/// The executable name (Cargo's `[[bin]]`). On Linux it is also the X11
/// `WM_CLASS` *instance* and the icon-theme name the installer writes
/// (`<size>/apps/rift-music.png`, matched by the desktop entry's `Icon=`).
pub const BINARY_NAME: &str = "rift-music";

pub mod app;
pub mod controller;
pub mod cover;
pub mod gfx;
#[cfg(any(debug_assertions, feature = "preview", test))]
pub mod preview;
pub mod screens;
pub mod single_instance;
pub mod state;
pub mod text;
pub mod theme;
pub mod ui;
pub mod window_state;
