//! Clipboard backends for the text input.
//!
//! The UI only sees the [`Clipboard`] trait. By default [`Ui`](crate::ui::Ui)
//! lazily creates the native backend ([`NativeClipboard`], backed by `arboard`)
//! the first time a copy/paste/select-all happens, so:
//!
//! * headless tests and CI never *need* a display/clipboard service: the backend
//!   is created on demand, and the tests that exercise the real OS paths skip
//!   when it is unavailable. The OS integration tests (Windows/Linux) are opt-in
//!   with `RIFT_TEST_OS_CLIPBOARD=1`: they write the real clipboard on purpose,
//!   serialised and with best-effort restore, and a Wayland selection can outlive
//!   the test. The default tests use the in-memory backend.
//! * a live window gets real Ctrl+C/Ctrl+V support with no wiring in `main.rs`,
//! * failures to initialise (no clipboard service, restricted session) degrade
//!   to no-ops instead of panicking.
//!
//! # Linux: Wayland and X11
//!
//! `arboard` is built with its `wayland-data-control` feature (see the
//! `crates/rift-desktop/Cargo.toml` comment): on a Wayland session the native
//! backend talks to the compositor directly, which is the only way it can see
//! selections copied by other Wayland clients. The X11-only configuration is
//! not enough there — an X11 clipboard connection sees only the XWayland
//! clipboard, and on KDE a selection copied by a Wayland app is not mirrored
//! into it, so Ctrl+V silently did nothing (a YouTube Music link copied from
//! the browser could not be pasted into the sidebar's import box). When
//! `WAYLAND_DISPLAY` is unset, or the compositor does not implement
//! data-control (GNOME/Mutter), `arboard` falls back to X11 on its own, so the
//! X11/XWayland path is unchanged on those desktops.
//!
//! [`MemoryClipboard`] is a deterministic in-process implementation for tests
//! and for preview/screenshot mode.

use std::sync::{Arc, Mutex};

/// A clipboard bridge. Implement this to inject a custom backend with
/// [`Ui::set_clipboard`](crate::ui::Ui::set_clipboard).
pub trait Clipboard {
    fn get_text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: &str);
}

/// An in-process clipboard (no OS access). Cheap to clone/share between owners;
/// useful for deterministic tests and preview mode.
#[derive(Clone, Default)]
pub struct MemoryClipboard {
    shared: Arc<Mutex<Option<String>>>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the currently stored text without going through the trait.
    pub fn text(&self) -> Option<String> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Seed the clipboard directly.
    pub fn set(&self, text: &str) {
        *self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(text.to_string());
    }
}

impl Clipboard for MemoryClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.text()
    }

    fn set_text(&mut self, text: &str) {
        self.set(text);
    }
}

/// The native OS clipboard (Windows / macOS / Linux X11/Wayland via `arboard`).
///
/// On Linux the `arboard` build enables `wayland-data-control`, so a Wayland
/// session uses the compositor's clipboard and only desktops without that
/// protocol (or sessions without `WAYLAND_DISPLAY`) fall back to X11 — see the
/// module docs.
///
/// Construction is fallible and happens lazily in [`Ui`](crate::ui::Ui): when
/// `arboard` cannot connect to the platform clipboard service the backend simply
/// reports [`Self::is_available`] `= false` and every operation is a no-op.
pub struct NativeClipboard {
    inner: Option<arboard::Clipboard>,
}

impl Default for NativeClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeClipboard {
    /// Try to connect to the OS clipboard. Never panics.
    pub fn new() -> Self {
        Self {
            inner: arboard::Clipboard::new().ok(),
        }
    }

    /// Whether the OS clipboard is usable in this session.
    pub fn is_available(&self) -> bool {
        self.inner.is_some()
    }

    /// Clear the OS clipboard (used between frames/tests to restore state).
    pub fn clear(&mut self) {
        if let Some(clipboard) = self.inner.as_mut() {
            let _ = clipboard.clear();
        }
    }
}

impl Clipboard for NativeClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.inner.as_mut()?.get_text().ok()
    }

    fn set_text(&mut self, text: &str) {
        if let Some(clipboard) = self.inner.as_mut() {
            let _ = clipboard.set_text(text.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OS clipboard is process/thread-global on Windows, and on Linux it is
    /// a single selection owned by one process at a time; the tests that touch
    /// it must not run concurrently (cargo runs tests in parallel). Non-OS tests
    /// (`MemoryClipboard`) never take this lock.
    ///
    /// The OS-touching tests write to the real clipboard and put the previous
    /// contents back, but on Wayland the selection is owned by the writing
    /// process: the restore does not outlive the test process, so a local
    /// `cargo test` run can leave a test marker on the desktop clipboard (KDE
    /// keeps it in the clipboard-manager history) until the next copy. That is
    /// acceptable for a developer run; CI is headless and skips these tests.
    ///
    /// The in-process lock cannot serialise *separate* test processes: two
    /// concurrent `cargo test` runs on the same desktop share one OS clipboard
    /// and can overwrite each other's marker, so run the clipboard tests from
    /// one invocation at a time when checking flakiness.
    #[cfg(any(windows, target_os = "linux"))]
    static NATIVE_CLIPBOARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Read the clipboard until it returns `want` (or the timeout), returning the
    /// last read.
    ///
    /// A Wayland clipboard write is queued on the compositor's protocol stream:
    /// `arboard`'s Wayland `set_text` returns after `set_selection` is queued,
    /// not after the compositor round-trip, so an immediately following read can
    /// still see the previous selection. Write-then-read assertions therefore
    /// poll instead of assuming synchronous delivery; on X11/Windows the first
    /// read already matches (one extra call, no sleep).
    #[cfg(any(windows, target_os = "linux"))]
    fn poll_clipboard(
        mut read: impl FnMut() -> Option<String>,
        want: Option<&str>,
    ) -> Option<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let got = read();
            if got.as_deref() == want || std::time::Instant::now() >= deadline {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Put the previous contents back and wait until the restore is visible.
    ///
    /// Waiting matters beyond the assertion: an asynchronous Wayland
    /// `set_text` still in flight when the test returns could land after the
    /// next test's write and make *that* test read the stale value. An empty
    /// clipboard has nothing to wait for — clearing is a Wayland roundtrip, and
    /// a clipboard manager (KDE's klipper) may re-own the selection with its
    /// last history entry immediately after the clear.
    #[cfg(any(windows, target_os = "linux"))]
    fn restore_native(native: &mut NativeClipboard, previous: Option<String>) {
        let Some(text) = previous else {
            native.clear();
            return;
        };
        native.set_text(&text);
        let _ = poll_clipboard(|| native.get_text(), Some(text.as_str()));
    }

    /// [`restore_native`] for the external `arboard` handle the UI test uses to
    /// prove the copy really reached the platform clipboard.
    #[cfg(any(windows, target_os = "linux"))]
    fn restore_arboard(clipboard: &mut arboard::Clipboard, previous: Option<String>) {
        let Some(text) = previous else {
            let _ = clipboard.clear();
            return;
        };
        let _ = clipboard.set_text(text.clone());
        let _ = poll_clipboard(|| clipboard.get_text().ok(), Some(text.as_str()));
    }

    /// OS-clipboard tests are opt-in: they write the developer's real clipboard
    /// (and a Wayland selection can outlive the test), so the default suite must
    /// not touch it. CI is headless and would skip anyway; set
    /// `RIFT_TEST_OS_CLIPBOARD=1` to exercise them locally.
    #[cfg(any(windows, target_os = "linux"))]
    fn os_clipboard_tests_enabled() -> bool {
        std::env::var_os("RIFT_TEST_OS_CLIPBOARD")
            .is_some_and(|value| !value.is_empty() && value != "0")
    }

    #[test]
    fn memory_clipboard_roundtrips_and_shares_state() {
        let mut a = MemoryClipboard::new();
        assert_eq!(a.get_text(), None);
        a.set_text("hello");
        assert_eq!(a.get_text().as_deref(), Some("hello"));
        // Clones observe the same buffer (that is how a test can read back what
        // the UI copied).
        let peer = a.clone();
        assert_eq!(peer.text().as_deref(), Some("hello"));
        a.set_text("world");
        assert_eq!(peer.text().as_deref(), Some("world"));
    }

    /// On a machine with a working OS clipboard: exercise the backend through
    /// the trait and restore the previous clipboard contents afterwards.
    ///
    /// Windows/Linux only, matching `os_clipboard_tests_enabled`; macOS keeps
    /// the in-memory test only (the real-clipboard tests are opt-in where the
    /// helper is compiled).
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn native_clipboard_roundtrips_when_available() {
        if !os_clipboard_tests_enabled() {
            return;
        }
        let _clipboard_lock = NATIVE_CLIPBOARD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut native = NativeClipboard::new();
        if !native.is_available() {
            // Headless CI: no clipboard service. Must be a silent no-op.
            assert_eq!(native.get_text(), None);
            native.set_text("ignored");
            assert!(!native.is_available() || native.get_text().is_some());
            return;
        }
        let previous = native.get_text();
        let marker = format!("rift-clipboard-test-{}", std::process::id());
        native.set_text(&marker);
        let read_back = poll_clipboard(|| native.get_text(), Some(marker.as_str()));
        assert_eq!(
            read_back.as_deref(),
            Some(marker.as_str()),
            "the native clipboard must round-trip text"
        );
        restore_native(&mut native, previous);
    }

    /// Full path through `Ui`: paste from the real OS clipboard, then copy back
    /// to it. Windows and Linux; silently skips when no clipboard service exists
    /// (headless CI) and restores the previous clipboard contents best-effort
    /// (see the lock's note: a Wayland selection does not outlive the process).
    ///
    /// On Linux this also covers the Wayland backend: with the
    /// `wayland-data-control` feature enabled, both `arboard` handles here (the
    /// test's and the one `Ui` creates lazily) go through the compositor, which
    /// is the path that makes Ctrl+V see what other Wayland apps copied.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn ui_pastes_from_and_copies_to_the_real_os_clipboard() {
        if !os_clipboard_tests_enabled() {
            return;
        }
        let _clipboard_lock = NATIVE_CLIPBOARD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        use super::super::{Id, InputState, Key, KeyInput, Modifiers, Rect, TextInputStyle, Ui};

        let mut os = match arboard::Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(_) => return, // headless CI: nothing to test
        };
        let previous = os.get_text().ok();
        let id = Id::from_str("clipboard-test/input");
        let marker = format!("rift-ui-clipboard-{}", std::process::id());
        if os.set_text(marker.clone()).is_err() {
            return; // clipboard locked by another process; not a failure
        }

        let mut ui = Ui::new();
        ui.request_focus(id);
        // A Wayland write is queued, not synchronous: ensure the app's own
        // clipboard connection sees the marker *before* the paste frame,
        // otherwise Ctrl+V could read the previous selection.
        let visible = poll_clipboard(|| ui.clipboard_text(), Some(marker.as_str()));
        assert_eq!(
            visible.as_deref(),
            Some(marker.as_str()),
            "the app clipboard must see the OS copy before Ctrl+V"
        );
        let viewport = Rect::new(0.0, 0.0, 400.0, 80.0);
        let rect = Rect::new(10.0, 10.0, 300.0, 33.0);
        let style = TextInputStyle::default_theme(&ui.theme().clone());
        let mut text = String::new();

        let run = |ui: &mut Ui, text: &mut String, keys: &[Key], ctrl: bool, time: f64| {
            let mut input = InputState {
                viewport,
                scale_factor: 1.0,
                time,
                delta_time: 1.0 / 60.0,
                mouse_in_window: true,
                ..InputState::default()
            };
            input.modifiers = Modifiers {
                ctrl,
                ..Modifiers::default()
            };
            input.keys = keys
                .iter()
                .map(|&key| KeyInput {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers: input.modifiers,
                })
                .collect();
            ui.begin_frame(input, viewport);
            ui.text_input(rect, id, text, "Placeholder", &style);
            let _ = ui.end_frame();
        };

        // Paste frame: Ctrl+V must insert the OS clipboard text.
        run(&mut ui, &mut text, &[Key::Char('v')], true, 0.1);
        assert_eq!(text, marker, "Ctrl+V must paste the OS clipboard text");

        // Copy frame: Ctrl+A then Ctrl+C must put the text back on the OS
        // clipboard. First replace the clipboard with a *different* sentinel:
        // otherwise it already holds `marker` from the setup above and the copy
        // assertion would pass even if Ctrl+C did nothing.
        let before_copy = format!("{marker}-before-copy");
        if os.set_text(before_copy.clone()).is_err() {
            return;
        }
        assert_eq!(
            poll_clipboard(|| os.get_text().ok(), Some(before_copy.as_str())).as_deref(),
            Some(before_copy.as_str()),
            "the overwrite sentinel must be visible before testing Ctrl+C"
        );
        run(&mut ui, &mut text, &[Key::Char('a')], true, 0.2);
        run(&mut ui, &mut text, &[Key::Char('c')], true, 0.3);
        // Read it with the *external* handle to prove it really is the platform
        // clipboard, not our in-process copy.
        let copied = poll_clipboard(|| os.get_text().ok(), Some(marker.as_str()));
        assert_eq!(
            copied.as_deref(),
            Some(marker.as_str()),
            "Ctrl+C must write to the OS clipboard"
        );
        restore_arboard(&mut os, previous);
    }

    /// A selection copied by another Wayland app must be visible to the app's
    /// clipboard. This is the regression the `wayland-data-control` feature
    /// exists for: with arboard's X11-only Linux build, `get_text` returned
    /// `ContentNotAvailable` for a Wayland-owned selection, so Ctrl+V in the
    /// sidebar's "import playlist" box silently did nothing for a link copied
    /// from a browser on a Wayland desktop.
    ///
    /// Skips (never fails) when the environment has no Wayland session or a
    /// compositor without data-control (GNOME/Mutter, headless CI), and restores
    /// the previous clipboard contents.
    #[cfg(target_os = "linux")]
    #[test]
    fn native_clipboard_reads_a_selection_owned_by_a_wayland_client() {
        if !os_clipboard_tests_enabled() {
            return;
        }
        use wl_clipboard_rs::copy::{MimeType, Options, Source};

        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return; // not a Wayland session: the Wayland path cannot be tested
        }
        let _clipboard_lock = NATIVE_CLIPBOARD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let marker = format!("rift-wayland-selection-{}", std::process::id());
        let mut native = NativeClipboard::new();
        // Before overwriting the selection: the contents to restore afterwards.
        let previous = native.get_text();

        // `copy` returns once the selection is claimed and keeps a background
        // thread serving pastes until another Wayland client copies (our restore
        // below), so this is not blocking. The claim is queued asynchronously;
        // `poll_clipboard` waits for it to become readable.
        let copied = Options::new().copy(
            Source::Bytes(marker.clone().into_bytes().into_boxed_slice()),
            MimeType::Text,
        );
        if copied.is_err() {
            return; // compositor without data-control: nothing observable
        }

        let pasted = poll_clipboard(|| native.get_text(), Some(marker.as_str()));
        // Restoring destroys the test's Wayland data source, so the background
        // serving thread exits and cannot leak into another test.
        restore_native(&mut native, previous);
        assert_eq!(
            pasted.as_deref(),
            Some(marker.as_str()),
            "the app clipboard must see a selection copied by a Wayland client"
        );
    }

    /// The UI must not panic when the native clipboard cannot be created (the
    /// headless/CI path): copy/paste simply leave the bound text untouched.
    #[test]
    fn ui_tolerates_an_unavailable_native_clipboard() {
        use super::super::{Id, InputState, Key, KeyInput, Modifiers, Rect, TextInputStyle, Ui};
        let mut ui = Ui::new();
        // Force the unavailable path deterministically even on a desktop.
        ui.set_native_clipboard_enabled(false);
        assert!(!ui.native_clipboard_available());
        let id = Id::from_str("clipboard-test/disabled");
        ui.request_focus(id);
        let viewport = Rect::new(0.0, 0.0, 400.0, 80.0);
        let rect = Rect::new(10.0, 10.0, 300.0, 33.0);
        let style = TextInputStyle::default_theme(&ui.theme().clone());
        let mut text = String::from("abc");
        for (time, keys) in [
            (0.1, vec![Key::Char('a')]),
            (0.2, vec![Key::Char('c')]),
            (0.3, vec![Key::Char('v')]),
        ] {
            let mut input = InputState {
                viewport,
                scale_factor: 1.0,
                time,
                delta_time: 1.0 / 60.0,
                mouse_in_window: true,
                ..InputState::default()
            };
            input.modifiers = Modifiers {
                ctrl: true,
                ..Modifiers::default()
            };
            input.keys = keys
                .into_iter()
                .map(|key| KeyInput {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers: input.modifiers,
                })
                .collect();
            ui.begin_frame(input, viewport);
            ui.text_input(rect, id, &mut text, "Placeholder", &style);
            let _ = ui.end_frame();
        }
        // With no clipboard: Ctrl+A selects, Ctrl+C does nothing, Ctrl+V does
        // nothing, so the text is unchanged.
        assert_eq!(text, "abc");
    }
}
