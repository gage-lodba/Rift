//! Section headings (`<h2>` and the heading + trailing action variant).

use crate::text::Weight;
use crate::ui::{Id, Rect, Response, Sense, Ui};

use super::{style, style_lh, H2_LINE, H2_MARGIN_BOTTOM};

// ---------------------------------------------------------------------------
// section headings
// ---------------------------------------------------------------------------

/// An `<h2>` section heading. Draws the 22 px line box at `y` (with `x`/`w` from
/// the content column) and returns the y below its 14 px bottom margin.
pub fn section_header(ui: &mut Ui, x: f32, y: f32, w: f32, label: &str) -> f32 {
    let theme = ui.theme().clone();
    let st = style_lh(17.0, Weight::Bold, theme.text, H2_LINE);
    ui.text(Rect::new(x, y, w, H2_LINE), label, &st);
    y + H2_LINE + H2_MARGIN_BOTTOM
}

/// A section heading with a trailing text action (`"Show all"`), returns the
/// rectangle of the action so the caller can test it.
pub fn section_header_action(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    label: &str,
    action_label: &str,
) -> (f32, Response) {
    let theme = ui.theme().clone();
    let st = style_lh(17.0, Weight::Bold, theme.text, H2_LINE);
    ui.text(Rect::new(x, y, w, H2_LINE), label, &st);
    let action_w =
        ui.text_width(action_label, &style(13.0, Weight::SemiBold, theme.text_dim)) + 12.0;
    let action_rect = Rect::new(x + w - action_w, y + 2.0, action_w, H2_LINE - 4.0);
    let id = Id::from_str("section-header-action").with(label);
    let resp = ui.interact(action_rect, id, Sense::CLICK);
    if resp.hovered {
        ui.text_centered(
            action_rect,
            action_label,
            &style(13.0, Weight::SemiBold, theme.text),
        );
        // Underline on hover, like `.link-btn:hover`.
        let ty = action_rect.y + action_rect.h - 3.0;
        ui.hline(action_rect.x + 6.0, ty, action_w - 12.0, 1.0, theme.text);
    } else {
        ui.text_centered(
            action_rect,
            action_label,
            &style(13.0, Weight::SemiBold, theme.text_dim),
        );
    }
    (y + H2_LINE + H2_MARGIN_BOTTOM, resp)
}
