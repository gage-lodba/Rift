//! Cover tiles: the loaded texture when available, otherwise the generated
//! fallback gradient with the centred music note.

use crate::gfx::paint::TextureId;
use crate::state::State;
use crate::ui::{Color, Rect, Ui};

use super::{ensure_textures, COVER_FALLBACK_TEX, NOTE_TEX};

// ---------------------------------------------------------------------------
// cover tiles
// ---------------------------------------------------------------------------

/// A cover tile: the loaded texture when available, otherwise the fallback
/// gradient with a centred music note at 30% of the tile. `radius` follows the
/// old CSS (`6` for card covers, `5` for the player cover, `4` for row covers,
/// `999` for round artist avatars).
///
/// The caller supplies the texture id (`state.cover_texture(url)`); pass `None`
/// for the placeholder. `state` is only needed to request a fetch.
pub fn cover_tile(
    ui: &mut Ui,
    state: &mut State,
    rect: Rect,
    texture: Option<TextureId>,
    radius: f32,
) {
    ensure_textures(ui);
    match texture {
        Some(tex) => ui.image(rect, tex, Color::WHITE, radius),
        None => {
            ui.image(rect, COVER_FALLBACK_TEX, Color::WHITE, radius);
            let theme = ui.theme().clone();
            // The old SVG icon was drawn into a 30%-of-the-tile box (the SVG's
            // 24-unit viewBox maps to that box, so its ink sits exactly where the
            // web UI's did).
            let note = rect.w.min(rect.h) * 0.30;
            let box_rect = Rect::new(
                rect.x + (rect.w - note) * 0.5,
                rect.y + (rect.h - note) * 0.5,
                note,
                note,
            );
            ui.image(box_rect, NOTE_TEX, theme.text_dim, 0.0);
        }
    }
    // The row/card may carry a cover URL the controller has not fetched yet.
    let _ = state;
}

/// A cover tile from a track's URL, requesting the fetch if needed.
pub fn track_cover(ui: &mut Ui, state: &mut State, rect: Rect, url: &str, radius: f32) {
    let texture = state.request_cover(url);
    cover_tile(ui, state, rect, texture, radius);
}
