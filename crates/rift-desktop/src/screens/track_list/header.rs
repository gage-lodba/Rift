//! Collection header: the `.collection-meta` line and the `.head-actions` row
//! (Play pill + Download `.head-btn` state machine).

use rift_types::Track;

use crate::screens::cards;
use crate::state::{fmt_total_duration, playlist_fully_downloaded, Action, State};
use crate::text::Weight;
use crate::ui::{Color, Icon, Id, Rect, Ui};

// ---------------------------------------------------------------------------
// collection header
// ---------------------------------------------------------------------------

/// `"N songs • H hr M min"` (or `"1 song • …"`), the collection subtitle.
pub fn collection_meta_text(tracks: &[Track]) -> String {
    let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
    let n = tracks.len();
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} song{plural} • {}", fmt_total_duration(total))
}

/// Height of the `.collection-meta` line (13 px, line box 17).
pub const COLLECTION_META_H: f32 = 17.0;

/// Draw the `.collection-meta` line. Returns the y below it.
pub fn collection_meta(ui: &mut Ui, x: f32, y: f32, w: f32, tracks: &[Track]) -> f32 {
    let theme = ui.theme().clone();
    let text = collection_meta_text(tracks);
    ui.text(
        Rect::new(x, y, w, COLLECTION_META_H),
        &text,
        &cards::style_lh(13.0, Weight::Regular, theme.text_dim, COLLECTION_META_H),
    );
    y + COLLECTION_META_H
}

/// State of the collection-level Download button (`.head-btn`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadHeadState {
    /// Not downloaded: "Download".
    Download,
    /// N active: disabled spinner + "Downloading {`n_done+1}/{total`}".
    Downloading { done: usize, total: usize },
    /// All downloaded: "Downloaded" (accent). `pinned` = can't remove because
    /// another fully-downloaded playlist keeps these copies offline.
    Downloaded { pinned: bool },
}

impl DownloadHeadState {
    pub fn label(&self) -> String {
        match self {
            DownloadHeadState::Download => "Download".to_string(),
            DownloadHeadState::Downloading { done, total } => {
                format!("Downloading {}/{}", (done + 1).min(*total), total)
            }
            DownloadHeadState::Downloaded { .. } => "Downloaded".to_string(),
        }
    }

    pub fn icon(&self) -> Icon {
        match self {
            DownloadHeadState::Download => Icon::Download,
            DownloadHeadState::Downloading { .. } => Icon::Download,
            DownloadHeadState::Downloaded { .. } => Icon::CheckCircle,
        }
    }

    /// Accent styling for the "Downloaded" state only.
    pub fn accent(&self) -> bool {
        matches!(self, DownloadHeadState::Downloaded { .. })
    }

    pub fn enabled(&self) -> bool {
        match self {
            DownloadHeadState::Download => true,
            DownloadHeadState::Downloading { .. } => false,
            DownloadHeadState::Downloaded { pinned } => !pinned,
        }
    }
}

/// Pure state machine for the collection Download button, mirroring the old
/// `App::actions()` closure (downloading wins over downloaded, pinned disables
/// removal). Split out so it is testable without a UI.
pub fn download_head_state(
    total: usize,
    done: usize,
    active: usize,
    pinned: bool,
) -> DownloadHeadState {
    if active > 0 {
        DownloadHeadState::Downloading { done, total }
    } else if total > 0 && done >= total {
        DownloadHeadState::Downloaded { pinned }
    } else {
        DownloadHeadState::Download
    }
}

/// Whether this collection is the collection currently loaded in the player, so
/// the Play pill can toggle instead of restarting it (old `is_current`).
pub fn is_current_collection(state: &State, source: Option<&str>) -> bool {
    let source = match source {
        Some(s) => s,
        None => return false,
    };
    source == state.queue.source.as_deref().unwrap_or("")
        && matches!(
            state.playback,
            rift_types::PlaybackState::Playing
                | rift_types::PlaybackState::Paused
                | rift_types::PlaybackState::Loading
        )
}

/// Whether any of `tracks` is pinned offline by a fully-downloaded playlist
/// *other than* the one in view (the collection-level block rule).
pub(crate) fn blocked_by_other_playlist(
    state: &State,
    tracks: &[Track],
    source: Option<&str>,
) -> bool {
    let own = source.and_then(|s| s.strip_prefix("playlist:"));
    state.library.playlists.iter().any(|p| {
        Some(p.id.as_str()) != own
            && playlist_fully_downloaded(p, &state.downloads.downloaded)
            && p.tracks
                .iter()
                .any(|pt| tracks.iter().any(|t| t.id == pt.id || t.same_song(pt)))
    })
}

/// `.play-all` / `.head-btn` height: 8 px padding + 19 px line + 8 px.
pub const HEAD_BTN_H: f32 = 35.0;
/// `.play-all` padding: 8px 22px.
pub const PLAY_ALL_PAD_X: f32 = 22.0;
/// `.head-btn` padding: 8px 18px.
pub const HEAD_BTN_PAD_X: f32 = 18.0;
/// `.head-actions { margin: 18px 0 16px }`.
pub const HEAD_ACTIONS_MARGIN_TOP: f32 = 18.0;
pub const HEAD_ACTIONS_MARGIN_BOTTOM: f32 = 16.0;
/// Gap between the Play pill and the Download button.
pub const HEAD_ACTIONS_GAP: f32 = 10.0;
/// Height of a whole `.head-actions` row including its margins.
pub const HEAD_ACTIONS_H: f32 = HEAD_ACTIONS_MARGIN_TOP + HEAD_BTN_H + HEAD_ACTIONS_MARGIN_BOTTOM;

/// Position + collection context for one [`head_actions`] row (bundled so the
/// immediate-mode widget call stays below clippy's argument lint).
#[derive(Clone, Copy, Debug)]
pub struct HeadActions<'a> {
    /// Left edge of the collection content.
    pub x: f32,
    /// Bottom of the previous element (the collection-meta line).
    pub y: f32,
    pub tracks: &'a [Track],
    /// Collection's queue tag (`"liked"`, `"playlist:<id>"`, ...).
    pub source: Option<&'a str>,
    /// Namespaces the widget ids.
    pub id_key: &'a str,
}

/// Draw the `.head-actions` row for a collection (Play pill + Download button).
///
/// The row starts `18px` below [`HeadActions::y`] and ends `16px` before the
/// returned y. Returns the y below the row (ready for the track list).
pub fn head_actions(
    ui: &mut Ui,
    state: &State,
    actions: &mut Vec<Action>,
    head: HeadActions<'_>,
) -> f32 {
    let theme = ui.theme().clone();
    if head.tracks.is_empty() {
        return head.y;
    }
    let row_y = head.y + HEAD_ACTIONS_MARGIN_TOP;
    let id = Id::from_str("head-actions").with(head.id_key);

    // ---- Play pill ----
    let is_current = is_current_collection(state, head.source);
    let show_pause = is_current
        && matches!(
            state.playback,
            rift_types::PlaybackState::Playing | rift_types::PlaybackState::Loading
        );
    let play_label = if show_pause { "Pause" } else { "Play" };
    let play_icon = if show_pause { Icon::Pause } else { Icon::Play };
    let play_text_w = ui.text_width(play_label, &cards::style(14.0, Weight::Bold, Color::WHITE));
    let play_w = PLAY_ALL_PAD_X * 2.0 + 16.0 + 7.0 + play_text_w;
    let play_rect = Rect::new(head.x, row_y, play_w.max(84.0), HEAD_BTN_H);
    let play_resp = cards::accent_button(
        ui,
        play_rect,
        id.with("play"),
        play_label,
        Some(play_icon),
        14.0,
        Weight::Bold,
        999.0,
    );
    if play_resp.clicked {
        if is_current {
            actions.push(Action::TogglePlay);
        } else {
            // Shuffle applies to the collection Play button only (rows always
            // start at the row that was clicked), exactly like the old UI.
            let start = if state.queue.shuffle && head.tracks.len() > 1 {
                crate::state::shuffled_start(ui.time().fract(), head.tracks.len())
            } else {
                0
            };
            actions.push(Action::PlayTracks {
                tracks: head.tracks.to_vec(),
                start,
                source: head.source.map(str::to_owned),
            });
        }
    }

    // ---- Download button ----
    let ids: Vec<String> = head.tracks.iter().map(|t| t.id.clone()).collect();
    let total = ids.len();
    let done = ids
        .iter()
        .filter(|id| state.downloads.downloaded.contains(*id))
        .count();
    let active = ids
        .iter()
        .filter(|id| state.downloads.downloading.contains(*id))
        .count();
    let pinned = blocked_by_other_playlist(state, head.tracks, head.source);
    let dl_state = download_head_state(total, done, active, pinned);
    let dl_label = dl_state.label();
    let dl_text_w = ui.text_width(&dl_label, &cards::style(14.0, Weight::SemiBold, theme.text));
    let dl_w = HEAD_BTN_PAD_X * 2.0 + 16.0 + 7.0 + dl_text_w;
    let dl_rect = Rect::new(
        play_rect.right() + HEAD_ACTIONS_GAP,
        row_y,
        dl_w.max(96.0),
        HEAD_BTN_H,
    );
    let dl_id = id.with("download");
    if dl_state.enabled() {
        let resp = cards::secondary_pill(
            ui,
            dl_rect,
            dl_id,
            &dl_label,
            Some(dl_state.icon()),
            dl_state.accent(),
        );
        if resp.clicked {
            match dl_state {
                DownloadHeadState::Download => actions.push(Action::DownloadTracks(ids)),
                DownloadHeadState::Downloaded { .. } => actions.push(Action::RemoveDownloads(ids)),
                DownloadHeadState::Downloading { .. } => {}
            }
        }
    } else {
        paint_disabled_head_button(ui, dl_rect, &dl_label, dl_state, &theme);
    }

    row_y + HEAD_BTN_H + HEAD_ACTIONS_MARGIN_BOTTOM
}

fn paint_disabled_head_button(
    ui: &mut Ui,
    rect: Rect,
    label: &str,
    dl_state: DownloadHeadState,
    theme: &crate::theme::Theme,
) {
    let accent = dl_state.accent();
    ui.bordered_rect(
        rect,
        theme.bg_raised,
        999.0,
        1.0,
        if accent {
            theme.accent_dim
        } else {
            theme.border
        },
    );
    let st = cards::style(
        14.0,
        if accent {
            Weight::Bold
        } else {
            Weight::SemiBold
        },
        if accent { theme.accent } else { theme.text },
    );
    let text_w = ui.text_width(label, &st);
    let inner_w = 16.0 + 7.0 + text_w;
    let start_x = rect.x + (rect.w - inner_w) * 0.5;
    let icon_rect = Rect::new(start_x, rect.y + (rect.h - 16.0) * 0.5, 16.0, 16.0);
    if matches!(dl_state, DownloadHeadState::Downloading { .. }) {
        cards::spinner(ui, icon_rect.center_v(), 14.0, theme.text_dim);
    } else {
        ui.icon(icon_rect, dl_state.icon(), theme.accent, 16.0);
    }
    ui.text(
        Rect::new(start_x + 16.0 + 7.0, rect.y, text_w + 2.0, rect.h),
        label,
        &st,
    );
}
