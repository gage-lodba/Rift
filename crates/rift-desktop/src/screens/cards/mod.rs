//! Reusable primitives shared by every screen: section headings, buttons, cover
//! tiles, the library card, track rows, sliders and the text field. This module
//! directory replaces the former 1,725-line `cards.rs`; `mod.rs` keeps the shared
//! layout constants/style helpers and re-exports every public item, so
//! `screens::cards::*` call sites are unchanged.
//!
//! Every number here comes from `docs/vulkan-migration/ui-spec.md` (measured off
//! the archived old-UI capture `docs/vulkan-migration/reference-old-ui.png` and
//! the removed `ui/styles.css`). The immediate-mode API is `crate::ui`: paint +
//! hit-test in one call, state in the `Ui` context keyed by [`Id`].
//!
//! Two paint limitations shape the widget set:
//!  * the paint contract has no gradient primitive, so the fallback cover tile
//!    and the Rift logo mark are generated once into small RGBA textures and
//!    drawn as images (`textures.rs`);
//!  * soft shadows are approximated with stacked translucent rounded rects
//!    (`Ui::shadow`).

use crate::text::{TextStyle, Weight};
use crate::ui::layout::{Align, Size, VAlign};
use crate::ui::{Color, Prim, Rect};

mod buttons;
mod card;
mod cover;
mod section;
mod slider;
mod text_field;
mod textures;
mod track_row;

pub use buttons::{
    accent_button, button, icon_button, link_button, mix, primary_button, secondary_button,
    secondary_pill, ButtonColors, ButtonStyle,
};
pub use card::{card, card_height, grid_metrics};
pub use cover::{cover_tile, track_cover};
pub use section::{section_header, section_header_action};
pub use slider::{slider, SliderOutcome};
pub use text_field::text_field;
pub use textures::{
    ensure_textures, fallback_cover_rgba, music_note_rgba, COVER_FALLBACK_TEX, LOGO_DRAW_SIZE,
    LOGO_TEX, LOGO_TILE_INSET, NOTE_TEX,
};
pub use track_row::{spinner, track_row, TrackRowOpts, TrackRowResponse};

// ---------------------------------------------------------------------------
// layout constants (ui-spec.md §4)
// ---------------------------------------------------------------------------

/// `h2` line box: 17 px font, Segoe UI `normal` line-height → 22 px pitch.
pub const H2_LINE: f32 = 22.0;
pub const H2_MARGIN_TOP: f32 = 18.0;
pub const H2_MARGIN_BOTTOM: f32 = 14.0;

/// Card: `padding: 10px`, cover radius 6, name margin-top 9, name line 17,
/// sub margin-top 2, sub line 16. Total height = 2*10 + cover + 44.
pub const CARD_PADDING: f32 = 10.0;
pub const CARD_NAME_MARGIN: f32 = 9.0;
pub const CARD_NAME_LINE: f32 = 17.0;
pub const CARD_SUB_MARGIN: f32 = 2.0;
pub const CARD_SUB_LINE: f32 = 16.0;
pub const CARD_RADIUS: f32 = 8.0;
pub const COVER_RADIUS: f32 = 6.0;

/// Track row: `padding: 7px 10px`, cover 40, radius 6, row height 54.
pub const TROW_PAD_X: f32 = 10.0;
pub const TROW_PAD_Y: f32 = 7.0;
pub const TROW_GAP: f32 = 12.0;
pub const TROW_COVER: f32 = 40.0;
pub const TROW_TITLE_LINE: f32 = 17.0;
pub const TROW_ARTIST_LINE: f32 = 16.0;
pub const TROW_DL_W: f32 = 28.0;
pub const TROW_DUR_W: f32 = 40.0;
pub const TROW_ACTIONS_W: f32 = 56.0;
pub const TROW_HEIGHT: f32 = 54.0;

/// Segoe-UI-parity line boxes (the old web UI fell back to Segoe UI, whose
/// `normal` line-height is `1.33 * size`; Inter's is 1.21 — pin the box so the
/// vertical rhythm matches the reference screenshot).
pub fn line_box(size: f32) -> f32 {
    (size * 1.33).round()
}

/// A text style with an explicit line box (see [`line_box`]).
pub fn style(size: f32, weight: Weight, color: Color) -> TextStyle {
    TextStyle::new(size, weight, color).with_line_height(line_box(size))
}

/// A text style with an explicit line height in px.
pub fn style_lh(size: f32, weight: Weight, color: Color, line_height: f32) -> TextStyle {
    TextStyle::new(size, weight, color).with_line_height(line_height)
}

/// A disabled/inert spacer so hidden controls keep their horizontal slot.
pub fn none_rect() -> Rect {
    Rect::ZERO
}

/// Debug helper: the exact paint primitive for a rectangle (used by tests).
pub fn rect_prim(rect: Rect, color: Color) -> Prim {
    Prim::rect(rect, color)
}

/// Helper for callers that want a fixed-size box aligned in a row.
pub fn boxed(rect: Rect, size: Size, v: VAlign) -> Rect {
    rect.align(size, Align::Left, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_boxes_match_segoe_ui_metrics() {
        assert_eq!(line_box(17.0), 23.0); // 17 * 1.33 = 22.6 -> 23 (h2 uses 22)
        assert_eq!(line_box(13.0), 17.0);
        assert_eq!(line_box(12.0), 16.0);
        assert_eq!(line_box(11.0), 15.0);
        assert_eq!(line_box(10.0), 13.0);
        assert_eq!(line_box(15.0), 20.0);
    }
}
