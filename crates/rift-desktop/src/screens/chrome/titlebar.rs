//! The 46 px window titlebar: logo + wordmark, centred search box, window
//! controls, and the draggable region.

use crate::screens::cards;
use crate::state::{Action, State};
use crate::text::Weight;
use crate::theme::Theme;
use crate::ui::{Color, CursorShape, Icon, Id, Key, Rect, Sense, Ui};

/// Titlebar search box (ui-spec.md §6.1).
const SEARCH_BOX_W: f32 = 480.0;
const SEARCH_BOX_H: f32 = 31.0;
const SEARCH_BTN_W: f32 = 79.0;

/// Window control buttons are 46×46 flush right.
const WIN_BTN_W: f32 = 46.0;
const WIN_GLYPH: f32 = 14.0;

// ---------------------------------------------------------------------------
// titlebar
// ---------------------------------------------------------------------------

/// The 46 px window titlebar: logo + wordmark, centred search box, window
/// controls, and the drag region. Window commands go through `actions`
/// (`Action::WindowDrag`, `WindowMinimize`, `WindowToggleMaximize`,
/// `Action::WindowClose`).
pub fn titlebar(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, rect: Rect) {
    let theme: Theme = ui.theme().clone();
    ui.fill(rect, theme.titlebar_bg);
    ui.hline(rect.x, rect.bottom() - 1.0, rect.w, 1.0, theme.border);

    // ---- logo mark + gradient wordmark (x=14, 22×22, gap 9) ----
    cards::ensure_textures(ui);
    let mark = Rect::new(rect.x + 14.0, rect.y + (rect.h - 22.0) * 0.5, 22.0, 22.0);
    let logo = Rect::new(
        mark.x - cards::LOGO_TILE_INSET,
        mark.y - cards::LOGO_TILE_INSET,
        cards::LOGO_DRAW_SIZE,
        cards::LOGO_DRAW_SIZE,
    );
    ui.image(logo, cards::LOGO_TEX, Color::WHITE, 0.0);
    // Wordmark gradient: `linear-gradient(110deg, #7df9ff, #c084fc, #ff8df5)`.
    // Painted per glyph at the colour of the glyph's centre position along the
    // word, which reproduces the reference's R/I/F/T cores.
    let word_style =
        cards::style_lh(17.0, Weight::Bold, theme.accent, 22.0).with_letter_spacing(2.0);
    let word_y = rect.y + (rect.h - 22.0) * 0.5;
    let stops = [
        Color::hex(0x7df9ff),
        Color::hex(0xc084fc),
        Color::hex(0xff8df5),
    ];
    let glyphs: Vec<(char, f32)> = "RIFT"
        .chars()
        .map(|ch| (ch, ui.text_width(&ch.to_string(), &word_style) + 2.0))
        .collect();
    let total_w: f32 = glyphs.iter().map(|(_, w)| *w).sum::<f32>() - 2.0;
    let mut x = mark.right() + 9.0;
    let mut advance = 0.0f32;
    for (ch, w) in &glyphs {
        let s = ch.to_string();
        let t = if total_w > 0.0 {
            ((advance + w * 0.5) / total_w).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let color = if t < 0.5 {
            cards::mix(stops[0], stops[1], t / 0.5)
        } else {
            cards::mix(stops[1], stops[2], (t - 0.5) / 0.5)
        };
        ui.text(
            Rect::new(x, word_y, w + 2.0, 22.0),
            &s,
            &word_style.with_color(color),
        );
        x += w;
        advance += w;
    }

    // ---- search box (absolutely centred, 480×31 at y=7) ----
    let box_w = SEARCH_BOX_W.min(rect.w * 0.42);
    let box_rect = Rect::new(
        rect.x + (rect.w - box_w) * 0.5,
        rect.y + ((rect.h - SEARCH_BOX_H) * 0.5).floor(),
        box_w,
        SEARCH_BOX_H,
    );
    ui.fill(box_rect, theme.bg_raised);

    // Search button: 18 px horizontal padding, fills the inner height; its right
    // corners follow the box radius (`overflow: hidden` in the old CSS).
    let btn_rect = Rect::new(
        box_rect.right() - 1.0 - SEARCH_BTN_W,
        box_rect.y + 1.0,
        SEARCH_BTN_W,
        box_rect.h - 2.0,
    );
    let btn_id = Id::from_str("titlebar/search-btn");
    let btn = ui.interact(btn_rect, btn_id, Sense::CLICK);
    let btn_fill = cards::mix(theme.accent, theme.accent_dim, btn.hover_t);
    ui.rounded_rect(btn_rect, btn_fill, 5.0);
    // Square off the left side where it meets the input.
    ui.fill(Rect::new(btn_rect.x, btn_rect.y, 6.0, btn_rect.h), btn_fill);
    ui.text_centered(
        btn_rect,
        "Search",
        &cards::style_lh(13.0, Weight::SemiBold, Color::WHITE, 17.0),
    );
    if btn.focus_visible {
        ui.focus_ring(btn_rect, 6.0);
    }

    // Text field: everything left of the button. The framework's text input
    // gives the old `<input>`'s editing behaviour (click-to-caret, selection,
    // clipboard, horizontal scrolling, Enter submit).
    let input_rect = Rect::new(
        box_rect.x + 1.0,
        box_rect.y + 1.0,
        (btn_rect.x - box_rect.x - 1.0).max(0.0),
        box_rect.h - 2.0,
    );
    let input_id = Id::from_str("titlebar/search-input");
    let mut query = state.search.query.clone();
    // Escape releases the field on this frame (the old UI cleared the box and
    // stepped off the search view). Handling it before the widget lets the UI's
    // `wants_keyboard()` be false at the end of this frame, so app-level
    // shortcuts stop being suppressed immediately.
    let escape = ui.is_focused(input_id) && ui.input().key_clicked(Key::Escape);
    if escape {
        ui.clear_focus();
        query.clear();
    }
    let input_style = crate::ui::TextInputStyle::search(&theme);
    let input = ui.text_input(
        input_rect,
        input_id,
        &mut query,
        "Search YouTube Music...",
        &input_style,
    );
    let focused = ui.is_focused(input_id);
    // Border last so it sits over the button's rounded corners.
    let border = if focused { theme.focus } else { theme.border };
    ui.stroke(box_rect, 6.0, 1.0, border);
    if focused {
        ui.stroke(box_rect.expand(1.0), 6.5, 1.0, theme.focus);
    }
    if input.changed {
        actions.push(Action::SearchQueryChanged(query.clone()));
        // The old box live-searched on a 300 ms debounce; the native port issues
        // the command on the edit frame (the controller's search generation keeps
        // stale results from landing, and the acceptance harness expects the
        // command on the typing frame).
        let trimmed = query.trim();
        if !trimmed.is_empty() {
            actions.push(Action::Search(trimmed.to_string()));
        }
    }
    if (btn.clicked || input.submitted) && !query.trim().is_empty() {
        actions.push(Action::Search(query.trim().to_string()));
    }
    if escape || input.cancelled {
        // `SearchQueryChanged("")` makes app.rs cancel the in-flight search and
        // drop the previous results, like the old on_clear.
        actions.push(Action::SearchQueryChanged(String::new()));
    }

    // ---- window controls (46×46 each, flush right) ----
    let controls_x = rect.right() - WIN_BTN_W * 3.0;
    let buttons = [
        (
            Id::from_str("titlebar/min"),
            Icon::Minimize,
            Action::WindowMinimize,
        ),
        (
            Id::from_str("titlebar/max"),
            if state.window_maximized {
                Icon::FullscreenExit
            } else {
                Icon::Maximize
            },
            Action::WindowToggleMaximize,
        ),
        (
            Id::from_str("titlebar/close"),
            Icon::Close,
            Action::WindowClose,
        ),
    ];
    for (i, (id, icon, action)) in buttons.into_iter().enumerate() {
        let btn = Rect::new(controls_x + WIN_BTN_W * i as f32, rect.y, WIN_BTN_W, rect.h);
        let resp = ui.interact(btn, id, Sense::CLICK);
        let is_close = i == 2;
        if resp.hover_t > 0.0 {
            let hover = if is_close {
                Color::hex(0xe81123)
            } else {
                theme.bg_hover
            };
            ui.fill(btn, hover.with_alpha((resp.hover_t * 255.0).round() as u8));
        }
        let color = if resp.hovered {
            if is_close {
                Color::WHITE
            } else {
                theme.text
            }
        } else {
            theme.text_dim
        };
        ui.icon(btn, icon, color, WIN_GLYPH);
        if resp.focus_visible {
            ui.focus_ring(btn, 0.0);
        }
        if resp.clicked {
            actions.push(action);
        }
    }

    // ---- drag region (declared last so buttons win the press) ----
    // Skip the drag hit area over the interactive children: otherwise its
    // focus-capable DRAG sense steals focus from the search field on the press
    // frame (the field looked focused but never received text). The region also
    // never takes keyboard focus, so Tab order is unaffected.
    let pointer = ui.input().mouse_pos;
    let controls_rect = Rect::new(controls_x, rect.y, WIN_BTN_W * 3.0, rect.h);
    let over_child = box_rect.expand(2.0).contains(pointer.0, pointer.1)
        || controls_rect.contains(pointer.0, pointer.1);
    if !over_child {
        let drag = ui.interact(
            rect,
            Id::from_str("titlebar/drag"),
            Sense::new(true, true, false),
        );
        if drag.drag_started {
            actions.push(Action::WindowDrag);
        }
        if drag.double_clicked {
            actions.push(Action::WindowToggleMaximize);
        }
        if drag.hovered {
            ui.set_cursor(CursorShape::Default);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_geometry_matches_the_reference() {
        // Search box is 480 wide and horizontally centred; controls are 3×46.
        let w = 1160.0;
        let search_w = SEARCH_BOX_W.min(w * 0.42);
        assert_eq!(search_w, 480.0);
        assert_eq!((w - search_w) * 0.5, 340.0);
        assert_eq!(w - WIN_BTN_W * 3.0, 1022.0);
        assert_eq!((46.0 - SEARCH_BOX_H) * 0.5, 7.5);
    }
}
