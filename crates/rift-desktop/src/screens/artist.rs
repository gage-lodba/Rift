//! Artist views: the profile page (`View::Artist`) and the full catalogue
//! (`View::ArtistSongs`).
//!
//! Spec: `ui-spec.md` §7.5 / §7.6.
//!
//! Artist page: `.page-head` with the 150 px round avatar and the 30/800 name,
//! then "Top songs" (+ "Show all" link when the catalogue exists), the top-track
//! list, then "Albums" and "Singles" card grids (singles split out by
//! `album_type == "Single"`).
//!
//! `ArtistSongs`: the same page head with an "All songs" kind line, the artist name
//! as a link back to the profile, `"N songs • duration"` and the Play/Download
//! row (source `"artist:<id>"`), then the full track list.

use crate::screens::{cards, pages, track_list};
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Id, Rect, Sense, Ui};

const ARTIST_HEAD_H: f32 = pages::ARTIST_AVATAR + pages::PAGE_HEAD_MARGIN_BOTTOM;

/// Height of the artist page's `page-head` (150 avatar + 24 bottom margin).
pub const PAGE_HEAD_H: f32 = ARTIST_HEAD_H;

fn page_for<'a>(state: &'a State, id: &str) -> Option<&'a rift_types::ArtistPage> {
    state.artist_page.as_ref().filter(|page| page.id == id)
}

/// Content height (no main padding) for [`view`] (the artist profile).
pub fn height(ui: &mut Ui, state: &State, width: f32, id: &str) -> f32 {
    let Some(page) = page_for(state, id) else {
        return pages::EMPTY_H;
    };
    let mut h = PAGE_HEAD_H + pages::H2_LINE + pages::VIEW_HEAD_MARGIN_BOTTOM;
    h += track_list::list_height(page.tracks.len());
    let singles = page
        .albums
        .iter()
        .filter(|a| a.album_type == "Single")
        .count();
    let albums = page.albums.len() - singles;
    if albums > 0 {
        h += pages::SECTION_H;
        h += pages::grid_height(ui, width, albums) + pages::CARD_GRID_MARGIN_BOTTOM;
    }
    if singles > 0 {
        // The previous grid's 26px bottom margin collapses with the h2's 18px top
        // margin, so the gap is 26 (already counted above) when albums exist.
        if albums == 0 {
            h += pages::H2_MARGIN_TOP;
        }
        h += pages::H2_LINE + pages::H2_MARGIN_BOTTOM;
        h += pages::grid_height(ui, width, singles) + pages::CARD_GRID_MARGIN_BOTTOM;
    }
    h
}

/// Draw the artist profile at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect, id: &str) {
    // Move the page cache out of `State` for the frame: the avatar request and
    // the row painters need `&mut State`, so borrowing `state.artist_page`
    // directly would conflict. It goes back before returning.
    let page = state.artist_page.take();
    let Some(artist) = page.as_ref().filter(|page| page.id == id) else {
        state.artist_page = page;
        pages::empty(ui, area.x, area.y, area.w, "Loading artist...");
        return;
    };

    // ---- page head: avatar + name (align-items: center) ----
    pages::artist_avatar(ui, state, area.x, area.y, &artist.image);
    let meta_x = area.x + pages::ARTIST_AVATAR + pages::PAGE_HEAD_GAP;
    let meta_w = (area.w - pages::ARTIST_AVATAR - pages::PAGE_HEAD_GAP).max(0.0);
    let title_y = pages::centered_meta_y(area.y, pages::ARTIST_AVATAR, pages::PAGE_TITLE_LINE);
    pages::page_title(ui, meta_x, title_y, meta_w, &artist.name);
    let mut y = area.y + PAGE_HEAD_H;

    // ---- "Top songs" + "Show all" ----
    let show_all = artist.tracks_playlist_id.is_some();
    if show_all {
        let (next_y, resp) =
            pages::view_head_action(ui, area.x, y, area.w, "Top songs", "Show all");
        if resp.clicked {
            actions.push(Action::OpenArtistSongs(artist.id.clone()));
        }
        y = next_y;
    } else {
        y = pages::view_head(ui, area.x, y, area.w, "Top songs");
    }

    // ---- top tracks ----
    let opts = track_list::TrackListOpts {
        id: "artist-top",
        ..track_list::TrackListOpts::default()
    };
    track_list::track_list(
        ui,
        state,
        actions,
        Rect::new(
            area.x,
            y,
            area.w,
            track_list::list_height(artist.tracks.len()),
        ),
        &artist.tracks,
        &opts,
    );
    y += track_list::list_height(artist.tracks.len());

    // ---- albums / singles ----
    // `album_grid` takes `&[AlbumSummary]`, so the two filtered views are
    // collected. Album summaries are small (id/name/cover/artist strings) and a
    // page has a handful of them.
    let singles: Vec<rift_types::AlbumSummary> = artist
        .albums
        .iter()
        .filter(|a| a.album_type == "Single")
        .cloned()
        .collect();
    let albums: Vec<rift_types::AlbumSummary> = artist
        .albums
        .iter()
        .filter(|a| a.album_type != "Single")
        .cloned()
        .collect();
    if !albums.is_empty() {
        y = pages::section(ui, area.x, y, area.w, "Albums");
        y += pages::album_grid(ui, state, actions, area.x, y, area.w, &albums);
        y += pages::CARD_GRID_MARGIN_BOTTOM;
    }
    if !singles.is_empty() {
        if albums.is_empty() {
            y += pages::H2_MARGIN_TOP;
        }
        y = cards::section_header(ui, area.x, y, area.w, "Singles");
        y += pages::album_grid(ui, state, actions, area.x, y, area.w, &singles);
        y += pages::CARD_GRID_MARGIN_BOTTOM;
    }
    let _ = y;
    state.artist_page = page;
}

// ---------------------------------------------------------------------------
// ArtistSongs
// ---------------------------------------------------------------------------

/// The artist-songs page-head height (151 px when the catalogue is loaded: the
/// meta block outgrows the 150 px avatar; 150 px otherwise).
pub fn songs_head_height(state: &State, id: &str) -> f32 {
    let songs = state
        .artist_songs
        .as_ref()
        .filter(|(cached_id, _)| cached_id == id)
        .map(|(_, t)| t);
    let mut meta = pages::PAGE_KIND_H + pages::PAGE_TITLE_LINE;
    if let Some(tracks) = songs.filter(|t| !t.is_empty()) {
        meta += pages::PAGE_SUB_H + track_list::HEAD_ACTIONS_H;
        let _ = tracks;
    }
    (meta.max(pages::ARTIST_AVATAR)) + pages::PAGE_HEAD_MARGIN_BOTTOM
}

/// Content height (no main padding) for [`songs_view`].
pub fn songs_height(ui: &mut Ui, state: &State, _width: f32, id: &str) -> f32 {
    let head = songs_head_height(state, id);
    let songs = state
        .artist_songs
        .as_ref()
        .filter(|(cached_id, _)| cached_id == id)
        .map(|(_, t)| t);
    let content = match songs {
        None => pages::EMPTY_H,
        Some(t) if t.is_empty() => pages::EMPTY_H,
        Some(t) => track_list::list_height(t.len()),
    };
    let _ = ui;
    head + content
}

/// Draw the artist's full song catalogue at `area` (`View::ArtistSongs`).
pub fn songs_view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect, id: &str) {
    // Move both caches out for the frame (the avatar request and row painters
    // need `&mut State`); restore them before returning. The head height depends
    // on the catalogue, so measure it first.
    let head_h = songs_head_height(state, id);
    let page = state.artist_page.take();
    let artist = page.as_ref().filter(|page| page.id == id);
    let songs = state.artist_songs.take();
    let catalogue = songs.as_ref().filter(|(cached_id, _)| cached_id == id);

    let theme = ui.theme().clone();

    // ---- page head ----
    let image = artist.map(|a| a.image.as_str()).unwrap_or_default();
    pages::artist_avatar(ui, state, area.x, area.y, image);
    let meta_x = area.x + pages::ARTIST_AVATAR + pages::PAGE_HEAD_GAP;
    let meta_w = (area.w - pages::ARTIST_AVATAR - pages::PAGE_HEAD_GAP).max(0.0);
    let mut meta_h = pages::PAGE_KIND_H + pages::PAGE_TITLE_LINE;
    let has_songs = catalogue.is_some_and(|(_, tracks)| !tracks.is_empty());
    if has_songs {
        meta_h += pages::PAGE_SUB_H + track_list::HEAD_ACTIONS_H;
    }
    let mut meta_y =
        pages::centered_meta_y(area.y, head_h - pages::PAGE_HEAD_MARGIN_BOTTOM, meta_h);

    meta_y = pages::page_kind(ui, meta_x, meta_y, meta_w, "All songs");
    // The title is a link back to the artist profile (`.link`).
    let title_h = pages::PAGE_TITLE_LINE;
    let mut title_y = meta_y;
    if let Some(artist) = artist.filter(|a| !a.name.is_empty()) {
        let style = cards::style_lh(30.0, Weight::Bold, theme.text, title_h);
        let text_w = ui.text_width(&artist.name, &style).min(meta_w);
        let rect = Rect::new(meta_x, title_y, text_w + 4.0, title_h);
        let resp = ui.interact(rect, Id::from_str("artist-songs/artist-link"), Sense::CLICK);
        ui.text(rect, &artist.name, &style);
        if resp.hovered {
            ui.hline(rect.x, rect.y + title_h - 2.0, text_w, 1.0, theme.text_dim);
        }
        if resp.focus_visible {
            ui.focus_ring(rect, 4.0);
        }
        if resp.clicked {
            actions.push(Action::OpenArtist(id.to_string()));
        }
    } else {
        pages::page_title(ui, meta_x, title_y, meta_w, "");
    }
    title_y += title_h;
    meta_y = title_y;

    if let Some((_, tracks)) = catalogue.filter(|(_, tracks)| !tracks.is_empty()) {
        let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
        let sub = format!(
            "{} songs • {}",
            tracks.len(),
            crate::state::fmt_total_duration(total)
        );
        meta_y = pages::page_sub(ui, meta_x, meta_y, meta_w, &sub);
        let source = format!("artist:{id}");
        meta_y = track_list::head_actions(
            ui,
            state,
            actions,
            track_list::HeadActions {
                x: meta_x,
                y: meta_y,
                tracks,
                source: Some(source.as_str()),
                id_key: &source,
            },
        );
    }
    let _ = meta_y;

    let y = area.y + head_h;
    match catalogue {
        None => {
            pages::empty(ui, area.x, y, area.w, "Loading songs...");
        }
        Some((_, tracks)) if tracks.is_empty() => {
            pages::empty(ui, area.x, y, area.w, "No songs found.");
        }
        Some((_, tracks)) => {
            let source = format!("artist:{id}");
            let opts = track_list::TrackListOpts {
                source: Some(source.as_str()),
                id: "artist-songs",
                ..track_list::TrackListOpts::default()
            };
            track_list::track_list(
                ui,
                state,
                actions,
                Rect::new(area.x, y, area.w, track_list::list_height(tracks.len())),
                tracks,
                &opts,
            );
        }
    }
    state.artist_page = page;
    state.artist_songs = songs;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview;
    use crate::ui::Ui;

    #[test]
    fn loading_artist_is_just_the_empty_state() {
        let mut ui = Ui::new();
        let state = State::default();
        assert_eq!(height(&mut ui, &state, 766.0, "x"), pages::EMPTY_H);
    }

    #[test]
    fn artist_page_height_sections() {
        let mut ui = Ui::new();
        let state = State {
            artist_page: Some(preview::preview_artist_page()),
            ..State::default()
        };
        let page = state.artist_page.as_ref().unwrap();
        let singles = page
            .albums
            .iter()
            .filter(|a| a.album_type == "Single")
            .count();
        let albums = page.albums.len() - singles;
        let h = height(&mut ui, &state, 766.0, "preview-artist-1");
        let mut expected = 174.0 + 26.0 + page.tracks.len() as f32 * 54.0;
        expected += pages::SECTION_H;
        expected += pages::grid_height(&Ui::new(), 766.0, albums) + pages::CARD_GRID_MARGIN_BOTTOM;
        if singles > 0 {
            expected += pages::H2_LINE + pages::H2_MARGIN_BOTTOM;
            expected +=
                pages::grid_height(&Ui::new(), 766.0, singles) + pages::CARD_GRID_MARGIN_BOTTOM;
        }
        assert_eq!(h, expected);
    }

    #[test]
    fn artist_songs_head_is_151_with_catalogue() {
        let state = State {
            artist_songs: Some((
                "a".into(),
                vec![rift_types::Track {
                    id: "t".into(),
                    title: "Song".into(),
                    artist: String::new(),
                    album: None,
                    duration: Some(1),
                    cover: String::new(),
                    artists: Vec::new(),
                    album_id: None,
                }],
            )),
            ..State::default()
        };
        // 19 + 40 + 23 + 69 = 151, and the page-head margin adds 24.
        assert_eq!(songs_head_height(&state, "a"), 151.0 + 24.0);
        let mut ui = Ui::new();
        assert_eq!(songs_height(&mut ui, &state, 766.0, "a"), 175.0 + 54.0);
    }
}
