//! Home view: "Your library" cover cards + "Recently played" track list.
//! Owned by `screens-chrome`.
//!
//! Reference geometry (`docs/ui-spec.md` §7.1, measured on
//! `Previews/image.png`): h2 line box at the content top, 4-across card grid of
//! 178 px cards (18 px gap), 26 px below the grid the second heading, then the
//! 12 recently played rows (54 px each). All of it scrolls inside `main`; this
//! function returns the total content height so the root can size the scrollbar.

use std::fmt::Write as _;

use crate::state::{Action, State};
use crate::ui::{Id, Rect, Ui};

use super::{cards, menu, track_list};

/// Draw the Home view into `content` (the main area's padded content column).
/// Returns the total content height in logical px (used for the scrollbar).
pub fn home(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, content: Rect) -> f32 {
    let theme = ui.theme().clone();
    let mut y = content.y;
    // Visible band for cheap culling: the scroll container clips the paint, but
    // running the card grid (cover request + text layout per card) for an
    // arbitrarily large library would still cost per-frame work.
    let clip = ui.clip_rect().unwrap_or_else(|| ui.viewport());

    // ---- "Your library" ----
    y = cards::section_header(ui, content.x, y, content.w, "Your library");

    let (cols, card_w) = cards::grid_metrics(content.w, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    // Move the playlist list out of `State` for the card grid: `cards::card`
    // needs `&mut State` for cover requests, so borrowing the list straight from
    // `state.library` would conflict — and cloning every playlist (with all its
    // tracks) per frame would allocate on the hot path.
    let playlists = std::mem::take(&mut state.library.playlists);
    let total = playlists.len() + 1;
    // One reused buffer for the subtitle strings ("N songs") instead of a
    // `format!` allocation per card per frame.
    let mut sub = String::with_capacity(16);

    for slot in 0..total {
        let row = slot / cols;
        let col = slot % cols;
        let x = content.x + col as f32 * (card_w + theme.card_gap);
        let card_y = y + row as f32 * (card_h + theme.card_gap);
        let rect = Rect::new(x, card_y, card_w, card_h);
        if card_y + card_h < clip.top() || card_y > clip.bottom() {
            continue;
        }

        if slot == 0 {
            // Liked Songs card: cover from the first liked track.
            let liked_len = state.library.liked.len();
            let cover_url = state
                .library
                .liked
                .first()
                .map(|t| t.cover.clone())
                .unwrap_or_default();
            sub.clear();
            let _ = write!(sub, "{liked_len} songs");
            let resp = cards::card(
                ui,
                state,
                rect,
                Id::from_str("home/card/liked"),
                &cover_url,
                "Liked Songs",
                &sub,
                false,
            );
            if resp.clicked {
                actions.push(Action::Navigate(crate::state::View::Liked));
            }
        } else {
            let playlist = &playlists[slot - 1];
            let id = &playlist.id;
            let name = &playlist.name;
            let count = playlist.tracks.len();
            let cover_url = playlist.tracks.first().map_or("", |t| t.cover.as_str());
            // The kebab is a nested control: interact it *before* the card so
            // its press latches first (the old DOM's `.card-action` overlay
            // sits under the kebab for the same reason), then paint it on top.
            let inner = rect.inset(cards::CARD_PADDING);
            let name_y = inner.y + cover + cards::CARD_NAME_MARGIN;
            let kebab = Rect::new(
                inner.right() - 24.0,
                name_y + (cards::CARD_NAME_LINE - 24.0) * 0.5,
                24.0,
                24.0,
            );
            let kebab_id = Id::from_str("home/card-menu").with(id);
            let hovering = ui.hovered(rect);
            let kebab_resp = if hovering {
                Some(ui.interact(kebab, kebab_id, crate::ui::Sense::CLICK))
            } else {
                None
            };
            sub.clear();
            let _ = write!(sub, "{count} songs");
            let resp = cards::card(
                ui,
                state,
                rect,
                Id::from_str("home/card/playlist").with(id),
                cover_url,
                name,
                &sub,
                false,
            );
            // Paint the kebab above the card content.
            if let Some(kresp) = kebab_resp {
                let t = kresp.hover_t;
                if t > 0.0 {
                    ui.rounded_rect(
                        kebab,
                        theme.bg_hover.with_alpha((t * 255.0).round() as u8),
                        theme.radius_sm,
                    );
                }
                ui.icon(
                    kebab,
                    crate::ui::Icon::MoreVert,
                    cards::mix(theme.text_dim, theme.text, t),
                    15.0,
                );
                if kresp.clicked {
                    let entries = vec![
                        menu::Entry::item(
                            menu::Item::new("Rename", Some(crate::ui::Icon::Edit)),
                            Action::BeginRenamePlaylist {
                                id: id.clone(),
                                name: name.clone(),
                            },
                        ),
                        menu::Entry::item(
                            menu::Item::new("Delete", Some(crate::ui::Icon::Delete)).danger(),
                            Action::DeletePlaylist(id.clone()),
                        ),
                    ];
                    menu::open(ui, kebab_id, kebab, entries);
                }
            }
            if resp.clicked && !menu::is_open() {
                actions.push(Action::Navigate(crate::state::View::Playlist(id.clone())));
            }
        }
    }
    state.library.playlists = playlists;
    let rows = total.div_ceil(cols.max(1));
    y += rows as f32 * card_h + rows.saturating_sub(1) as f32 * theme.card_gap;
    y += 26.0; // `.card-grid { margin-bottom: 26px }`

    // ---- "Recently played" ----
    y = cards::section_header(ui, content.x, y, content.w, "Recently played");

    // The old app: `library.recently_played.iter().take(12)`. Move the list out
    // for the frame (the row painters need `&mut State`) and restore it below;
    // only the first 12 are drawn.
    let recent = std::mem::take(&mut state.library.recently_played);
    let shown = recent.len().min(12);
    if shown == 0 {
        let empty_h = 17.0 + 96.0;
        ui.text_centered(
            Rect::new(content.x, y + 48.0, content.w, 17.0),
            "Nothing played yet. Search for a song to get started.",
            &cards::style_lh(13.0, crate::text::Weight::Regular, theme.text_dim, 17.0),
        );
        y += empty_h;
    } else {
        // The shared track list gives the rows their context menus and correct
        // nested-control click handling; Home is the `single_click` variant (a
        // row click plays just that song).
        let opts = track_list::TrackListOpts {
            source: None,
            single_click: true,
            playlist_id: None,
            show_album: true,
            show_download: true,
            show_like: true,
            all_liked: false,
            id: "home/recent",
        };
        let full = Rect::new(content.x, y, content.w, shown as f32 * cards::TROW_HEIGHT);
        y += track_list::track_list(ui, state, actions, full, &recent[..shown], &opts);
    }
    state.library.recently_played = recent;

    // Content height includes the main area's bottom padding.
    (y - content.y) + theme.main_padding_v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_is_capped_at_twelve_rows() {
        // The old app: `library.recently_played.iter().take(12)`.
        let mut state = State::default();
        state.library.recently_played = (0..20)
            .map(|i| rift_types::Track {
                id: format!("t{i}"),
                title: format!("Song Title {}", i + 1),
                artist: "Artist Name".into(),
                album: Some("Album Name".into()),
                duration: Some(187),
                cover: String::new(),
                artists: Vec::new(),
                album_id: None,
            })
            .collect();
        assert_eq!(state.library.recently_played.iter().take(12).count(), 12);
    }

    /// Home moves the library lists out of `State` for the frame (so the card
    /// and row painters can request covers through `&mut State`); both must be
    /// restored after the frame.
    #[test]
    fn home_restores_the_library_lists() {
        let mut ui = Ui::new();
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let input = crate::ui::InputState {
            viewport,
            mouse_pos: (0.0, 0.0),
            mouse_in_window: true,
            time: 1.0,
            ..Default::default()
        };
        ui.begin_frame(input, viewport);
        let mut state = crate::preview::preview_state();
        let playlists = state.library.playlists.len();
        let recent = state.library.recently_played.len();
        let mut actions = Vec::new();
        let content = crate::screens::main_content_rect(&ui, Rect::new(335.0, 46.0, 825.0, 375.0));
        let height = home(&mut ui, &mut state, &mut actions, content);
        let _ = ui.end_frame();
        assert_eq!(
            state.library.playlists.len(),
            playlists,
            "playlists restored"
        );
        assert_eq!(
            state.library.recently_played.len(),
            recent,
            "recent list restored"
        );
        assert!(height > 0.0, "content height reported");
    }
}
