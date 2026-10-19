//! Framework-agnostic input state handed to the immediate-mode UI each frame.
//!
//! `main.rs` translates winit events into this struct; the UI (`crates/rift-desktop/src/ui/`)
//! only ever sees these winit-agnostic types. Mouse coordinates are logical
//! pixels with the origin at the window's top-left (same space the UI lays out
//! in).
//!
//! Per-frame edges (`*_pressed`, `*_released`, `focus_lost`, `scroll`, `keys`,
//! `text`) are valid for exactly one frame; call [`InputState::end_frame`] after
//! the UI has consumed the frame.

use crate::gfx::paint::Rect;

/// Keyboard modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

/// A named key. Text-producing keys are also represented as `Char`, which is
/// what text inputs consume; `Char` is only reported when no modifier (besides
/// shift) is held, so shortcuts win over typing, matching the old UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Space,
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Char(char),
}

/// One key transition for this frame.
#[derive(Clone, Copy, Debug)]
pub struct KeyInput {
    pub key: Key,
    pub pressed: bool,
    /// OS key-repeat (holding the key down).
    pub repeat: bool,
    pub modifiers: Modifiers,
}

/// Full input snapshot for one frame.
#[derive(Clone, Debug)]
pub struct InputState {
    /// Window client area in logical px.
    pub viewport: Rect,
    pub scale_factor: f32,
    /// Seconds since application start (monotonic).
    pub time: f64,
    pub delta_time: f32,
    /// Pointer position in logical px; can be outside the window.
    pub mouse_pos: (f32, f32),
    pub mouse_in_window: bool,
    pub mouse_left_down: bool,
    pub mouse_right_down: bool,
    pub mouse_middle_down: bool,
    /// Click edges (true only on the frame the button went down).
    pub mouse_left_pressed: bool,
    pub mouse_right_pressed: bool,
    pub mouse_middle_pressed: bool,
    /// Release edges.
    pub mouse_left_released: bool,
    pub mouse_right_released: bool,
    pub mouse_middle_released: bool,
    /// Set for exactly one frame when the window lost focus (alt-tab, click on
    /// another application, lock screen).
    ///
    /// The OS releases held keys/buttons while we are not focused, so their
    /// release edges never arrive. The UI uses this flag to drop stale
    /// press/drag latches: without it the first click after refocusing is
    /// swallowed by the previous interaction's `active` latch. It is **not** a
    /// synthetic release edge; the matching release frame is simply dropped.
    /// Consumed by [`InputState::end_frame`], so it cannot outlive its frame.
    pub focus_lost: bool,
    /// Wheel/trackpad delta in logical px (positive x = right, positive y =
    /// down/content-scroll convention used by winit).
    pub scroll: (f32, f32),
    pub modifiers: Modifiers,
    /// Key transitions this frame, in order.
    pub keys: Vec<KeyInput>,
    /// Typed text this frame (IME/character input), in order.
    pub text: String,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            scale_factor: 1.0,
            time: 0.0,
            delta_time: 0.0,
            mouse_pos: (0.0, 0.0),
            mouse_in_window: false,
            mouse_left_down: false,
            mouse_right_down: false,
            mouse_middle_down: false,
            mouse_left_pressed: false,
            mouse_right_pressed: false,
            mouse_middle_pressed: false,
            mouse_left_released: false,
            mouse_right_released: false,
            mouse_middle_released: false,
            focus_lost: false,
            scroll: (0.0, 0.0),
            modifiers: Modifiers::default(),
            keys: Vec::new(),
            text: String::new(),
        }
    }
}

impl InputState {
    /// Whether a key went down this frame (including OS repeats).
    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys.iter().any(|k| k.pressed && k.key == key)
    }

    /// Whether a key went down this frame (ignoring OS repeats).
    pub fn key_clicked(&self, key: Key) -> bool {
        self.keys
            .iter()
            .any(|k| k.pressed && !k.repeat && k.key == key)
    }

    /// Whether the key is currently held (the last event for it was a press).
    pub fn key_down(&self, key: Key) -> bool {
        self.keys
            .iter()
            .rev()
            .find(|k| k.key == key)
            .map(|k| k.pressed)
            .unwrap_or(false)
    }

    /// `ctrl` on Windows/Linux, `cmd` on macOS.
    pub fn command(&self) -> bool {
        self.modifiers.ctrl || self.modifiers.super_key
    }

    /// Clear one-frame edges; called after the UI consumed the frame.
    pub fn end_frame(&mut self) {
        self.mouse_left_pressed = false;
        self.mouse_right_pressed = false;
        self.mouse_middle_pressed = false;
        self.mouse_left_released = false;
        self.mouse_right_released = false;
        self.mouse_middle_released = false;
        self.focus_lost = false;
        self.scroll = (0.0, 0.0);
        self.keys.clear();
        self.text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_lost_defaults_to_false() {
        assert!(!InputState::default().focus_lost);
    }

    /// The flag is a one-frame edge like the button presses: after the UI has
    /// consumed the frame, it is cleared.
    #[test]
    fn end_frame_consumes_focus_lost() {
        let mut input = InputState {
            focus_lost: true,
            mouse_left_released: true,
            keys: vec![KeyInput {
                key: Key::Space,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }],
            text: String::from("x"),
            ..InputState::default()
        };

        input.end_frame();

        assert!(!input.focus_lost, "focus_lost is consumed exactly once");
        assert!(!input.mouse_left_released, "release edges are one-shot too");
        assert!(input.keys.is_empty() && input.text.is_empty());
    }
}
