# Platform integration: window state + single instance + Linux desktop launcher

The app owns three pieces of desktop behaviour: two small dependency-free
modules and the Linux desktop launcher. They are wired into
`crates/rift-desktop/src/main.rs` / `crates/rift-desktop/src/lib.rs`; this
document is the contract.

| Feature | Contract | Files |
|---|---|---|
| Window state | [`window_state`](#window-state) | `crates/rift-desktop/src/window_state.rs` |
| Single instance | [`single_instance`](#single-instance) | `crates/rift-desktop/src/single_instance.rs` |
| Linux desktop launcher | [`desktop launcher`](#linux-desktop-launcher) | `crates/rift-desktop/assets/linux/Rift.desktop`, `tools/install-linux-desktop.sh` |

None of these adds a crate: `window_state` uses `winit` (already a direct
dependency) + `serde_json` + `tracing`; `single_instance` uses raw `kernel32`
FFI on Windows and `flock` FFI on Unix; the desktop launcher is plain data plus
a shell installer, and its window identity uses two `winit` platform extensions
that the pinned `winit` already provides.

`window_state` and `single_instance` compile and are fully unit-tested
**without** the `backend` cargo feature (they are part of the renderer/UI-only
build), and with it enabled. The launcher has no code feature gate: its entry
and icon guard tests run in plain `cargo test` on every host, and the installer
round-trip is Linux-only.

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

`restore` also validates the remembered *position* against the monitors that
exist now: if the saved window would not overlap any monitor by at least a
grab-sized strip (48 physical px) — the display was unplugged, the arrangement
changed, the file was hand-edited — the saved size and maximized flag still
apply, but the window is centred on the current/primary monitor instead, because
an off-screen window cannot be reached with the mouse. A warning is logged. (If
the platform exposes no monitor list, the saved position is trusted rather than
moving every window.)

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
/// A saved position that is no longer on any monitor is replaced by centring.
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

## Linux desktop launcher

The Linux desktop launcher is optional desktop integration: a committed
freedesktop launcher entry plus a user-local installer. Running the binary
directly stays the supported no-install path; with the launcher installed Rift
appears in the application menu, and the compositor groups the running window
(and its icon) under that entry.

### Files

| File | Role |
|---|---|
| `crates/rift-desktop/assets/linux/Rift.desktop` | the committed launcher entry; installed unchanged except for the rebased `Exec` |
| `crates/rift-desktop/src/lib.rs` | `APP_ID = "Rift"` and `BINARY_NAME = "rift-music"` — the identity the entry and the window share |
| `crates/rift-desktop/src/main.rs` | `with_linux_launcher_identity()` applies that identity to the `winit` window at creation |
| `tools/install-linux-desktop.sh` | installs/removes the binary, icons, entry and install manifest into the XDG directories (or an explicit `<prefix>/bin` + `<prefix>/share`) |
| `crates/rift-desktop/tests/desktop_entry.rs` | guard tests for the entry, the icon assets and (Linux) the installer's install/uninstall behavior |

### Identity contract

The launcher identity is two names, and they have to agree everywhere they
appear, or the compositor cannot attach the running window to the launcher: the
window is not grouped/activated from the menu and gets a generic taskbar icon.

* `APP_ID = "Rift"` is the desktop-file ID (the entry's file name without the
  `.desktop` suffix), the Wayland `app_id` and the X11 `WM_CLASS` **class**
  (applied by `main.rs::with_linux_launcher_identity()`); the entry's
  `StartupWMClass` repeats it for X11 desktops.
* `BINARY_NAME = "rift-music"` is the executable name, the X11 `WM_CLASS`
  **instance**, the installed `Exec` target and the icon-theme name
  (`Icon=rift-music` → `<size>/apps/rift-music.png`).

The identity is applied to the window attributes before `create_window`, so the
first frame already advertises it. Verified against a live KWin session
(2026-10-30): the window was associated with the installed entry
(`desktopFileName` resolved to `Rift`) and reported `resourceClass` `Rift` and
`resourceName` `rift-music`. Wayland consumes only the class/app-id half of the
identity, while X11 consumes both names (class `Rift`, instance `rift-music`).

### Installer

```sh
tools/install-linux-desktop.sh                  # target/release, else target/debug
tools/install-linux-desktop.sh --binary PATH    # a built or downloaded release binary (no execute bit needed)
tools/install-linux-desktop.sh --prefix DIR     # DIR/bin + DIR/share instead of the XDG defaults
tools/install-linux-desktop.sh --dry-run        # print the operations, write nothing
tools/install-linux-desktop.sh --no-cache-refresh  # skip the desktop/icon cache refresh
tools/install-linux-desktop.sh --uninstall      # remove only the manifest-listed files
tools/install-linux-desktop.sh --uninstall --force  # no manifest: remove only the fixed paths, after a warning
```

With no `--prefix` the installer follows the XDG base directories: the binary
goes to `${XDG_BIN_HOME:-$HOME/.local/bin}/rift-music`, while the entry, icons
and install manifest go under `${XDG_DATA_HOME:-$HOME/.local/share}` as
`applications/Rift.desktop`, `icons/hicolor/<size>/apps/rift-music.png` and
`rift-desktop/install-manifest`. Both XDG variables must be absolute when set.
An explicit `--prefix DIR` instead uses `DIR/bin/rift-music` and `DIR/share/...`
for all of those, which is the form to use for a system tree such as
`/usr/local`; the script never escalates, so run it as whoever owns that tree.

An install is staged before it is published: the binary, the five icons and the
rebased entry are first written under temporary names in their destination
directories, then the staged entry is validated, and only after that are the
files renamed into place and the manifest written. Publication is transactional
across the seven files plus the manifest: before a destination is replaced, any
existing file there is moved to a hidden backup in the same directory, and the
manifest is written atomically only after every file is in place. If any step
after validation fails — a rename, a directory where a file belongs, or the
manifest write — the installer removes the files it already published, restores
the previous binary, icons and entry from those backups (contents and modes),
leaves the previous manifest untouched and exits non-zero with a clear error. A
first-time install that fails therefore leaves no partial files or manifest
behind, and a rejected entry publishes nothing and replaces nothing; the
script's exit trap removes the staged temp files. The backups are deleted only
after the manifest rename has committed. `--dry-run` follows the same order but
prints the operations instead of running them and never creates, backs up,
renames or deletes anything.

The installer writes `install-manifest` atomically (a temp file in the same
directory, then a rename) after every staged file has been published and the
entry validated; that rename is the transaction's commit point, so if it fails
the published files are rolled back too. It is a small text file with `#`
comments and one absolute path per line, listing every path the install created
(binary, entry, five icons).
`--uninstall` reads that manifest and
removes only its listed paths that still lie inside the installer's `bin`/`data`
roots, then removes the manifest last; anything else in the prefix is left
alone (empty directories may remain). If the manifest is missing but files exist
at Rift's fixed paths (a hand-installed copy, or a stale install from before
manifests), uninstall refuses with an instruction to re-run with `--force`;
`--force` prints a warning and removes only the fixed binary, entry and icon
paths, never anything else. If neither the manifest nor any fixed path exists,
uninstall is a successful no-op, so repeating it is idempotent. `--dry-run`
performs no writes in either mode.

The installed entry's `Exec` is always double-quoted. The Desktop Entry
Specification applies two escaping layers in order — the Exec quoted-string
layer (one backslash before `"`, `` ` ``, `$` and `\`), then the general string
layer (every backslash doubled) — so the file contains **four** backslashes for
a literal `\` and **two** for each of `"`, `` ` `` and `$` (`\\"`, `` \\` ``,
`\\$`). Doing only one layer leaves the entry unloadable by GLib (`gio launch`
rejects it). `%` is a field-code marker and GLib rejects the `%%` workaround as
well, so a `%` in the installed binary path is rejected with a clear error before
anything is written.

An explicit `--binary` only has to be a readable regular file: `install -Dm755`
sets the installed mode, so a release asset downloaded with mode `0644` works
without `chmod +x`. Auto-discovered `target/release/rift-music` and
`target/debug/rift-music` still have to be executable.

The installer then refreshes `update-desktop-database` / `gtk-update-icon-cache`
/ `kbuildsycoca6` (or `kbuildsycoca5`) best-effort, when those tools exist (skip
that step with `--no-cache-refresh`; the guard tests pass it so running
`cargo test` never rebuilds a developer's real caches). When
`desktop-file-validate` is installed the staged entry is validated before
publication, and a rejection fails the install before anything is published,
without leaving an entry or a manifest behind (the exit trap removes the staged
entry).

### Launcher entry decisions

* `SingleMainWindow=true` — the process-wide single-instance guard
  (`single_instance.rs`) means a second launch exits by design (no focus IPC);
  the entry tells the shell the app has one main window.
* `StartupNotify=false` — the app has no XDG startup-notification support, so
  launchers must not wait for an activation event that never arrives.
* `Exec=rift-music` is the bare binary name with **no `%U`/`%F` field codes**:
  the installer rebases it, and `main.rs`'s `Args::parse` rejects any positional
  argument (`unknown argument "…" (try --help)`) before the window or GPU work
  starts, so a field code expanded by the launcher would make every menu click
  exit with an error.
* `Categories=AudioVideo;Audio;Player;` — `AudioVideo` is the single main
  category; a second main category (such as `Network`) makes
  `desktop-file-validate` complain and can list the app twice in menus. The rest
  of the field set: `Type=Application`, `Terminal=false`, `Version=1.5`,
  `Name=Rift`, `GenericName`/`Comment`/`Keywords`.

### Icon theme layout

The installer maps five existing PNG assets into hicolor theme slots (the same
table as the guard test's `ICON_ASSETS`):

| Theme slot | Asset |
|---|---|
| `hicolor/32x32/apps/rift-music.png` | `assets/icons/32x32.png` |
| `hicolor/64x64/apps/rift-music.png` | `assets/icons/64x64.png` |
| `hicolor/128x128/apps/rift-music.png` | `assets/icons/128x128.png` |
| `hicolor/256x256/apps/rift-music.png` | `assets/icons/128x128@2x.png` |
| `hicolor/512x512/apps/rift-music.png` | `assets/icons/icon.png` |

### Guard tests

`crates/rift-desktop/tests/desktop_entry.rs` is a normal test target — part of
plain `cargo test`, on every host:

* parses the committed entry strictly (single `[Desktop Entry]` group, no
  duplicate keys, no key before the header) and asserts the required fields,
  the identity agreement and a well-formed category list;
* asserts `Exec` is the bare binary name with no field codes or whitespace;
* asserts the entry is ASCII with LF endings and a final newline;
* asserts every icon asset is a real PNG whose dimensions match its hicolor
  slot;
* (`cfg(target_os = "linux")`) runs the installer into a throwaway `--prefix`
  with `--binary` pointing at the test-built executable, asserts the **exact raw
  `Exec=` encoding** for a path containing every reserved character, checks the
  installed binary, five icons and the manifest's path list, asserts the
  installed entry's file stem is `APP_ID` and its `Icon=` is `BINARY_NAME`, reads
  each installed icon's IHDR to check its dimensions match the hicolor slot,
  appends a path outside the install roots to the manifest to prove it is
  skipped, then runs `--uninstall` and checks its own files (including the icons
  and the manifest) are gone while decoy files next to them survive, and that a
  second uninstall is a successful no-op;
* runs the installer with a stub `desktop-file-validate` on `PATH` that rejects
  every entry, and asserts the failed install leaves a previous binary, entry
  and manifest byte-for-byte intact, publishes no icon or new binary, leaves a
  fresh prefix with no entry/manifest at all, and removes its staged temp files;
* fails publication after the binary and icons were already renamed into place
  by replacing the installed entry destination with a non-empty directory, and
  asserts the failed reinstall restores the previous binary, icons and manifest
  byte-for-byte and mode-for-mode, leaves the foreign entry directory untouched
  and removes its staging/backup files; repeats that failure on a first install
  and asserts no binary, icon or manifest is published; and makes the final
  manifest rename fail (a stub `mv` that delegates every other call) to assert
  the already-published binary, entry and icons are rolled back to the previous
  install;
* asserts uninstall without a manifest refuses to delete a decoy
  `bin/rift-music` + unrelated `Rift.desktop`, that `--dry-run --force` still
  changes nothing, and that `--force` then removes only the fixed paths;
* asserts a `%` in the install path is rejected before anything is written;
* installs from a non-executable (mode `0644`) copy of the binary, checks the
  installed mode is `0755`, and checks the source mode is untouched;
* sets `HOME`, `XDG_DATA_HOME` and `XDG_BIN_HOME` on the child process and
  checks the default locations, the `$HOME/.local` fallbacks when the XDG
  variables are unset, and that an explicit `--prefix` wins for both install and
  uninstall.

The installer tests are Linux-only; the portable checks keep the committed
entry, the identity constants and the icon assets in step everywhere.

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
