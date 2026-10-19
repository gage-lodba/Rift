//! Inline sidebar editors (new playlist / import playlist) and the animated
//! now-playing equalizer.

use crate::screens::cards;
use crate::state::{Action, State};
use crate::theme::Theme;
use crate::ui::{Color, Icon, Id, Rect, Response, Sense, Ui};

use super::row::side_row_id;

/// Style of the inline sidebar editors (`.side-new-input`): raised fill, 1px
/// accent border, 6px radius, 13px text, 7x12 padding. The border/background are
/// painted by the caller on the full row so the in-field buttons can sit on top.
pub(super) fn inline_input_style(theme: &Theme) -> crate::ui::TextInputStyle {
    let mut style = crate::ui::TextInputStyle::default_theme(theme);
    style.bg = Color::TRANSPARENT;
    style.border = Color::TRANSPARENT;
    style.border_focused = Color::TRANSPARENT;
    style
}

/// Paint an `.ibtn` icon button from an already-interacted response (the nested
/// buttons are interacted before the text field, see the inline editors).
pub(super) fn paint_inline_icon(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    color: Color,
    resp: &Response,
) {
    if resp.hover_t > 0.0 {
        ui.rounded_rect(
            rect,
            theme
                .bg_hover
                .with_alpha((resp.hover_t * 255.0).round() as u8),
            theme.radius_sm,
        );
    }
    ui.icon(
        rect,
        icon,
        cards::mix(color, theme.text, resp.hover_t),
        16.0,
    );
    if resp.focus_visible {
        ui.focus_ring(rect, theme.radius_sm);
    }
}

/// Focus bookkeeping for the inline sidebar editors.
///
/// The editor closes when focus moves *outside* it. Its own controls (text
/// field, confirm, cancel) keep it open: clicking a button focuses it on the
/// press frame, so treating that as "moved on" would close the editor before
/// the release frame delivers the button's click. The text field is focused
/// when the editor first appears (nothing focused, or focus still on the row
/// that opened it).
pub(super) fn inline_editor_keep_open(
    ui: &mut Ui,
    input_id: Id,
    opener: Id,
    confirm_id: Id,
    cancel_id: Id,
) -> bool {
    if ui.is_focused(input_id) {
        return true;
    }
    match ui.focused_id() {
        None => {
            ui.request_focus(input_id);
            true
        }
        Some(focused) if focused == opener => {
            ui.request_focus(input_id);
            true
        }
        Some(focused) if focused == confirm_id || focused == cancel_id => true,
        Some(_) => false,
    }
}

/// In-field button geometry for the sidebar inline editors (old CSS:
/// `.side-new-btn { width: 22px; height: 22px }`, `.confirm { right: 34px }`,
/// `.cancel { right: 6px }`). Returns `(confirm, cancel)`: 22 px each with a
/// 6 px gap, so their hit rects never overlap.
pub(super) fn inline_editor_buttons(row: Rect) -> (Rect, Rect) {
    let y = row.y + (row.h - 22.0) * 0.5;
    let cancel = Rect::new(row.right() - 6.0 - 22.0, y, 22.0, 22.0);
    let confirm = Rect::new(row.right() - 34.0 - 22.0, y, 22.0, 22.0);
    (confirm, cancel)
}

/// The inline "new playlist" draft row (`.side-new-row`): text field with
/// confirm/cancel buttons inside it. Enter creates (non-empty) and closes;
/// Escape / clicking elsewhere cancels; the draft lives in `State::new_playlist`
/// via the playlist-editor actions.
pub(super) fn new_playlist_input(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    rect: Rect,
) {
    let theme = ui.theme().clone();
    let row = Rect::new(rect.x, rect.y, rect.w, 34.0);
    let input_id = Id::from_str("sidebar/new-playlist-input");
    let confirm_id = Id::from_str("sidebar/confirm");
    let cancel_id = Id::from_str("sidebar/cancel");
    let opener = side_row_id("New playlist", false);
    if !inline_editor_keep_open(ui, input_id, opener, confirm_id, cancel_id) {
        actions.push(Action::ClosePlaylistEditor);
        return;
    }
    // In-field buttons: 22 px wide, old CSS `.side-new-btn.confirm { right: 34px }`
    // and `.side-new-btn.cancel { right: 6px }`. The confirm sits 22 px further
    // left so the two hit rects never overlap (they used to be at right-34 and
    // right-28, i.e. on top of each other).
    let (confirm, cancel) = inline_editor_buttons(row);
    // Nested buttons first: their press must latch before the text field's.
    let ok = ui.interact(confirm, confirm_id, Sense::CLICK);
    let no = ui.interact(cancel, cancel_id, Sense::CLICK);

    let draft = state.new_playlist.clone().unwrap_or_default();
    let mut value = draft.clone();
    let style = inline_input_style(&theme);
    ui.bordered_rect(row, theme.bg_raised, theme.radius_md, 1.0, theme.accent);
    // Leave room for both in-field buttons (old `.side-new-input` had
    // `padding-right: 62px`); the border above still spans the full row.
    let field = Rect::new(row.x, row.y, (row.w - 50.0).max(1.0), row.h);
    let resp = ui.text_input(field, input_id, &mut value, "Playlist name", &style);
    paint_inline_icon(ui, &theme, confirm, Icon::Check, theme.accent, &ok);
    paint_inline_icon(ui, &theme, cancel, Icon::Close, theme.text_dim, &no);

    if value != draft {
        actions.push(Action::PlaylistDraftChanged {
            id: None,
            name: value.clone(),
        });
    }
    if resp.submitted || ok.clicked {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            actions.push(Action::Toast("Playlist name can't be empty.".into()));
        } else {
            actions.push(Action::CreatePlaylist(trimmed));
            actions.push(Action::ClosePlaylistEditor);
        }
    } else if resp.cancelled || no.clicked {
        actions.push(Action::ClosePlaylistEditor);
    }
}

/// The inline "import playlist from a link/ID" paste box (`.side-new-row`).
/// Enter submits `Action::ImportYtPlaylistUrl` (the controller imports and
/// navigates/notices); Escape or clicking elsewhere closes; the draft lives in
/// `State::import_playlist` (UI-local, like `new_playlist`).
pub(super) fn import_playlist_input(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    rect: Rect,
) {
    let theme = ui.theme().clone();
    let row = Rect::new(rect.x, rect.y, rect.w, 34.0);
    let input_id = Id::from_str("sidebar/import-input");
    let confirm_id = Id::from_str("sidebar/import-confirm");
    let cancel_id = Id::from_str("sidebar/import-cancel");
    let opener = side_row_id("Import playlist", false);
    if !inline_editor_keep_open(ui, input_id, opener, confirm_id, cancel_id) {
        state.import_playlist = None;
        return;
    }
    // Same in-field layout as the new-playlist editor: confirm at right 34,
    // cancel at right 6, each 22 px wide (no overlap).
    let (confirm, cancel) = inline_editor_buttons(row);
    let ok = ui.interact(confirm, confirm_id, Sense::CLICK);
    let no = ui.interact(cancel, cancel_id, Sense::CLICK);

    let draft = state.import_playlist.clone().unwrap_or_default();
    let mut value = draft.clone();
    let style = inline_input_style(&theme);
    ui.bordered_rect(row, theme.bg_raised, theme.radius_md, 1.0, theme.accent);
    // Leave room for both in-field buttons (old `padding-right: 62px`).
    let field = Rect::new(row.x, row.y, (row.w - 50.0).max(1.0), row.h);
    let resp = ui.text_input(
        field,
        input_id,
        &mut value,
        "YouTube Music link or ID",
        &style,
    );
    paint_inline_icon(ui, &theme, confirm, Icon::Check, theme.accent, &ok);
    paint_inline_icon(ui, &theme, cancel, Icon::Close, theme.text_dim, &no);

    if state.import_playlist.as_deref() != Some(value.as_str()) {
        state.import_playlist = Some(value.clone());
    }
    if resp.submitted || ok.clicked {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            actions.push(Action::Toast(
                "Paste a YouTube Music playlist link or ID.".into(),
            ));
        } else {
            actions.push(Action::ImportYtPlaylistUrl(trimmed));
            state.import_playlist = None;
        }
    } else if resp.cancelled || no.clicked {
        state.import_playlist = None;
    }
}

/// The animated now-playing equalizer (3 accent bars, 0.9 s bounce; paused bars
/// are a flat 5 px).
pub(super) fn eq_bars(ui: &mut Ui, rect: Rect, color: Color, playing: bool) {
    let total_w = 3.0 * 3.0 + 2.0 * 2.0; // 3 bars of 3 px, 2 px gaps = 13
    let x0 = rect.x + (rect.w - total_w) * 0.5;
    let base = rect.y + rect.h - 5.0;
    let t = ui.time() as f32;
    for i in 0..3 {
        let height = if playing {
            let phase = match i {
                0 => -0.5,
                1 => -0.2,
                _ => -0.7,
            } as f32;
            let a = ((t / 0.9 + phase).rem_euclid(1.0)) * std::f32::consts::TAU;
            let s = (a.sin() * 0.5 + 0.5).powi(2); // ease-in-out-ish
            4.0 + s * 10.0
        } else {
            5.0
        };
        let bx = x0 + i as f32 * 5.0;
        ui.rounded_rect(Rect::new(bx, base + 5.0 - height, 3.0, height), color, 1.0);
    }
}
