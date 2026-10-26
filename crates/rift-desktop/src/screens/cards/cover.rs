//! Cover tiles: the loaded texture when available, otherwise the generated
//! fallback gradient with the centred music note.

use crate::gfx::paint::TextureId;
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
/// The caller supplies the texture id (`state.request_cover(url)`); pass `None`
/// for the placeholder. Requesting the fetch is the caller's job, which keeps
/// this painter independent of `State` (call sites that need the texture can
/// request it before borrowing the data they draw).
pub fn cover_tile(ui: &mut Ui, rect: Rect, texture: Option<TextureId>, radius: f32) {
    ensure_textures(ui);
    if let Some(tex) = texture {
        ui.image(rect, tex, Color::WHITE, radius);
        return;
    }
    ui.image(rect, COVER_FALLBACK_TEX, Color::WHITE, radius);
    let theme = ui.theme().clone();
    // The old SVG icon was drawn into a 30%-of-the-tile box (the SVG's 24-unit
    // viewBox maps to that box, so its ink sits exactly where the web UI's did).
    let note = rect.w.min(rect.h) * 0.30;
    let box_rect = Rect::new(
        rect.x + (rect.w - note) * 0.5,
        rect.y + (rect.h - note) * 0.5,
        note,
        note,
    );
    ui.image(box_rect, NOTE_TEX, theme.text_dim, 0.0);
}
