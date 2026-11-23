//! Unit tests for the split text-input module.

use super::edit::{paste_text, MAX_PASTE_BYTES};
use super::*;
use crate::ui::clipboard::MemoryClipboard;
use crate::ui::{InputState, Key, KeyInput, Modifiers};

fn paste_frame(ui: &mut Ui, id: Id, text: &mut String, paste: &str) {
    let clipboard = MemoryClipboard::new();
    clipboard.set(paste);
    ui.set_clipboard(Box::new(clipboard));

    let viewport = Rect::new(0.0, 0.0, 400.0, 80.0);
    let mut input = InputState {
        viewport,
        scale_factor: 1.0,
        time: 0.016,
        delta_time: 1.0 / 60.0,
        mouse_in_window: true,
        ..InputState::default()
    };
    input.modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    input.keys = vec![KeyInput {
        key: Key::Char('v'),
        pressed: true,
        repeat: false,
        modifiers: input.modifiers,
    }];
    let style = TextInputStyle::default_theme(&ui.theme().clone());
    ui.begin_frame(input, viewport);
    let _ = ui.text_input(
        Rect::new(10.0, 10.0, 300.0, 33.0),
        id,
        text,
        "Placeholder",
        &style,
    );
    let _ = ui.end_frame();
}

/// A multi-megabyte Ctrl+V must not be inserted wholesale: the widget caps
/// it at [`MAX_PASTE_BYTES`] and cuts on a UTF-8 char boundary.
#[test]
fn paste_through_the_text_input_is_capped_on_a_char_boundary() {
    let mut ui = Ui::new();
    let id = Id::from_str("paste-cap/input");
    ui.request_focus(id);
    // Three times the cap of a 2-byte char.
    let huge = "é".repeat(MAX_PASTE_BYTES * 3 / 2);
    let mut text = String::new();
    paste_frame(&mut ui, id, &mut text, &huge);

    assert!(
        text.len() <= MAX_PASTE_BYTES,
        "paste must be bounded, got {} bytes",
        text.len()
    );
    assert!(text.is_char_boundary(text.len()), "cut splits a char");
    assert!(
        text.chars().all(|ch| ch == 'é'),
        "text is still valid UTF-8"
    );
    assert_eq!(text.len(), MAX_PASTE_BYTES, "2-byte chars fit exactly");
    assert_eq!(text.chars().count(), MAX_PASTE_BYTES / 2);
}

#[test]
fn push_ime_text_strips_control_characters_and_keeps_multibyte_text() {
    let mut input = InputState::default();

    // A CJK composition commit with an embedded control/NUL byte: only the
    // visible characters are appended.
    push_ime_text(&mut input, "日本語\u{0000}\n한국어");
    assert_eq!(input.text, "日本語한국어");

    // Appends (does not replace) and keeps astral-plane characters intact.
    push_ime_text(&mut input, "🎵é");
    assert_eq!(input.text, "日本語한국어🎵é");

    // A commit that is only control characters (e.g. Enter sanitized by the
    // IME) appends nothing.
    push_ime_text(&mut input, "\r\t\u{0007}");
    assert_eq!(input.text, "日本語한국어🎵é");
}

#[test]
fn paste_helper_trims_control_bytes_and_never_splits_a_char() {
    // One byte short of the cap, then 2-byte chars: none of them fit.
    let raw = format!("{}{}", "a".repeat(MAX_PASTE_BYTES - 1), "ééé");
    let (out, truncated) = paste_text(&raw);
    assert!(truncated);
    assert_eq!(out.len(), MAX_PASTE_BYTES - 1);
    assert!(out.is_char_boundary(out.len()));

    // Control characters are dropped and do not count toward the cap.
    let (out, truncated) = paste_text("a\nb\0c");
    assert_eq!(out, "abc");
    assert!(!truncated);

    // Exactly the cap fits without truncation.
    let exact = "a".repeat(MAX_PASTE_BYTES);
    let (out, truncated) = paste_text(&exact);
    assert_eq!(out.len(), MAX_PASTE_BYTES);
    assert!(!truncated);
}
