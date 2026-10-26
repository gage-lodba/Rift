//! The Updates row's status line (`Check now` / "Download update").

use crate::screens::cards;
use crate::state::State;
use crate::text::Weight;
use crate::ui::{BadgeKind, Color, Rect, Ui};

use super::layout::{BADGE_H, LINE_13};

/// Badge width: 12/600 label + 8 px horizontal padding each side.
fn badge_w(ui: &mut Ui, label: &str) -> f32 {
    ui.text_width(label, &cards::style(12.0, Weight::SemiBold, Color::WHITE)) + 16.0
}

/// Draw the update-check status line. Returns the y below it.
pub(crate) fn draw_update_status(
    ui: &mut Ui,
    state: &State,
    x: f32,
    y: f32,
    width: f32,
    meta_style: &crate::text::TextStyle,
) -> f32 {
    match &state.update_status {
        None => {
            let text = if state.preview {
                "Not checked yet (no backend in preview mode)"
            } else {
                "Checking…"
            };
            ui.text(Rect::new(x, y, width, LINE_13), text, meta_style);
            y + BADGE_H
        }
        Some(u) if u.update_available => {
            let label = "Update available";
            let w = badge_w(ui, label);
            ui.badge(Rect::new(x, y, w, BADGE_H), label, BadgeKind::Danger);
            let meta = format!("v{} → v{}", u.current, u.latest.clone().unwrap_or_default());
            ui.text(
                Rect::new(x + w + 8.0, y + (BADGE_H - LINE_13) * 0.5, 260.0, LINE_13),
                &meta,
                meta_style,
            );
            y + BADGE_H
        }
        Some(u) if u.latest.is_some() => {
            let label = "✓ Up to date";
            let w = badge_w(ui, label);
            ui.badge(Rect::new(x, y, w, BADGE_H), label, BadgeKind::Success);
            ui.text(
                Rect::new(x + w + 8.0, y + (BADGE_H - LINE_13) * 0.5, 120.0, LINE_13),
                &format!("v{}", u.current),
                meta_style,
            );
            y + BADGE_H
        }
        Some(u) => {
            ui.text(
                Rect::new(x, y, width, LINE_13),
                &format!("Rift v{} — could not reach GitHub", u.current),
                meta_style,
            );
            y + BADGE_H
        }
    }
}
