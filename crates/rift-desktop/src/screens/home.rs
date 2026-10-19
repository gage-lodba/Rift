//! Home view: "Your library" cover cards + "Recently played" track list.
//! Owned by `screens-chrome`.
//!
//! Reference geometry (`docs/ui-spec.md` §7.1, measured on
//! `Previews/image.png`): h2 line box at the content top, 4-across card grid of
//! 178 px cards (18 px gap), 26 px below the grid the second heading, then the
//! 12 recently played rows (54 px each). All of it scrolls inside `main`; this
//! function returns the total content height so the root can size the scrollbar.

use crate::state::{Action, State};
use crate::ui::{Id, Rect, Ui};

use super::{cards, menu, track_list};

/// Draw the Home view into `content` (the main area's padded content column).
/// Returns the total content height in logical px (used for the scrollbar).
pub fn home(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, content: Rect) -> f32 {
    let theme = ui.theme().clone();
    let mut y = content.y;

    // ---- "Your library" ----
    y = cards::section_header(ui, content.x, y, content.w, "Your library");

    let (cols, card_w) = cards::grid_metrics(content.w, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    let playlists = state.library.playlists.clone();
    let total = playlists.len() + 1;

    for slot in 0..total {
        let row = slot / cols;
        let col = slot % cols;
        let x = content.x + col as f32 * (card_w + theme.card_gap);
        let card_y = y + row as f32 * (card_h + theme.card_gap);
        let rect = Rect::new(x, card_y, card_w, card_h);

        if slot == 0 {
            // Liked Songs card: cover from the first liked track.
            let cover_url = state
                .library
                .liked
                .first()
                .map(|t| t.cover.clone())
                .unwrap_or_default();
            let resp = cards::card(
                ui,
                state,
                rect,
                Id::from_str("home/card/liked"),
                &cover_url,
                "Liked Songs",
                &format!("{} songs", state.library.liked.len()),
                false,
            );
            if resp.clicked {
                actions.push(Action::Navigate(crate::state::View::Liked));
            }
        } else {
            let playlist = &playlists[slot - 1];
            let id = playlist.id.clone();
            let name = playlist.name.clone();
            let count = playlist.tracks.len();
            let cover_url = playlist
                .tracks
                .first()
                .map(|t| t.cover.as_str())
                .unwrap_or("")
                .to_string();
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
            let kebab_id = Id::from_str("home/card-menu").with(&id);
            let hovering = ui.hovered(rect);
            let kebab_resp = if hovering {
                Some(ui.interact(kebab, kebab_id, crate::ui::Sense::CLICK))
            } else {
                None
            };
            let resp = cards::card(
                ui,
                state,
                rect,
                Id::from_str("home/card/playlist").with(&id),
                &cover_url,
                &name,
                &format!("{count} songs"),
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
                actions.push(Action::Navigate(crate::state::View::Playlist(id)));
            }
        }
    }
    let rows = total.div_ceil(cols.max(1));
    y += rows as f32 * card_h + rows.saturating_sub(1) as f32 * theme.card_gap;
    y += 26.0; // `.card-grid { margin-bottom: 26px }`

    // ---- "Recently played" ----
    y = cards::section_header(ui, content.x, y, content.w, "Recently played");

    let recent: Vec<rift_types::Track> = state
        .library
        .recently_played
        .iter()
        .take(12)
        .cloned()
        .collect();
    if recent.is_empty() {
        let empty_h = 17.0 + 96.0;
        ui.text_centered(
            Rect::new(content.x, y + 48.0, content.w, 17.0),
            "Nothing played yet. Search for a song to get started.",
            &cards::style_lh(13.0, crate::text::Weight::Regular, theme.text_dim, 17.0),
        );
        y += empty_h;
    } else {
        // The shared track list (screens-pages' `track_list.rs`) gives the rows
        // their context menus and correct nested-control click handling; Home is
        // the `single_click` variant (a row click plays just that song).
        let opts = track_list::TrackListOpts {
            source: None,
            single_click: true,
            playlist_id: None,
            show_album: true,
            show_download: true,
            show_like: true,
            id: "home/recent",
        };
        let full = Rect::new(
            content.x,
            y,
            content.w,
            recent.len() as f32 * cards::TROW_HEIGHT,
        );
        y += track_list::track_list(ui, state, actions, full, &recent, &opts);
    }

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
}
