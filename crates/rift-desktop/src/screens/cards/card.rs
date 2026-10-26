//! The library / album / artist card and its responsive grid metrics.

use crate::state::State;
use crate::text::Weight;
use crate::ui::{Id, Rect, Response, Sense, Ui};

use super::cover::cover_tile;
use super::{
    style_lh, CARD_NAME_LINE, CARD_NAME_MARGIN, CARD_PADDING, CARD_RADIUS, CARD_SUB_LINE,
    CARD_SUB_MARGIN, COVER_RADIUS,
};

// ---------------------------------------------------------------------------
// card grid + library card
// ---------------------------------------------------------------------------

/// Column count and width for `repeat(auto-fill, minmax(160px, 1fr)); gap: 18px`.
pub fn grid_metrics(content_w: f32, min_width: f32, gap: f32) -> (usize, f32) {
    let mut cols = ((content_w + gap) / (min_width + gap)).floor() as usize;
    cols = cols.max(1);
    let w = (content_w - gap * (cols.saturating_sub(1)) as f32) / cols as f32;
    (cols, w.max(0.0))
}

/// Height of a library card whose cover is `cover` px square.
pub fn card_height(cover: f32) -> f32 {
    CARD_PADDING * 2.0 + cover + CARD_NAME_MARGIN + CARD_NAME_LINE + CARD_SUB_MARGIN + CARD_SUB_LINE
}

/// A library / album / artist card. `cover_url` empty → fallback tile. The card
/// paints its hover background and returns the interaction; `menu` is the
/// optional kebab response (caller decides whether the card has one).
#[allow(clippy::too_many_arguments)] // immediate-mode widget: explicit style/layout args are the API
pub fn card(
    ui: &mut Ui,
    state: &mut State,
    rect: Rect,
    id: Id,
    cover_url: &str,
    name: &str,
    sub: &str,
    cover_round: bool,
) -> Response {
    let theme = ui.theme().clone();
    let resp = ui.interact(rect, id, Sense::CLICK);
    if resp.hover_t > 0.0 {
        ui.rounded_rect(
            rect,
            theme
                .bg_raised
                .with_alpha((resp.hover_t * 255.0).round() as u8),
            CARD_RADIUS,
        );
    }
    let inner = rect.inset(CARD_PADDING);
    let cover_size = inner.w;
    let cover_rect = Rect::new(inner.x, inner.y, cover_size, cover_size);
    let texture = state.request_cover(cover_url);
    let radius = if cover_round {
        cover_size * 0.5
    } else {
        COVER_RADIUS
    };
    cover_tile(ui, cover_rect, texture, radius);

    let name_y = cover_rect.bottom() + CARD_NAME_MARGIN;
    let name_style = style_lh(13.0, Weight::SemiBold, theme.text, CARD_NAME_LINE);
    let name_rect = Rect::new(inner.x, name_y, inner.w, CARD_NAME_LINE);
    if cover_round {
        ui.text_centered(name_rect, name, &name_style);
    } else {
        ui.text(name_rect, name, &name_style);
    }
    let sub_y = name_y + CARD_NAME_LINE + CARD_SUB_MARGIN;
    let sub_rect = Rect::new(inner.x, sub_y, inner.w, CARD_SUB_LINE);
    let sub_style = style_lh(12.0, Weight::Regular, theme.text_dim, CARD_SUB_LINE);
    if cover_round {
        ui.text_centered(sub_rect, sub, &sub_style);
    } else {
        ui.text(sub_rect, sub, &sub_style);
    }
    if resp.focus_visible {
        // Cards use `outline-offset: -2px` (ui-spec §2.4/§6.9): the ring must not
        // bleed into the neighbouring card in the grid.
        ui.focus_ring_inset(rect, CARD_RADIUS);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_metrics_matches_the_reference_library() {
        // Content width 766 (window 1160, sidebar 335): 4 columns of 178 px.
        let (cols, w) = grid_metrics(766.0, 160.0, 18.0);
        assert_eq!(cols, 4);
        assert!((w - 178.0).abs() < 0.01);
        // Default sidebar 216 at 1160: 5 columns.
        let (cols, w) = grid_metrics(886.0, 160.0, 18.0);
        assert_eq!(cols, 5);
        assert!((w - 162.8).abs() < 0.01);
        // Narrow window: at least one column.
        let (cols, _) = grid_metrics(120.0, 160.0, 18.0);
        assert_eq!(cols, 1);
    }

    #[test]
    fn card_height_matches_the_reference() {
        // Reference cover 158 -> card 222 px tall.
        assert_eq!(card_height(158.0), 222.0);
    }
}
