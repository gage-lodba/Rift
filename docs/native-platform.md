# Platform integration: window state + single instance

The app owns two pieces of desktop behaviour as small dependency-free modules.
They are wired into `crates/rift-desktop/src/main.rs` / `crates/rift-desktop/src/lib.rs`;
this document is the module contract.

| Feature | Native implementation | File |
|---|---|---|
| Window state | [`window_state`](#window-state) | `crates/rift-desktop/src/window_state.rs` |
| Single instance | [`single_instance`](#single-instance) | `crates/rift-desktop/src/single_instance.rs` |

Neither module adds a dependency: `window_state` uses `winit` (already a direct
dependency) + `serde_json` + `tracing`; `single_instance` uses raw `kernel32`
FFI on Windows and `flock` FFI on Unix.

Both modules compile and are fully unit-tested **without** the `backend` cargo
feature (they are part of the renderer/UI-only build), and with it enabled.

---

## window state

`crates/rift-desktop/src/window_state.rs` — remembers the window's size, position and maximized
flag across restarts, in the **exact file and JSON shape** the old plugin used,
so an existing installation keeps its remembered window.

### Data

* File: `<data dir>/.window-state.json` — i.e.
  `%APPDATA%\dev.jerimiah.rift\.window-state.json` on Windows,
  `~/.local/share/dev.jerimiah.rift/.window-state.json` on Linux,
  `~/Library/Application Support/dev.jerimiah.rift/.window-state.json` on macOS.
* The data directory is `rift::native_data_dir()` when the `backend` feature is
  on (so `RIFT_DATA_DIR` is honoured); renderer/UI-only builds derive the same
  path from the environment inside the module, because they cannot depend on the
  backend crate.
* The real user file is never written by tests; `load`/`save` take an explicit
  directory, and the tests use throwaway directories.

Exact schema (unchanged from the old window-state plugin):

```json
{
  "main": {
    "width": 1160,
    "height": 740,
    "x": 2552,
    "y": -8,
    "prev_x": 2552,
    "prev_y": -8,
    "maximized": true,
    "visible": true,
    "decorated": false,
    "fullscreen": false
  }
}
```

Sizes/positions are **logical pixels** (the unit of the old shell's window
config and of the UI layout); the module converts through the window's
scale factor in `restore`/`track`. `prev_x`/`prev_y` mirror `x`/`y`
(the native app restores normal geometry directly, so the pre-maximize pair has
no separate role; writing them keeps the file schema-identical).

Read rules on load: a missing/unreadable/corrupt file → `None` (file is left in
place). A `"main"` window object is used; another window label falls back to the
first object in the file. Sizes are clamped into `MIN_WIDTH..=MAX_DIMENSION` /
`MIN_HEIGHT..=MAX_DIMENSION` (below-minimum remembered sizes are lifted to the
minimum the layout needs; absurd sizes are capped), coordinates saturate into
`i32`. A zero width/height is treated as unusable and yields `None`.

### API

```rust
pub const WINDOW_LABEL: &str = "main";
pub const FILE_NAME: &str = ".window-state.json";
pub const DEFAULT_WIDTH: u32 = 1160;   // old shell config
pub const DEFAULT_HEIGHT: u32 = 740;
pub const MIN_WIDTH: u32 = 940;        // old shell config minWidth
pub const MIN_HEIGHT: u32 = 620;
pub const MAX_DIMENSION: u32 = 16384;
pub const DEFAULT_SIZE: (u32, u32) = (DEFAULT_WIDTH, DEFAULT_HEIGHT);
pub const MIN_SIZE: (u32, u32) = (MIN_WIDTH, MIN_HEIGHT);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub width: u32,   // logical inner size
    pub height: u32,
    pub x: i32,       // logical outer position (negative is legitimate)
    pub y: i32,
    pub maximized: bool,
}
impl Default for WindowState { /* 1160x740, (0,0), not maximized */ }

impl WindowState {
    pub fn size(&self) -> (u32, u32);
    pub fn position(&self) -> (i32, i32);
    pub fn load(data_dir: &Path) -> Option<Self>;
    pub fn save(&self, data_dir: &Path);       // best-effort, atomic, never panics
    pub fn load_default() -> Option<Self>;     // platform data dir
    pub fn save_default(&self);
}

/// Apply saved geometry to a live window (inner size, outer position, maximize).
pub fn restore(window: &winit::window::Window, state: &WindowState);
/// Refresh `state` from the live window (call on Resized/Moved/ScaleFactorChanged/
/// Maximized/Unmaximized). Keeps normal geometry while maximized/minimized.
pub fn track(window: &winit::window::Window, state: &mut WindowState);
/// Persist to the platform data dir (exit path).
pub fn save_on_exit(state: &WindowState);
```

`save` is `create_dir_all` + serialise + unique temp file + rename; failures are
logged with `tracing::warn` and swallowed (window geometry must never block
shutdown).

### Integration sketch for `main.rs`

```rust
use rift_desktop::window_state::{self, WindowState};

// In ApplicationHandler::resumed, before creating the window:
let saved = WindowState::load_default();
let attrs = winit::window::Window::default_attributes()
    .with_title("Rift")
    .with_decorations(false)
    .with_resizable(true)
    .with_inner_size(saved.map(WindowState::size).unwrap_or(window_state::DEFAULT_SIZE))
    .with_min_inner_size(window_state::MIN_SIZE);
let window = event_loop.create_window(attrs)?;

// After creation: apply position/maximize (size is usually already right).
let mut state = saved.unwrap_or_default();
if saved.is_some() {
    window_state::restore(&window, &state);
}

// In window_event:
//   Resized | Moved | ScaleFactorChanged | Maximized | Unmaximized
//       => window_state::track(&window, &mut state);
//   CloseRequested => { window_state::track(&window, &mut state);
//                       window_state::save_on_exit(&state); ... }

// On exit (just before the event loop returns), if not already saved:
window_state::save_on_exit(&state);
```

The `winit` calls used are `request_inner_size` (winit 0.30: the size request is
a hint, hence `let _ =`), `set_outer_position`, `set_maximized`, `is_maximized`,
`is_minimized`, `inner_size`, `outer_position`, `scale_factor`.

---

## single instance

`crates/rift-desktop/src/single_instance.rs` — exactly one Rift process may own the audio device,
the library files and the playback snapshot. The old plugin's extra behaviour
(focus the already-running window) is **not preserved**: there is no IPC channel
between instances anymore, so a second launch **exits immediately**. Document
that in the UX if desired ("Rift is already running").

### Mechanism

* **Windows** (raw `kernel32` FFI): named mutex
  `Local\dev.jerimiah.rift-single-instance`.
  First process creates and keeps the handle; a second process calls
  `CreateMutexW`, gets a handle plus `GetLastError() == ERROR_ALREADY_EXISTS`,
  closes it and exits. `Local\` is the per-session namespace, so two users on one
  machine each get an instance (correct for a per-user app). The mutex is
  released when the process exits, including a crash.
* **Linux/macOS/BSD** (raw `flock` FFI): non-blocking exclusive lock on
  `<data dir>/.single-instance.lock` (`LOCK_EX | LOCK_NB`). The kernel releases
  the lock when the process dies, so a crash cannot leave a stale lock.
* **Other platforms**: no-op guard that always succeeds (documented degradation,
  not a startup failure).

### API

```rust
pub const ...; // none
pub fn instance_name() -> String;              // stable resource name (for logs/tests)
pub struct SingleInstance { /* platform guard */ }
impl SingleInstance {
    /// None = another instance holds the lock (or the OS refused the resource:
    /// treated conservatively as busy).
    pub fn acquire() -> Option<Self>;
}
/// Convenience wrapper.
pub fn acquire() -> Option<SingleInstance>;
/// Diagnostic probe only (briefly acquires+releases); startup must use `acquire`.
pub fn is_already_running() -> bool;
```

The guard is RAII: keep it alive for the whole process; dropping it releases the
lock.

### Integration sketch for `main.rs`

```rust
fn main() -> anyhow::Result<()> {
    // Before the window, backend, audio thread or runtime: a second launch must
    // not fight the first over the audio device or library files.
    let _instance = match rift_desktop::single_instance::acquire() {
        Some(instance) => instance,
        None => {
            eprintln!("Rift is already running.");
            return Ok(());
        }
    };
    // ... window, backend, event loop ...
    Ok(())
}
```

---

## Verification (run 2026-10-18, Windows 11, Rust 1.88)

Both modules were compiled and tested in an out-of-tree scratch harness
(absolute `#[path]` includes; no changes to `crates/rift-desktop/src/lib.rs`, which the Lead
owns), with `winit = 0.30.13`:

* `cargo check` (no `backend` feature): OK — modules compile for the
  renderer-only build, `RIFT_DATA_DIR`/env fallback path included.
* `cargo check --features backend`: OK — same modules against `rift::
  native_data_dir()`.
* `cargo clippy --all-targets -- -D warnings` and the same with
  `--features backend`: **clean** (no warnings).
* `cargo test` and `cargo test --features backend`: **12 passed, 0 failed**,
  including:
  * exact legacy JSON document loads (upgrade path),
  * save→load round-trip and exact plugin schema keys,
  * clamping (below-min → min, absurd → capped, zero → rejected, coordinates
    saturate),
  * corrupt/missing file → `None`, failed save swallowed, no temp file left,
  * Windows named mutex: second acquire refused, dropped guard re-acquirable,
  * public guard: second `SingleInstance::acquire()` in the process refused,
  * the user's real `%APPDATA%\dev.jerimiah.rift\.window-state.json` was only
    read once to copy its schema; every test above used temp directories.
