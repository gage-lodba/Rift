//! The minimal single-line text field.
//!
//! Superseded by the framework's `ui::text_input` (which every screen now uses),
//! but kept and re-exported from [`super`] so the published `screens::cards::*`
//! surface stays stable. New code should use `ui::text_input`.

use crate::text::TextStyle;
use crate::ui::{Color, Id, Rect, Response, Sense, Ui};

/// A minimal single-line editor with the caret at the end.
///
/// Kept for API stability only — the framework's `ui::text_input` replaced this
/// widget long ago and is what the screens use. Do not add callers; delete the
/// helper (and its `cards` re-export) when the published surface is next revised.
pub fn text_field(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    value: &mut String,
    placeholder: &str,
    text_style: &TextStyle,
    placeholder_color: Color,
) -> Response {
    let theme = ui.theme().clone();
    let resp = ui.interact(rect, id, Sense::FOCUS);
    let focused = ui.focused_id() == Some(id);
    let mut changed = false;
    if resp.hovered && ui.input().mouse_left_pressed {
        ui.request_focus(id);
    }
    if focused {
        // Text + editing keys. These are one-frame edges; the app applies the
        // action after the frame, so the caller renders from its own buffer.
        let text: String = ui.input().text.clone();
        if !text.is_empty() {
            value.push_str(&text);
            changed = true;
        }
        for key in ui.input().keys.clone() {
            if !key.pressed {
                continue;
            }
            match key.key {
                crate::ui::Key::Backspace => {
                    value.pop();
                    changed = true;
                }
                crate::ui::Key::Escape => {
                    value.clear();
                    ui.clear_focus();
                    changed = true;
                }
                _ => {}
            }
        }
    }
    let display = if value.is_empty() {
        placeholder.to_string()
    } else {
        value.clone()
    };
    let color = if value.is_empty() {
        placeholder_color
    } else {
        text_style.color
    };
    let inner = rect.inset_xy(12.0, 0.0);
    ui.text(inner, &display, &text_style.with_color(color));
    if focused {
        let w = if value.is_empty() {
            0.0
        } else {
            ui.text_width(value, text_style)
        };
        let caret_x = (inner.x + w).min(inner.right() - 1.0);
        ui.vline(caret_x, inner.y + 3.0, inner.h - 6.0, 1.0, theme.focus);
    }
    let mut resp = resp;
    resp.clicked |= changed;
    resp
}
