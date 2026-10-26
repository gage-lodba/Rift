//! Persistent window size/position/maximized state, replacing the old desktop
//! shell's window-state plugin after the native rewrite.
//!
//! The old plugin wrote `%APPDATA%\dev.jerimiah.rift\.window-state.json` with a
//! per-window object:
//!
//! ```json
//! {
//!   "main": {
//!     "width": 1160, "height": 740,
//!     "x": 2552, "y": -8,
//!     "prev_x": 2552, "prev_y": -8,
//!     "maximized": true, "visible": true,
//!     "decorated": false, "fullscreen": false
//!   }
//! }
//! ```
//!
//! [`WindowState::load`]/[`WindowState::save`] read and write **exactly** that
//! shape, so an existing installation keeps its remembered window. Sizes are
//! stored in logical pixels (the unit the old window config and the UI layout
//! use); they are converted through the window's scale factor in
//! [`restore`]/[`track`].
//!
//! Integration for `main.rs` (the Lead owns that file):
//!
//! ```text
//! // In ApplicationHandler::resumed, before creating the window:
//! let saved = window_state::WindowState::load_default();
//! let base = Window::default_attributes()
//!     .with_title("Rift")
//!     .with_decorations(false)
//!     .with_inner_size(saved.map(|s| s.size()).unwrap_or(window_state::DEFAULT_SIZE))
//!     .with_min_inner_size(window_state::MIN_SIZE);
//! // Then after `event_loop.create_window(base)?`:
//! if let Some(saved) = saved { window_state::restore(&window, &saved); }
//!
//! // Keep a `WindowState` alongside the window and update it from events:
//! // WindowEvent::Resized / Moved / ScaleFactorChanged -> window_state::track(&window, &mut state)
//! // WindowEvent::CloseRequested / about-to-wait -> window_state::save_on_exit(&state)
//! ```
//!
//! Nothing here touches the real data directory in tests: [`WindowState::load`]
//! / [`WindowState::save`] take an explicit directory, and the tests use
//! throwaway ones.

use std::path::{Path, PathBuf};

use winit::dpi::{LogicalPosition, LogicalSize};
use winit::window::Window;

/// The window label the old plugin used (the legacy config had one window,
/// `"main"`). Kept as the top-level JSON key.
pub const WINDOW_LABEL: &str = "main";

/// File name the old plugin used. Must not change: it is where an existing
/// installation's window geometry lives.
pub const FILE_NAME: &str = ".window-state.json";

/// Initial size of the app window (old window config: 1160x740, logical px).
pub const DEFAULT_WIDTH: u32 = 1160;
pub const DEFAULT_HEIGHT: u32 = 740;

/// Minimum inner size (old window config: 940x620, logical px). Saved sizes
/// below this are clamped up: the old plugin could remember a size from before
/// the minimum existed, and restoring something smaller than the layout needs
/// would render a broken window.
pub const MIN_WIDTH: u32 = 940;
pub const MIN_HEIGHT: u32 = 620;

/// Ceiling for a restored size. A corrupt/hand-edited file must not be able to
/// ask for a 4-billion-pixel window (which some drivers reject outright).
pub const MAX_DIMENSION: u32 = 16384;

/// Default inner size as a plain tuple (for `WindowAttributes::with_inner_size`
/// callers that do not want a winit type).
pub const DEFAULT_SIZE: (u32, u32) = (DEFAULT_WIDTH, DEFAULT_HEIGHT);

/// Minimum inner size as a plain tuple.
pub const MIN_SIZE: (u32, u32) = (MIN_WIDTH, MIN_HEIGHT);

/// Reverse-DNS application identifier (the data-directory name).
///
/// With the `backend` feature this is the backend's constant; without it the
/// renderer/UI-only builds still need the path for window-state and the
/// single-instance guard, so a copy lives here. They must stay equal.
#[cfg(feature = "backend")]
pub(crate) const APP_IDENTIFIER: &str = rift::APP_IDENTIFIER;
#[cfg(not(feature = "backend"))]
pub(crate) const APP_IDENTIFIER: &str = "dev.jerimiah.rift";

/// The remembered window geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    /// Inner size in logical pixels.
    pub width: u32,
    pub height: u32,
    /// Outer position in logical pixels. Negative values are legitimate
    /// (multi-monitor setups, window partly above/left of the primary display).
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            x: 0,
            y: 0,
            maximized: false,
        }
    }
}

impl WindowState {
    /// Logical inner size as a plain tuple.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Logical outer position as a plain tuple.
    pub fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Read the remembered window state from `data_dir`.
    ///
    /// Returns `None` when the file is missing, unreadable, malformed, or names
    /// a zero/incapable size — the caller then starts with the default geometry.
    /// A corrupt file is deliberately left in place (not deleted): it is the
    /// user's only copy and a later save will replace it.
    pub fn load(data_dir: &Path) -> Option<Self> {
        let body = std::fs::read_to_string(data_dir.join(FILE_NAME)).ok()?;
        let document: serde_json::Value = serde_json::from_str(&body).ok()?;
        parse_document(&document)
    }

    /// Write the current geometry to `data_dir`, creating the directory if
    /// needed.
    ///
    /// Best-effort: failures are logged, never propagated — losing remembered
    /// window geometry must not affect shutdown. Atomic (temp file + rename) so
    /// a crash mid-write cannot truncate the file.
    pub fn save(&self, data_dir: &Path) {
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            tracing::warn!(
                "could not create the data directory {} for window state: {e}",
                data_dir.display()
            );
            return;
        }
        let document = self.document();
        let bytes = match serde_json::to_vec_pretty(&document) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("could not serialize window state: {e}");
                return;
            }
        };
        let path = data_dir.join(FILE_NAME);
        if let Err(e) = write_atomic(&path, &bytes) {
            tracing::warn!("could not save window state to {}: {e}", path.display());
        }
    }

    /// Read from the platform data directory (see [`default_data_dir`]).
    ///
    /// This is the `main.rs` entry point; tests use [`load`](Self::load) with a
    /// temp directory so the user's real file is never touched.
    pub fn load_default() -> Option<Self> {
        default_data_dir().and_then(|dir| Self::load(&dir))
    }

    /// Save to the platform data directory.
    pub fn save_default(&self) {
        match default_data_dir() {
            Some(dir) => self.save(&dir),
            None => tracing::warn!("no data directory; window state will not be saved"),
        }
    }

    /// The exact JSON document the old plugin wrote (same keys, same nesting).
    ///
    /// `prev_x`/`prev_y` mirror `x`/`y`: the plugin used them for the
    /// pre-maximize position, and the native app restores the normal geometry
    /// directly, so mirroring keeps the document schema-identical for any tool
    /// that reads it. `visible`/`decorated`/`fullscreen` are written with the
    /// app's fixed window configuration (always visible, undecorated,
    /// non-fullscreen) rather than left out.
    fn document(&self) -> serde_json::Value {
        let mut window = serde_json::Map::new();
        window.insert("width".into(), serde_json::json!(self.width));
        window.insert("height".into(), serde_json::json!(self.height));
        window.insert("x".into(), serde_json::json!(self.x));
        window.insert("y".into(), serde_json::json!(self.y));
        window.insert("prev_x".into(), serde_json::json!(self.x));
        window.insert("prev_y".into(), serde_json::json!(self.y));
        window.insert("maximized".into(), serde_json::json!(self.maximized));
        window.insert("visible".into(), serde_json::json!(true));
        window.insert("decorated".into(), serde_json::json!(false));
        window.insert("fullscreen".into(), serde_json::json!(false));

        let mut root = serde_json::Map::new();
        root.insert(WINDOW_LABEL.to_string(), serde_json::Value::Object(window));
        serde_json::Value::Object(root)
    }
}

/// Read the remembered state and apply it to `window`: inner size, outer
/// position, then maximized.
///
/// Intended for `main.rs` right after the window is created. Best-effort: winit
/// request failures are not surfaced (they are platform quirks, and the default
/// geometry is already in place).
///
/// A remembered position that no longer overlaps any monitor (the display it was
/// on was unplugged, the layout changed, or the file was hand-edited) is not
/// restored: the window is centred on the current monitor instead, because an
/// off-screen window cannot be reached with the mouse. The saved size and
/// maximized flag still apply.
pub fn restore(window: &Window, state: &WindowState) {
    let (width, height) = state.size();
    // A request, not a guarantee: the compositor may adjust it (and some
    // platforms reject it before the window is mapped). The default geometry is
    // already in place, so the result is deliberately ignored.
    let _ = window.request_inner_size(LogicalSize::new(width as f64, height as f64));
    if let Some((x, y)) = restored_position(window, state) {
        window.set_outer_position(LogicalPosition::new(x, y));
    }
    if state.maximized {
        window.set_maximized(true);
    }
}

/// Smallest overlap, in physical pixels, between the window and a monitor that
/// counts as "the remembered position is still reachable".
///
/// A position that puts only a few pixels of the window on screen is as unusable
/// as one that is completely off it, so the check requires a grab-sized strip of
/// the window to be visible.
const MIN_VISIBLE_EXTENT: f64 = 48.0;

/// The position [`restore`] should use, in logical pixels.
///
/// `Some` keeps the remembered position when it is still reachable (or when the
/// platform exposes no monitor information to judge it against); `None` means no
/// fallback monitor could be found either, so the caller should leave the window
/// where the OS put it.
fn restored_position(window: &Window, state: &WindowState) -> Option<(f64, f64)> {
    let saved = (f64::from(state.x), f64::from(state.y));
    let scale = window.scale_factor();
    // `set_outer_position(LogicalPosition)` multiplies by the window's current
    // scale factor, so validate the physical rectangle it will actually produce.
    let candidate = (
        saved.0 * scale,
        saved.1 * scale,
        f64::from(state.width) * scale,
        f64::from(state.height) * scale,
    );
    let monitors: Vec<(f64, f64, f64, f64)> = window
        .available_monitors()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            (
                f64::from(position.x),
                f64::from(position.y),
                f64::from(size.width),
                f64::from(size.height),
            )
        })
        .collect();
    if monitors.is_empty() || rect_has_visible_area(candidate, &monitors) {
        // No monitor list (headless/odd platform): trust the saved value rather
        // than silently moving every window on that platform.
        return Some(saved);
    }
    tracing::warn!(
        "remembered window position ({}, {}) is not on any monitor; centring instead",
        state.x,
        state.y
    );
    let monitor = window
        .current_monitor()
        .or_else(|| window.primary_monitor())?;
    let position = monitor.position();
    let size = monitor.size();
    let x = f64::from(position.x) + (f64::from(size.width) - candidate.2) / 2.0;
    let y = f64::from(position.y) + (f64::from(size.height) - candidate.3) / 2.0;
    Some((x / scale, y / scale))
}

/// Whether `window` (`x, y, width, height`, physical px) overlaps at least one
/// monitor by a meaningful strip. Pure so the restore fallback is testable
/// without creating a window.
fn rect_has_visible_area(window: (f64, f64, f64, f64), monitors: &[(f64, f64, f64, f64)]) -> bool {
    let (x, y, width, height) = window;
    let needed_width = width.min(MIN_VISIBLE_EXTENT);
    let needed_height = height.min(MIN_VISIBLE_EXTENT);
    monitors
        .iter()
        .any(|&(monitor_x, monitor_y, monitor_w, monitor_h)| {
            let overlap_width = (x + width).min(monitor_x + monitor_w) - x.max(monitor_x);
            let overlap_height = (y + height).min(monitor_y + monitor_h) - y.max(monitor_y);
            overlap_width >= needed_width && overlap_height >= needed_height
        })
}

/// Update `state` from the live window: size, position and maximized flag.
///
/// Call from `WindowEvent::Resized`, `Moved`, `ScaleFactorChanged` and
/// `Maximized`/`Unmaximized`. While the window is maximized or minimized the
/// normal (restored) geometry is kept, so unmaximizing later returns to it.
pub fn track(window: &Window, state: &mut WindowState) {
    let maximized = window.is_maximized();
    state.maximized = maximized;
    // Minimized windows report zero/nonsense inner sizes; don't overwrite the
    // remembered geometry with them.
    if maximized || window.is_minimized().unwrap_or(false) {
        return;
    }

    let scale = window.scale_factor();
    let size = window.inner_size().to_logical::<f64>(scale);
    if size.width >= 1.0 && size.height >= 1.0 {
        state.width = size.width.round() as u32;
        state.height = size.height.round() as u32;
    }

    if let Ok(position) = window.outer_position() {
        let position = position.to_logical::<i32>(scale);
        state.x = position.x;
        state.y = position.y;
    }
}

/// Persist `state` to the default data directory. Call from the app's exit path
/// (`WindowEvent::CloseRequested` or just before the event loop returns).
pub fn save_on_exit(state: &WindowState) {
    state.save_default();
}

/// Platform application-data directory for the window-state / single-instance
/// files.
///
/// With the `backend` feature this is [`rift::native_data_dir`] (the single
/// source of truth, including `RIFT_DATA_DIR`); without it (renderer/UI-only
/// builds and tests) the same layout is derived from the environment so the
/// module needs no backend dependency.
pub(crate) fn default_data_dir() -> Option<PathBuf> {
    #[cfg(feature = "backend")]
    {
        rift::native_data_dir().ok()
    }
    #[cfg(not(feature = "backend"))]
    {
        fallback_data_dir()
    }
}

#[cfg(not(feature = "backend"))]
fn fallback_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RIFT_DATA_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(|appdata| PathBuf::from(appdata).join(APP_IDENTIFIER))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join(APP_IDENTIFIER)
            })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .map(|base| base.join(APP_IDENTIFIER))
    }
}

/// Parse the plugin document. Accepts the `"main"` window explicitly and falls
/// back to the first window object for a file written with another label (the
/// old plugin keyed by window label).
fn parse_document(document: &serde_json::Value) -> Option<WindowState> {
    let window = document
        .get(WINDOW_LABEL)
        .or_else(|| document.as_object()?.values().next())?;

    let width = window.get("width")?.as_u64()?;
    let height = window.get("height")?.as_u64()?;
    // A zero dimension is not a window: treat the file as unusable and let the
    // caller fall back to the defaults.
    if width == 0 || height == 0 {
        return None;
    }
    let width = clamp_dimension(width, MIN_WIDTH);
    let height = clamp_dimension(height, MIN_HEIGHT);

    let x = window
        .get("x")
        .and_then(serde_json::Value::as_i64)
        .map(clamp_coordinate)
        .unwrap_or(0);
    let y = window
        .get("y")
        .and_then(serde_json::Value::as_i64)
        .map(clamp_coordinate)
        .unwrap_or(0);
    let maximized = window
        .get("maximized")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    Some(WindowState {
        width,
        height,
        x,
        y,
        maximized,
    })
}

/// Clamp a saved dimension into the supported range for that axis (below-min
/// sizes are lifted to the minimum; absurd sizes are capped).
fn clamp_dimension(value: u64, minimum: u32) -> u32 {
    (value.min(u32::MAX as u64) as u32).clamp(minimum, MAX_DIMENSION)
}

/// Saturate a saved coordinate into `i32` (the file is JSON, so a hand-edited
/// i64 must not wrap into a random screen position).
fn clamp_coordinate(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Write via a uniquely named sibling temp file + rename, so a crash cannot
/// truncate the remembered state.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| FILE_NAME.to_string());
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.{}-{unique}.tmp", std::process::id()));
    let result = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        // Never leave scratch next to the user's data after a failed save.
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A scratch directory that deletes itself, so a failing test cannot leave
    /// junk behind (same idea as the backend's `util::testing::TempDir`).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rift-window-state-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn state() -> WindowState {
        WindowState {
            width: 1160,
            height: 740,
            x: 2552,
            y: -8,
            maximized: true,
        }
    }

    /// The saved file keeps the exact plugin schema: a `"main"` object with the
    /// same keys, so the native app and any old tooling agree on the file.
    #[test]
    fn save_writes_the_plugin_schema() {
        let dir = TempDir::new("schema");
        state().save(dir.path());

        let body = std::fs::read_to_string(dir.path().join(FILE_NAME)).expect("read saved state");
        let document: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let window = document.get(WINDOW_LABEL).expect("the main window object");
        for key in [
            "width",
            "height",
            "x",
            "y",
            "prev_x",
            "prev_y",
            "maximized",
            "visible",
            "decorated",
            "fullscreen",
        ] {
            assert!(
                window.get(key).is_some(),
                "the plugin schema needs a {key:?} key: {body}"
            );
        }
        assert_eq!(window["width"], 1160);
        assert_eq!(window["height"], 740);
        assert_eq!(window["x"], 2552);
        assert_eq!(window["y"], -8);
        assert_eq!(window["maximized"], true);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new("round-trip");
        let original = state();
        original.save(dir.path());
        assert_eq!(WindowState::load(dir.path()), Some(original));
    }

    /// A file written by the old shell's window-state plugin (extra keys and
    /// all) loads unchanged: this is the upgrade path for existing installs.
    #[test]
    fn loads_the_existing_legacy_plugin_document() {
        let dir = TempDir::new("legacy");
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{
                "main": {
                    "width": 1160, "height": 740,
                    "x": 2552, "y": -8,
                    "prev_x": 2552, "prev_y": -8,
                    "maximized": true, "visible": true,
                    "decorated": false, "fullscreen": false
                }
            }"#,
        )
        .expect("seed legacy file");

        assert_eq!(
            WindowState::load(dir.path()),
            Some(WindowState {
                width: 1160,
                height: 740,
                x: 2552,
                y: -8,
                maximized: true,
            })
        );
    }

    /// A document keyed by a different window label still loads (the plugin was
    /// per-label; the app has always had exactly one window).
    #[test]
    fn falls_back_to_the_first_window_object_for_another_label() {
        let dir = TempDir::new("other-label");
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"studio": {"width": 1000, "height": 700, "x": 4, "y": 5,"maximized": false}}"#,
        )
        .expect("seed file");

        let loaded = WindowState::load(dir.path()).expect("loads");
        assert_eq!(loaded.size(), (1000, 700));
        assert_eq!(loaded.position(), (4, 5));
    }

    /// Below-minimum geometry is lifted to the minimum the layout needs, and
    /// absurd dimensions are capped; a zero dimension is not a window at all.
    #[test]
    fn clamps_unusable_geometry() {
        let dir = TempDir::new("clamp");
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"main": {"width": 100, "height": 50, "maximized": false}}"#,
        )
        .expect("seed file");
        let loaded = WindowState::load(dir.path()).expect("loads");
        assert_eq!(loaded.size(), (MIN_WIDTH, MIN_HEIGHT));

        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"main": {"width": 999999999, "height": 999999999, "x": 9999999999999}}"#,
        )
        .expect("seed file");
        let loaded = WindowState::load(dir.path()).expect("loads");
        assert_eq!(loaded.size(), (MAX_DIMENSION, MAX_DIMENSION), "capped");
        assert_eq!(
            loaded.position(),
            (i32::MAX, 0),
            "coordinates saturate instead of wrapping"
        );

        for bad in [
            r#"{"main": {"width": 0, "height": 740}}"#,
            r#"{"main": {"width": 1160, "height": 0}}"#,
            r#"{"main": {"width": "wide", "height": 740}}"#,
            r#"{"main": {}}"#,
            r"{}",
            r"not json",
        ] {
            std::fs::write(dir.path().join(FILE_NAME), bad).expect("seed file");
            assert_eq!(
                WindowState::load(dir.path()),
                None,
                "must reject unusable state: {bad}"
            );
        }
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = TempDir::new("missing");
        assert_eq!(WindowState::load(dir.path()), None);
    }

    /// Saving creates the directory if needed (first launch with a fresh
    /// profile) and leaves no temporary file behind.
    #[test]
    fn save_creates_the_directory_and_leaves_no_scratch() {
        let dir = TempDir::new("create");
        let nested = dir.path().join("fresh").join("profile");
        state().save(&nested);

        assert!(nested.join(FILE_NAME).exists());
        let names: Vec<String> = std::fs::read_dir(&nested)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec![FILE_NAME.to_owned()], "no temp file may remain");
    }

    /// A failed save must not panic or leave scratch behind; the caller keeps
    /// running with the window it already has.
    #[test]
    fn a_failed_save_is_swallowed() {
        let dir = TempDir::new("failed");
        // A directory sitting at the target path makes every write/rename fail.
        std::fs::create_dir(dir.path().join(FILE_NAME)).expect("seed blocking directory");
        state().save(dir.path());
        assert!(dir.path().join(FILE_NAME).is_dir(), "target untouched");
    }

    /// A window fully inside a monitor — including a negative-coordinate monitor
    /// to the left of the primary one — is reachable and its position is kept.
    #[test]
    fn a_remembered_position_inside_a_monitor_is_reachable() {
        let monitors = vec![(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 2560.0, 1440.0)];
        assert!(rect_has_visible_area(
            (100.0, 100.0, 1160.0, 740.0),
            &monitors
        ));
        assert!(rect_has_visible_area(
            (2000.0, 50.0, 1160.0, 740.0),
            &monitors
        ));
        assert!(rect_has_visible_area(
            (-1500.0, 0.0, 1160.0, 740.0),
            &[(-1920.0, 0.0, 1920.0, 1080.0)],
        ));
    }

    /// A remembered position on a monitor that no longer exists is not
    /// reachable: restoring it would leave the window unreachable off-screen.
    #[test]
    fn a_remembered_position_off_screen_is_not_reachable() {
        let monitors = vec![(0.0, 0.0, 1920.0, 1080.0)];
        assert!(!rect_has_visible_area(
            (5000.0, 0.0, 1160.0, 740.0),
            &monitors
        ));
        assert!(!rect_has_visible_area(
            (0.0, -2000.0, 1160.0, 740.0),
            &monitors
        ));
        // 20 px of overlap is less than the grab-sized strip.
        assert!(!rect_has_visible_area(
            (-1140.0, 100.0, 1160.0, 740.0),
            &monitors
        ));
        // Just enough overlap is accepted.
        assert!(rect_has_visible_area(
            (-1100.0, 100.0, 1160.0, 740.0),
            &monitors
        ));
    }

    #[test]
    fn the_default_geometry_matches_the_old_window() {
        assert_eq!(DEFAULT_SIZE, (1160, 740));
        assert_eq!(MIN_SIZE, (940, 620));
        let default = WindowState::default();
        assert_eq!(default.size(), DEFAULT_SIZE);
        assert!(!default.maximized);
    }
}
