//! Window-anchored overlays: the toast and the update banner.
//!
//! Both float **above** every band of the app chrome (the old CSS gives them
//! `z-index` above the player bar), so they paint into the UI's deferred buffer
//! ([`Ui::deferred`]) — nothing declared later in the frame can cover them.
//!
//! Their geometry is window-anchored, not band-anchored (`ui-spec.md` §6.5/§6.6):
//!
//! ```text
//! toast:         left: 18px;  bottom: 110px; max-width: 420px
//! update banner: right: 18px; bottom: 110px; max-width: 480px
//! ```
//!
//! `bottom: 110px` leaves 26 px of air above the 84 px player bar. At the
//! reference size (1160x740) that is a toast top of `740 - 110 - 37 = 593`.
//!
//! Interaction note: because immediate-mode input is first-come-first-served,
//! the root should call [`overlays`] **early** in the frame (e.g. right after
//! clearing the window) even though it paints last. The banner's buttons then
//! latch the mouse press before the player-bar controls underneath see it. If
//! they are called at the end of the frame the paint order is still right, but a
//! press over a banner button can be stolen by the volume slider below it.
//!
//! Menus and tooltips are **not** here: the framework owns popup/tooltip layering
//! (`crates/rift-desktop/src/ui/popup.rs`). This module only draws the two app-level overlays.

use crate::gfx::paint::Color;
use crate::screens::cards;
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Icon, Id, Rect, Ui};

// ---------------------------------------------------------------------------
// toast geometry (ui-spec.md §6.5)
// ---------------------------------------------------------------------------

/// `left: 18px`.
pub const TOAST_LEFT: f32 = 18.0;
/// `bottom: 110px` (26 px above the 84 px player bar).
pub const TOAST_BOTTOM: f32 = 110.0;
/// `max-width: 420px`.
pub const TOAST_MAX_W: f32 = 420.0;
/// `padding: 10px 14px`.
pub const TOAST_PAD_X: f32 = 14.0;
pub const TOAST_PAD_Y: f32 = 10.0;
/// The 13 px text's hard line pitch (§4.2: `line box 17`).
pub const TOAST_LINE: f32 = 17.0;

/// Toast card size from its wrapped text size. The card is inline-block-like:
/// as wide as the text (plus 28 px of padding), capped at 420 px; the text wraps
/// inside the cap (the old CSS never wraps beyond `max-width`).
pub fn toast_size(text_w: f32, text_h: f32) -> (f32, f32) {
    let inner_w = text_w.clamp(0.0, TOAST_MAX_W - TOAST_PAD_X * 2.0);
    let w = (inner_w + TOAST_PAD_X * 2.0).min(TOAST_MAX_W);
    let h = text_h.max(TOAST_LINE) + TOAST_PAD_Y * 2.0;
    (w, h)
}

/// Top-left corner of the toast card in window coordinates.
pub fn toast_origin(viewport: Rect, size: (f32, f32)) -> Rect {
    Rect::new(
        TOAST_LEFT,
        viewport.bottom() - TOAST_BOTTOM - size.1,
        size.0,
        size.1,
    )
}

// ---------------------------------------------------------------------------
// update banner geometry (ui-spec.md §6.6)
// ---------------------------------------------------------------------------

/// `right: 18px`.
pub const BANNER_RIGHT: f32 = 18.0;
/// `bottom: 110px`.
pub const BANNER_BOTTOM: f32 = 110.0;
/// `max-width: 480px`.
pub const BANNER_MAX_W: f32 = 480.0;
/// `padding: 10px 12px 10px 16px`.
pub const BANNER_PAD_L: f32 = 16.0;
pub const BANNER_PAD_R: f32 = 12.0;
pub const BANNER_PAD_Y: f32 = 10.0;
/// `gap: 12px` between the children.
pub const BANNER_GAP: f32 = 12.0;
/// The text span carries `margin-right: 4px` on top of the flex gap.
pub const BANNER_TEXT_MARGIN: f32 = 4.0;
/// `.btn-primary`/`.btn-secondary` padding `8px 14px`, 13 px/600 line box 17.
pub const BANNER_BTN_PAD_X: f32 = 14.0;
pub const BANNER_BTN_H: f32 = 33.0;
/// The dismiss `ibtn` is 28x28.
pub const BANNER_CLOSE: f32 = 28.0;
/// 13 px text line box.
pub const BANNER_LINE: f32 = 17.0;
/// Banner card radius (`.update-banner` is `radius_lg`-plus: 10).
pub const BANNER_RADIUS: f32 = 10.0;

/// How much horizontal room the version text gets once the buttons are placed.
pub fn banner_text_room(download_w: f32, silence_w: f32) -> f32 {
    (BANNER_MAX_W
        - BANNER_PAD_L
        - BANNER_PAD_R
        - BANNER_TEXT_MARGIN
        - BANNER_GAP * 3.0
        - download_w
        - silence_w
        - BANNER_CLOSE)
        .max(40.0)
}

/// Banner card size from the measured text and button widths.
pub fn banner_size(text_w: f32, download_w: f32, silence_w: f32) -> (f32, f32) {
    let text_w = text_w.max(0.0).min(banner_text_room(download_w, silence_w));
    let w = (BANNER_PAD_L
        + text_w
        + BANNER_TEXT_MARGIN
        + BANNER_GAP
        + download_w
        + BANNER_GAP
        + silence_w
        + BANNER_GAP
        + BANNER_CLOSE
        + BANNER_PAD_R)
        .min(BANNER_MAX_W);
    let h = BANNER_PAD_Y * 2.0 + BANNER_BTN_H.max(BANNER_CLOSE).max(BANNER_LINE);
    (w, h)
}

/// Top-left corner of the banner card in window coordinates.
pub fn banner_origin(viewport: Rect, size: (f32, f32)) -> Rect {
    Rect::new(
        viewport.right() - BANNER_RIGHT - size.0,
        viewport.bottom() - BANNER_BOTTOM - size.1,
        size.0,
        size.1,
    )
}

// ---------------------------------------------------------------------------
// overlays
// ---------------------------------------------------------------------------

/// Draw every window-anchored overlay for this frame. See the module note about
/// calling this early in the frame.
pub fn overlays(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, viewport: Rect) {
    toast(ui, state, viewport);
    update_banner(ui, state, actions, viewport);
}

/// The toast (ui-spec.md §6.5): bottom-left card, one at a time, auto-expired by
/// `App::tick`. The old CSS has a single toast component and only the *error*
/// palette (informational notices are red-tinted too); we keep that one style.
/// It has no actions (no close button, ui-spec §6.5).
pub fn toast(ui: &mut Ui, state: &mut State, viewport: Rect) {
    let Some(toast) = state.toast.as_ref() else {
        return;
    };
    let theme = ui.theme().clone();
    let message = toast.message.clone();
    let is_error = toast.is_error;

    let text_style = cards::style_lh(13.0, Weight::Regular, theme.toast_text, TOAST_LINE);
    let wrapped = ui.measure_text_wrapped(&message, &text_style, TOAST_MAX_W - TOAST_PAD_X * 2.0);
    let (w, h) = toast_size(wrapped.w, wrapped.h);
    let rect = toast_origin(viewport, (w, h));

    ui.deferred(|ui| {
        // Error/notice share the palette; notices lean accent rather than red so
        // a success ("Exported …") does not read as a failure (the old CSS only
        // had the error variant — this is the one intentional divergence, and it
        // only affects notices).
        let (bg, border, text) = if is_error {
            (theme.toast_bg, theme.toast_border, theme.toast_text)
        } else {
            (theme.banner_bg, theme.border, theme.text)
        };
        ui.rounded_rect(rect, bg, theme.radius_lg);
        ui.bordered_rect(rect, Color::TRANSPARENT, theme.radius_lg, 1.0, border);
        let inner = Rect::new(
            rect.x + TOAST_PAD_X,
            rect.y + TOAST_PAD_Y,
            (rect.w - TOAST_PAD_X * 2.0).max(0.0),
            (rect.h - TOAST_PAD_Y * 2.0).max(0.0),
        );
        let style = cards::style_lh(13.0, Weight::Regular, text, TOAST_LINE);
        ui.text_wrapped(inner, &message, &style);
    });
}

/// The launch-time update banner (ui-spec.md §6.6): bottom-right card with the
/// version line, "Download", "Don't ask again" and a dismiss `X`.
pub fn update_banner(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, viewport: Rect) {
    let Some(status) = state.update_banner.as_ref().filter(|u| u.update_available) else {
        return;
    };
    let theme = ui.theme().clone();
    let latest = status.latest.as_deref().unwrap_or_default();
    let url = status.url.as_deref();
    let text = format!("Rift v{latest} is available.");

    let text_style = cards::style_lh(13.0, Weight::Regular, theme.text, BANNER_LINE);
    let button_style = cards::style(13.0, Weight::SemiBold, Color::WHITE);
    let download_w = ui.text_width("Download", &button_style) + BANNER_BTN_PAD_X * 2.0;
    let silence_w = ui.text_width("Don't ask again", &button_style) + BANNER_BTN_PAD_X * 2.0;
    let room = banner_text_room(download_w, silence_w);
    let text_w = ui.text_width(&text, &text_style).min(room);
    let (w, h) = banner_size(text_w, download_w, silence_w);
    let rect = banner_origin(viewport, (w, h));

    ui.deferred(|ui| {
        ui.shadow(rect, BANNER_RADIUS, 0.4);
        ui.rounded_rect(rect, theme.banner_bg, BANNER_RADIUS);
        ui.bordered_rect(rect, Color::TRANSPARENT, BANNER_RADIUS, 1.0, theme.border);

        let mid_y = rect.y + (rect.h - BANNER_BTN_H) * 0.5;
        let mut x = rect.x + BANNER_PAD_L;
        let text_rect = Rect::new(x, rect.y, room, rect.h);
        ui.text(text_rect, &text, &text_style);
        x += room + BANNER_TEXT_MARGIN + BANNER_GAP;

        let download_rect = Rect::new(x, mid_y, download_w, BANNER_BTN_H);
        if cards::primary_button(
            ui,
            download_rect,
            Id::from_str("overlay/update/download"),
            "Download",
            true,
        )
        .clicked
        {
            if let Some(url) = url {
                actions.push(Action::OpenUrl(url.to_string()));
            }
        }
        x += download_w + BANNER_GAP;

        let silence_rect = Rect::new(x, mid_y, silence_w, BANNER_BTN_H);
        if cards::secondary_button(
            ui,
            silence_rect,
            Id::from_str("overlay/update/silence"),
            "Don't ask again",
        )
        .clicked
        {
            actions.push(Action::SetUpdateNotifications(false));
            actions.push(Action::DismissUpdateBanner);
        }
        x += silence_w + BANNER_GAP;

        let close_rect = Rect::new(
            x,
            rect.y + (rect.h - BANNER_CLOSE) * 0.5,
            BANNER_CLOSE,
            BANNER_CLOSE,
        );
        if cards::icon_button(
            ui,
            close_rect,
            Id::from_str("overlay/update/dismiss"),
            Icon::Close,
            16.0,
            theme.text_dim,
        )
        .clicked
        {
            actions.push(Action::DismissUpdateBanner);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_is_padding_plus_text_and_caps_at_420() {
        // "Exported playlist." at 13 px is narrower than the cap.
        let (w, h) = toast_size(120.0, TOAST_LINE);
        assert!((w - (120.0 + 28.0)).abs() < 0.01);
        assert!((h - 37.0).abs() < 0.01, "10 + 17 + 10 = 37");
        // Long text wraps at the cap instead of growing.
        let (w, _) = toast_size(900.0, TOAST_LINE);
        assert_eq!(w, TOAST_MAX_W);
        // Two wrapped lines are 17 px taller.
        let (_, h) = toast_size(400.0, TOAST_LINE * 2.0);
        assert!((h - 54.0).abs() < 0.01);
    }

    #[test]
    fn toast_sits_110_above_the_window_bottom() {
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let rect = toast_origin(viewport, (200.0, 37.0));
        assert_eq!(rect.x, 18.0);
        assert_eq!(rect.bottom(), 740.0 - 110.0);
    }

    #[test]
    fn banner_places_buttons_with_gaps_and_caps_at_480() {
        // Short version text: the card shrinks to its content.
        let (w, h) = banner_size(150.0, 83.0, 113.0);
        assert!(w < BANNER_MAX_W);
        assert!((h - 53.0).abs() < 0.01, "10 + 33 + 10 = 53");

        // Very long text: the card caps; the text is ellipsized into `room`.
        let room = banner_text_room(83.0, 113.0);
        let (w, _) = banner_size(900.0, 83.0, 113.0);
        assert_eq!(w, BANNER_MAX_W);
        assert!(room > 0.0);
    }

    fn frame_with(state: &mut crate::state::State) -> crate::gfx::paint::PaintList {
        let mut ui = Ui::new();
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let input = crate::ui::InputState {
            viewport,
            mouse_pos: (500.0, 700.0),
            mouse_in_window: true,
            time: 1.0,
            ..Default::default()
        };
        ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        overlays(&mut ui, state, &mut actions, viewport);
        ui.end_frame()
    }

    #[test]
    fn toast_paints_a_card_at_the_bottom_left() {
        let mut state = crate::state::State::default();
        state.toast = Some(crate::state::Toast::error("Export failed", 1.0));
        let paint = frame_with(&mut state);
        let found = paint.layers.iter().flat_map(|l| l.prims.iter()).any(|p| {
            matches!(
                p,
                crate::gfx::paint::Prim::Rect { rect, color, radius, .. }
                    if (rect.x - 18.0).abs() < 0.01
                        && rect.bottom() <= 740.0 - 110.0 + 0.01
                        && (*radius - 8.0).abs() < 0.01
                        && *color == crate::theme::Theme::default().toast_bg
            )
        });
        assert!(found, "error toast card not painted");
    }

    #[test]
    fn update_banner_paints_when_a_newer_release_exists() {
        let mut state = crate::state::State {
            update_banner: Some(rift_types::UpdateStatus {
                current: "0.1.0".into(),
                latest: Some("0.2.0".into()),
                update_available: true,
                url: Some("https://example.invalid/release".into()),
            }),
            ..crate::state::State::default()
        };
        let paint = frame_with(&mut state);
        let found = paint.layers.iter().flat_map(|l| l.prims.iter()).any(|p| {
            matches!(
                p,
                crate::gfx::paint::Prim::Rect { rect, .. }
                    if (rect.right() - (1160.0 - 18.0)).abs() < 0.01
            )
        });
        assert!(found, "update banner not painted at the right edge");

        // No banner when the check found no update.
        let mut state = crate::state::State {
            update_banner: Some(rift_types::UpdateStatus {
                current: "0.1.0".into(),
                latest: Some("0.1.0".into()),
                update_available: false,
                url: None,
            }),
            ..crate::state::State::default()
        };
        let paint = frame_with(&mut state);
        assert!(paint.is_empty(), "no banner should be painted");
    }

    #[test]
    fn banner_sits_18_from_the_right_and_110_above_the_bottom() {
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let size = banner_size(150.0, 83.0, 113.0);
        let rect = banner_origin(viewport, size);
        assert_eq!(rect.right(), 1160.0 - 18.0);
        assert_eq!(rect.bottom(), 740.0 - 110.0);
    }
}
