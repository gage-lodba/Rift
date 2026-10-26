//! Search view (`View::Search`).
//!
//! Spec: `ui-spec.md` §7.4.
//!
//! * h2 `Results for “{query}”` (curly quotes) as the first child;
//! * `.tabs` Songs · Artists · Albums — active is an accent pill;
//! * Songs: "Searching..." while busy, "No results." when empty, else the shared
//!   track list (source-less: a row click starts the results at that row);
//! * Artists/Albums: lazily loaded (`None` → "Searching..."), then a card grid
//!   (artist cards are round + centred, subscribers as the subtitle; album cards
//!   use `"{album_type} • {year}"`).

use crate::screens::{pages, track_list};
use crate::state::{Action, SearchTab, State};
use crate::ui::{Rect, Ui};

/// Height of the `Results for …` h2 plus its 14 px bottom margin.
pub const SEARCH_HEAD_H: f32 = pages::H2_LINE + pages::H2_MARGIN_BOTTOM;

fn tab_content_height(ui: &mut Ui, state: &State, width: f32) -> f32 {
    match state.search.tab {
        SearchTab::Songs => {
            if state.search.busy || state.search.songs.is_empty() {
                pages::EMPTY_H
            } else {
                track_list::list_height(state.search.songs.len())
            }
        }
        SearchTab::Artists => match &state.search.artists {
            Some(artists) if !artists.is_empty() => pages::grid_height(ui, width, artists.len()),
            _ => pages::EMPTY_H,
        },
        SearchTab::Albums => match &state.search.albums {
            Some(albums) if !albums.is_empty() => pages::grid_height(ui, width, albums.len()),
            _ => pages::EMPTY_H,
        },
    }
}

/// Content height (no main padding) for [`view`].
pub fn height(ui: &mut Ui, state: &State, width: f32) -> f32 {
    SEARCH_HEAD_H + pages::TAB_H + pages::TABS_MARGIN_BOTTOM + tab_content_height(ui, state, width)
}

/// Draw the search view at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect) {
    let query = if state.search.shown_query.is_empty() {
        state.search.query.clone()
    } else {
        state.search.shown_query.clone()
    };
    let theme = ui.theme().clone();
    ui.text(
        Rect::new(area.x, area.y, area.w, pages::H2_LINE),
        &format!("Results for \u{201c}{query}\u{201d}"),
        &crate::screens::cards::style_lh(
            17.0,
            crate::text::Weight::Bold,
            theme.text,
            pages::H2_LINE,
        ),
    );
    let mut y = area.y + SEARCH_HEAD_H;

    // ---- tabs ----
    let mut x = area.x;
    for (tab, label) in [
        (SearchTab::Songs, "Songs"),
        (SearchTab::Artists, "Artists"),
        (SearchTab::Albums, "Albums"),
    ] {
        let (w, resp) = pages::tab(ui, x, y, label, state.search.tab == tab, label);
        if resp.clicked && state.search.tab != tab {
            actions.push(Action::SetSearchTab(tab));
        }
        x += w + pages::TABS_GAP;
    }
    y += pages::TAB_H + pages::TABS_MARGIN_BOTTOM;

    // ---- content ----
    // The result lists are moved out of `State` for their draw (the row/card
    // painters need `&mut State` for cover requests) and restored immediately:
    // cloning every result per frame would allocate on the hot path.
    match state.search.tab {
        SearchTab::Songs => {
            if state.search.busy {
                pages::empty(ui, area.x, y, area.w, "Searching...");
            } else if state.search.songs.is_empty() {
                pages::empty(ui, area.x, y, area.w, "No results.");
            } else {
                let tracks = std::mem::take(&mut state.search.songs);
                let opts = track_list::TrackListOpts {
                    id: "search-songs",
                    ..track_list::TrackListOpts::default()
                };
                track_list::track_list(
                    ui,
                    state,
                    actions,
                    Rect::new(area.x, y, area.w, track_list::list_height(tracks.len())),
                    &tracks,
                    &opts,
                );
                state.search.songs = tracks;
            }
        }
        SearchTab::Artists => {
            let artists = state.search.artists.take();
            match artists.as_ref() {
                Some(artists) if !artists.is_empty() => {
                    pages::artist_grid(ui, state, actions, area.x, y, area.w, artists);
                }
                _ => {
                    pages::empty(ui, area.x, y, area.w, "Searching...");
                }
            }
            state.search.artists = artists;
        }
        SearchTab::Albums => {
            let albums = state.search.albums.take();
            match albums.as_ref() {
                Some(albums) if !albums.is_empty() => {
                    pages::album_grid(ui, state, actions, area.x, y, area.w, albums);
                }
                _ => {
                    pages::empty(ui, area.x, y, area.w, "Searching...");
                }
            }
            state.search.albums = albums;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;
    use rift_types::Track;

    fn track(n: usize) -> Track {
        Track {
            id: format!("t{n}"),
            title: format!("Song {n}"),
            artist: "Artist".into(),
            album: Some("Album".into()),
            duration: Some(100),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    use crate::screens::menu::harness::Sim;

    #[test]
    fn tab_pills_emit_set_search_tab() {
        let mut state = State::default();
        state.view = crate::state::View::Search;
        state.search.query = "x".into();
        state.search.shown_query = "x".into();
        let area = Rect::new(359.0, 66.0, 767.0, 600.0);
        let mut sim = Sim::new();

        let tab_style = crate::screens::cards::style(
            13.0,
            crate::text::Weight::SemiBold,
            sim.ui.theme().text_dim,
        );
        let width = |sim: &mut Sim, label: &str| sim.ui.text_width(label, &tab_style) + 34.0;
        let songs_w = width(&mut sim, "Songs");
        let artists_w = width(&mut sim, "Artists");
        let tabs_y = area.y + SEARCH_HEAD_H + pages::TAB_H * 0.5;
        let artists = (area.x + songs_w + pages::TABS_GAP + artists_w * 0.5, tabs_y);
        let albums = (
            area.x
                + songs_w
                + pages::TABS_GAP
                + artists_w
                + pages::TABS_GAP
                + width(&mut sim, "Albums") * 0.5,
            tabs_y,
        );

        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            view(ui, &mut state, actions, area);
        };
        let actions = sim.click(artists, &mut draw);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SetSearchTab(crate::state::SearchTab::Artists))),
            "Artists tab: {actions:?}"
        );
        let actions = sim.click(albums, &mut draw);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SetSearchTab(crate::state::SearchTab::Albums))),
            "Albums tab: {actions:?}"
        );
    }
    #[test]
    fn empty_search_height_is_head_tabs_and_empty_state() {
        let mut ui = Ui::new();
        let state = State::default();
        let h = height(&mut ui, &state, 766.0);
        assert_eq!(h, 36.0 + 31.0 + 16.0 + 113.0);
    }

    #[test]
    fn songs_tab_height_tracks_the_result_count() {
        let mut ui = Ui::new();
        let mut state = State::default();
        state.search.songs = (0..3).map(track).collect();
        let h = height(&mut ui, &state, 766.0);
        assert_eq!(h, 36.0 + 31.0 + 16.0 + 3.0 * 54.0);
    }

    #[test]
    fn busy_search_shows_an_empty_state_even_with_stale_songs() {
        let mut ui = Ui::new();
        let mut state = State::default();
        state.search.songs = (0..3).map(track).collect();
        state.search.busy = true;
        let h = height(&mut ui, &state, 766.0);
        assert_eq!(h, 36.0 + 31.0 + 16.0 + 113.0);
    }
}
