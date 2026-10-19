//! The range slider used by seek / volume / crossfade.

use crate::theme::Theme;
use crate::ui::{Id, Rect, Response, Sense, Ui};

// ---------------------------------------------------------------------------
// sliders
// ---------------------------------------------------------------------------

/// What a [`slider`] interaction produced this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliderOutcome {
    pub response: Response,
    /// The fraction (0..=1) under the pointer while the slider is being dragged.
    pub dragged_fraction: Option<f32>,
    /// The thumb was released after a drag within the slider (commit).
    pub committed: bool,
    /// On the commit frame: the fraction under the pointer (0..=1), so callers
    /// can fire exactly one seek/save for the release position.
    pub committed_fraction: Option<f32>,
}

/// A range slider (seek / volume / crossfade): 4 px track with a 16 px round
/// thumb, played portion in the accent color. This is a local widget (the
/// framework's slider helper is not part of the frozen base API yet); it follows
/// the reference pixels from `ui-spec.md` §6.3.
pub fn slider(ui: &mut Ui, rect: Rect, id: Id, fraction: f32, theme: &Theme) -> SliderOutcome {
    const THUMB_R: f32 = 8.0;
    let resp = ui.interact(rect, id, Sense::DRAG);
    if resp.hovered {
        ui.set_cursor(crate::ui::CursorShape::Pointer);
    }
    let mut fraction = fraction.clamp(0.0, 1.0);
    let mut dragged_fraction = None;
    if resp.active {
        let pointer_x = ui.input().mouse_pos.0;
        let travel = (rect.w - THUMB_R * 2.0).max(1.0);
        let f = ((pointer_x - (rect.x + THUMB_R)) / travel).clamp(0.0, 1.0);
        fraction = f;
        dragged_fraction = Some(f);
    }

    // Track: Chromium's range track is ~3.5 px tall and inset 2 px from the
    // input's box (`margin: 2px` in the UA stylesheet) — the reference's
    // blended edge rows (669/672 on the seek bar) fall out of exactly this.
    const TRACK_INSET: f32 = 2.0;
    let track_rect = Rect::new(
        rect.x + TRACK_INSET,
        rect.y,
        (rect.w - TRACK_INSET * 2.0).max(2.0),
        rect.h,
    );
    let track_y = track_rect.y + (track_rect.h - 3.5) * 0.5;
    let track = Rect::new(track_rect.x, track_y, track_rect.w, 3.5);
    ui.rounded_rect(track, theme.seek_track, 1.75);
    let filled_w = THUMB_R + (track_rect.w - THUMB_R * 2.0) * fraction;
    if filled_w > 0.5 {
        ui.rounded_rect(
            Rect::new(track_rect.x, track_y, filled_w, 3.5),
            theme.seek_active,
            1.75,
        );
    }
    // Thumb: 16 px circle travelling inside the inset track.
    let cx = track_rect.x + THUMB_R + (track_rect.w - THUMB_R * 2.0) * fraction;
    let cy = rect.y + rect.h * 0.5;
    ui.circle(crate::ui::Vec2::new(cx, cy), THUMB_R, theme.seek_active);

    if resp.focus_visible {
        ui.focus_ring(rect, 2.0);
    }
    let committed = resp.clicked || (resp.hovered && ui.input().mouse_left_released);
    let committed_fraction = if committed {
        let pointer_x = ui.input().mouse_pos.0;
        let travel = (track_rect.w - THUMB_R * 2.0).max(1.0);
        Some(((pointer_x - (track_rect.x + THUMB_R)) / travel).clamp(0.0, 1.0))
    } else {
        None
    };
    SliderOutcome {
        response: resp,
        dragged_fraction,
        committed,
        committed_fraction,
    }
}
