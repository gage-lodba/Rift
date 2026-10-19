//! Clipboard backends for the text input.
//!
//! The UI only sees the [`Clipboard`] trait. By default [`Ui`](crate::ui::Ui)
//! lazily creates the native backend ([`NativeClipboard`], backed by `arboard`)
//! the first time a copy/paste/select-all happens, so:
//!
//! * headless tests and CI never need a display/clipboard service (the backend
//!   is only created on demand, and `cargo test` disables it explicitly where it
//!   would otherwise touch the OS),
//! * a live window gets real Ctrl+C/Ctrl+V support with no wiring in `main.rs`,
//! * failures to initialise (no X11/Wayland, restricted session) degrade to
//!   no-ops instead of panicking.
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

    /// The OS clipboard is process/thread-global on Windows; the native tests
    /// must not run concurrently with each other (cargo runs tests in parallel).
    #[cfg(windows)]
    static NATIVE_CLIPBOARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    #[test]
    fn native_clipboard_roundtrips_when_available() {
        #[cfg(windows)]
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
        assert_eq!(
            native.get_text().as_deref(),
            Some(marker.as_str()),
            "the native clipboard must round-trip text"
        );
        match previous {
            Some(text) => native.set_text(&text),
            None => native.clear(),
        }
    }

    /// Full path through `Ui`: paste from the real OS clipboard, then copy back
    /// to it. Windows-only; silently skips when no clipboard service exists (CI)
    /// and restores the previous clipboard contents.
    #[cfg(windows)]
    #[test]
    fn ui_pastes_from_and_copies_to_the_real_os_clipboard() {
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
        // clipboard. Read it with the *external* handle to prove it really is
        // the platform clipboard, not our in-process copy.
        run(&mut ui, &mut text, &[Key::Char('a')], true, 0.2);
        run(&mut ui, &mut text, &[Key::Char('c')], true, 0.3);
        assert_eq!(
            os.get_text().ok().as_deref(),
            Some(marker.as_str()),
            "Ctrl+C must write to the OS clipboard"
        );

        match previous {
            Some(text) => {
                let _ = os.set_text(text);
            }
            None => {
                let _ = os.clear();
            }
        }
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
