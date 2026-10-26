//! Row geometry: the `.settings-row` metrics, the row-height arithmetic shared by
//! [`height`] and [`view`], and the content height used by the scroll view.

use crate::screens::cards;
use crate::state::State;
use crate::text::Weight;
use crate::ui::Ui;

// ---------------------------------------------------------------------------
// row geometry
// ---------------------------------------------------------------------------

/// `.settings-row { max-width: 640px; padding: 16px 18px; gap: 20px }`.
pub const ROW_MAX_W: f32 = 640.0;
pub const ROW_PAD_X: f32 = 18.0;
pub const ROW_PAD_Y: f32 = 16.0;
pub const ROW_GAP: f32 = 20.0;
/// `.settings-row + .settings-row { margin-top: 12px }`.
pub const ROW_SPACING: f32 = 12.0;
pub const ROW_RADIUS: f32 = 10.0;
/// `.settings-label` 15/600 line box 20.
pub const LABEL_H: f32 = 20.0;
/// `.settings-desc { margin-top: 4px }` + 13 px line box 17.
pub const DESC_GAP: f32 = 4.0;
pub(crate) const LINE_13: f32 = 17.0;
/// `.switch` 44x24.
pub const SWITCH_W: f32 = 44.0;
pub const SWITCH_H: f32 = 24.0;
/// `.btn-primary`/`.btn-secondary`: padding 8x14 + 13/600 line 17 = 33.
pub const BTN_H: f32 = 33.0;
const BTN_PAD_X: f32 = 14.0;
/// `h2` line + 14 px bottom margin.
pub(crate) const TITLE_BLOCK_H: f32 = 22.0 + 14.0;
/// The vertical gap inside a row before each extra block (status/actions/input).
pub(crate) const BLOCK_GAP: f32 = 10.0;
/// Status-line badge height: 12/600 line 16 + padding 2x2.
pub(crate) const BADGE_H: f32 = 20.0;

/// The three rows' heights for the current state (used by both `height` and
/// `view`, so the scrollbar can never disagree with the painted rows).
pub fn row_heights(ui: &mut Ui, state: &State, width: f32) -> [f32; 3] {
    let theme = ui.theme().clone();
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);
    let row_w = width.min(ROW_MAX_W);
    let text_w = |control_w: f32| (row_w - ROW_PAD_X * 2.0 - ROW_GAP - control_w).max(0.0);

    let desc_h = |ui: &mut Ui, text: &str, w: f32| {
        ui.measure_text_wrapped(text, &desc_style, w).h.max(LINE_13)
    };

    // 1. Discord Rich Presence.
    let discord = row_h(
        LABEL_H
            + DESC_GAP
            + desc_h(
                ui,
                "Show the track you're listening to on your Discord profile.",
                text_w(SWITCH_W),
            ),
        SWITCH_H,
    );

    // 2. Crossfade (+ the 1..12 s slider row when enabled).
    let mut crossfade_content = LABEL_H
        + DESC_GAP
        + desc_h(
            ui,
            "Overlap the end of each track with the start of the next for a smooth transition.",
            text_w(SWITCH_W),
        );
    if state.crossfade > 0.0 {
        crossfade_content += BLOCK_GAP + 20.0; // slider row (16 px thumb + margin)
    }
    let crossfade = row_h(crossfade_content, SWITCH_H);

    // 3. Updates.
    let updates_content = LABEL_H
        + DESC_GAP
        + desc_h(
            ui,
            "Check GitHub for a newer version of Rift on launch and notify you.",
            text_w(SWITCH_W),
        )
        + BLOCK_GAP
        + BADGE_H
        + BLOCK_GAP
        + BTN_H;
    let updates = row_h(updates_content, SWITCH_H);

    [discord, crossfade, updates]
}

fn row_h(content_h: f32, control_h: f32) -> f32 {
    ROW_PAD_Y * 2.0 + content_h.max(control_h)
}

pub(crate) fn btn_width(ui: &mut Ui, label: &str) -> f32 {
    ui.text_width(
        label,
        &cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE),
    ) + BTN_PAD_X * 2.0
}

/// Content height (no main padding) for [`view`].
pub fn height(ui: &mut Ui, state: &State, width: f32) -> f32 {
    let rows = row_heights(ui, state, width);
    TITLE_BLOCK_H + rows.iter().sum::<f32>() + ROW_SPACING * (rows.len() as f32 - 1.0)
}
