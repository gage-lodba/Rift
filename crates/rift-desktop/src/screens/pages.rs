//! The routed page views and their shared building blocks.
//!
//! `screens::root` clips the main area and calls [`page`]; this module computes
//! the scroll container (`.main` resets/keeps one scroll offset across view
//! changes, like the old single `<main>` element) and dispatches to the per-view
//! modules by `state.view`.
//!
//! The scroll container uses [`Ui::scroll_area_stable`] so the 10 px gutter is
//! always reserved exactly as `main_content_rect` already accounts for: the
//! scroll viewport is the full `l.main` rectangle (whose right 10 px is the
//! gutter), so the painted scrollbar lands inside the reserved gutter and the
//! content column is `766 px` at the reference size (sidebar 335).
//!
//! Each view module exposes:
//!
//! * `pub fn height(ui, state, width) -> f32` — the content height *without* the
//!   main padding (the scroll area needs it before drawing);
//! * `pub fn view(ui, state, actions, area: Rect)` — draws the view at `area`
//!   (top-left = first content pixel).
//!
//! Ownership: `screens-pages`. Root composition stays in `mod.rs`.

use rift_types::{AlbumSummary, ArtistSummary};

use crate::screens::{album, artist, cards, layout, liked, playlist, search, settings, track_list};
use crate::state::{Action, State, View};
use crate::text::Weight;
use crate::ui::{Color, Id, Rect, Response, Sense, Ui, SCROLLBAR_WIDTH};

/// `.empty { padding: 48px 0; font-size: 13px }` line box 17 → 113 px.
pub const EMPTY_H: f32 = 48.0 * 2.0 + 17.0;

/// `h2` line box (17/700) — the first-child view heading.
pub const H2_LINE: f32 = 22.0;
/// `.view-head { margin-bottom: 4px }`.
pub const VIEW_HEAD_MARGIN_BOTTOM: f32 = 4.0;
/// `.h2 { margin: 18px 0 14px }` (non-first child).
pub const H2_MARGIN_TOP: f32 = 18.0;
pub const H2_MARGIN_BOTTOM: f32 = 14.0;
/// `.page-title` 30/800 line box 40.
pub const PAGE_TITLE_LINE: f32 = 40.0;
/// `.page-kind` 11/700 line box 15 + `margin-bottom: 4`.
pub const PAGE_KIND_H: f32 = 15.0 + 4.0;
/// `.page-head { gap: 22px; margin-bottom: 24px }`.
pub const PAGE_HEAD_GAP: f32 = 22.0;
pub const PAGE_HEAD_MARGIN_BOTTOM: f32 = 24.0;
/// Artist avatar is 150x150 round.
pub const ARTIST_AVATAR: f32 = 150.0;
/// Album art is 170x170, radius 8.
pub const ALBUM_ART: f32 = 170.0;
/// `.page-sub { margin-top: 6px }` + line box 17.
pub const PAGE_SUB_H: f32 = 6.0 + 17.0;
/// `.tab { padding: 6px 16px; border: 1px }` → 17 + 12 + 2.
pub const TAB_H: f32 = 31.0;
/// `.tabs { gap: 8px; margin-bottom: 16px }`.
pub const TABS_GAP: f32 = 8.0;
pub const TABS_MARGIN_BOTTOM: f32 = 16.0;
/// `.card-grid { margin-bottom: 26px }`.
pub const CARD_GRID_MARGIN_BOTTOM: f32 = 26.0;

// ---------------------------------------------------------------------------
// dispatcher
// ---------------------------------------------------------------------------

/// Draw the routed view (everything except Home, which the root draws itself).
/// Returns `false` when the view is Home, so the root can fall back.
pub fn page(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>) -> bool {
    let view = state.view.clone();
    if matches!(view, View::Home) {
        return false;
    }
    let l = layout(ui, state);
    let theme = ui.theme().clone();
    let pad_top = theme.main_padding_v;
    let pad_bottom = theme.main_padding_v;
    let width = content_width(ui, l.main);

    let content_h = match &view {
        View::Home => 0.0,
        View::Search => search::height(ui, state, width),
        View::Liked => liked::height(ui, state, width),
        View::Settings => settings::height(ui, state, width),
        View::Playlist(id) => playlist::height(ui, state, width, id),
        View::Artist(id) => artist::height(ui, state, width, id),
        View::ArtistSongs(id) => artist::songs_height(ui, state, width, id),
        View::Album(id) => album::height(ui, state, width, id),
    };
    let total_h = content_h + pad_top + pad_bottom;
    ui.scroll_area_stable(
        Id::from_str("main-scroll"),
        l.main,
        total_h,
        |ui, content| {
            let area = Rect::new(
                content.x + theme.main_padding_h,
                content.y + pad_top,
                (content.w - theme.main_padding_h * 2.0).max(0.0),
                content_h,
            );
            match &view {
                View::Home => {}
                View::Search => search::view(ui, state, actions, area),
                View::Liked => liked::view(ui, state, actions, area),
                View::Settings => settings::view(ui, state, actions, area),
                View::Playlist(id) => playlist::view(ui, state, actions, area, id),
                View::Artist(id) => artist::view(ui, state, actions, area, id),
                View::ArtistSongs(id) => artist::songs_view(ui, state, actions, area, id),
                View::Album(id) => album::view(ui, state, actions, area, id),
            }
        },
    );
    true
}

/// Content column width inside `main`: the same number `main_content_rect` gives
/// (main minus `24px` horizontal padding each side and the reserved 10 px
/// scrollbar gutter).
pub fn content_width(ui: &Ui, main: Rect) -> f32 {
    let theme = ui.theme();
    (main.w - theme.main_padding_h * 2.0 - SCROLLBAR_WIDTH).max(0.0)
}

// ---------------------------------------------------------------------------
// shared page primitives (measured from ui-spec.md §7 "common building blocks")
// ---------------------------------------------------------------------------

/// A first-child `h2` (17/700, line 22) with the `.view-head`'s 4 px bottom
/// margin. Returns the y below the view head.
pub fn view_head(ui: &mut Ui, x: f32, y: f32, w: f32, title: &str) -> f32 {
    let theme = ui.theme().clone();
    ui.text(
        Rect::new(x, y, w, H2_LINE),
        title,
        &cards::style_lh(17.0, Weight::Bold, theme.text, H2_LINE),
    );
    y + H2_LINE + VIEW_HEAD_MARGIN_BOTTOM
}

/// A `view-head` with a trailing text action (".link-btn", e.g. "Show all").
/// Returns `(y_below, response)`.
pub fn view_head_action(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    action_label: &str,
) -> (f32, Response) {
    let theme = ui.theme().clone();
    ui.text(
        Rect::new(x, y, w, H2_LINE),
        title,
        &cards::style_lh(17.0, Weight::Bold, theme.text, H2_LINE),
    );
    let style = cards::style(13.0, Weight::SemiBold, theme.text_dim);
    let action_w = ui.text_width(action_label, &style) + 12.0;
    let rect = Rect::new(x + w - action_w, y + 2.0, action_w, H2_LINE - 4.0);
    let resp = ui.interact(
        rect,
        Id::from_str("view-head-action").with(title),
        Sense::CLICK,
    );
    let color = if resp.hovered {
        theme.text
    } else {
        theme.text_dim
    };
    ui.text_centered(rect, action_label, &style.with_color(color));
    if resp.hovered {
        let w = ui.text_width(action_label, &style);
        ui.hline(
            rect.x + (rect.w - w) * 0.5,
            rect.y + rect.h - 2.0,
            w,
            1.0,
            color,
        );
    }
    if resp.focus_visible {
        ui.focus_ring(rect, 4.0);
    }
    (y + H2_LINE + VIEW_HEAD_MARGIN_BOTTOM, resp)
}

/// A `.page-kind` line: 11/700 uppercase, letter-spacing 1.5, line 15, then its
/// 4 px bottom margin. Returns the y below it.
pub fn page_kind(ui: &mut Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let theme = ui.theme().clone();
    let style = cards::style_lh(11.0, Weight::Bold, theme.text_dim, 15.0).with_letter_spacing(1.5);
    ui.text(Rect::new(x, y, w, 15.0), &text.to_uppercase(), &style);
    y + PAGE_KIND_H
}

/// A `.page-title` (30/800, line 40). Returns the y below it.
pub fn page_title(ui: &mut Ui, x: f32, y: f32, w: f32, title: &str) -> f32 {
    let theme = ui.theme().clone();
    ui.text(
        Rect::new(x, y, w, PAGE_TITLE_LINE),
        title,
        &cards::style_lh(30.0, Weight::Bold, theme.text, PAGE_TITLE_LINE),
    );
    y + PAGE_TITLE_LINE
}

/// A `.page-sub` line (13 dim, line 17) including its 6 px top margin. Returns
/// the y below it.
pub fn page_sub(ui: &mut Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let theme = ui.theme().clone();
    let style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, 17.0);
    ui.text(Rect::new(x, y + 6.0, w, 17.0), text, &style);
    y + PAGE_SUB_H
}

/// An `.empty` state (13 dim, centred, 48 px vertical padding). Returns the
/// height used (113 px).
pub fn empty(ui: &mut Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let theme = ui.theme().clone();
    let style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, 17.0);
    ui.text_centered(Rect::new(x, y + 48.0, w, 17.0), text, &style);
    EMPTY_H
}

/// A non-first `h2` section heading (18 px top margin, 22 line, 14 bottom).
/// Returns the y below it.
pub fn section(ui: &mut Ui, x: f32, y: f32, w: f32, title: &str) -> f32 {
    cards::section_header(ui, x, y + H2_MARGIN_TOP, w, title)
}

/// Height of a `h2` with its margins (18 + 22 + 14).
pub const SECTION_H: f32 = H2_MARGIN_TOP + H2_LINE + H2_MARGIN_BOTTOM;

// ---------------------------------------------------------------------------
// card grids
// ---------------------------------------------------------------------------

/// Height of a card grid with `n` cards in `width` (rows of 178-ish cards, 18 px
/// gap, `.card-grid` bottom margin excluded so callers can decide).
pub fn grid_height(ui: &Ui, width: f32, n: usize) -> f32 {
    let theme = ui.theme();
    let (cols, card_w) = cards::grid_metrics(width, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    let rows = n.div_ceil(cols.max(1));
    if rows == 0 {
        return 0.0;
    }
    rows as f32 * card_h + rows.saturating_sub(1) as f32 * theme.card_gap
}

/// `"Album • 2024"` card subtitle (`.card-sub`).
pub fn album_subtitle(album: &AlbumSummary) -> String {
    let mut out = String::new();
    write_album_subtitle(&mut out, album);
    out
}

/// Write [`album_subtitle`] into an existing buffer (grids reuse one buffer for
/// all cards instead of allocating a `String` per card per frame).
fn write_album_subtitle(out: &mut String, album: &AlbumSummary) {
    out.clear();
    out.push_str(&album.album_type);
    if let Some(year) = album.year {
        let _ = std::fmt::write(out, format_args!(" • {year}"));
    }
}

/// Compact subscriber count: `"1 subscriber"`, `"12K subscribers"`,
/// `"1.2M subscribers"` (ported from the old `format_subs`).
pub fn format_subscribers(n: u64) -> String {
    fn compact(x: f64, unit: &str) -> String {
        let s = format!("{x:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    }
    match n {
        1 => "1 subscriber".into(),
        n if n >= 1_000_000 => format!("{} subscribers", compact(n as f64 / 1e6, "M")),
        n if n >= 1_000 => format!("{} subscribers", compact(n as f64 / 1e3, "K")),
        n => format!("{n} subscribers"),
    }
}

/// Draw an album card grid. Returns the grid height (without the 26 px bottom
/// margin).
pub fn album_grid(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    x: f32,
    y: f32,
    w: f32,
    albums: &[AlbumSummary],
) -> f32 {
    let theme = ui.theme().clone();
    let (cols, card_w) = cards::grid_metrics(w, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    let mut sub = String::with_capacity(24);
    for (i, album) in albums.iter().enumerate() {
        let row = i / cols.max(1);
        let col = i % cols.max(1);
        let rect = Rect::new(
            x + col as f32 * (card_w + theme.card_gap),
            y + row as f32 * (card_h + theme.card_gap),
            card_w,
            card_h,
        );
        write_album_subtitle(&mut sub, album);
        let resp = cards::card(
            ui,
            state,
            rect,
            Id::from_str("album-card").with(&album.id),
            &album.cover,
            &album.name,
            &sub,
            false,
        );
        if resp.clicked {
            actions.push(Action::OpenAlbum(album.id.clone()));
        }
    }
    grid_height(ui, w, albums.len())
}

/// Draw an artist card grid (round avatar, centred text, subscriber subtitle).
pub fn artist_grid(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    x: f32,
    y: f32,
    w: f32,
    artists: &[ArtistSummary],
) -> f32 {
    let theme = ui.theme().clone();
    let (cols, card_w) = cards::grid_metrics(w, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    for (i, artist) in artists.iter().enumerate() {
        let row = i / cols.max(1);
        let col = i % cols.max(1);
        let rect = Rect::new(
            x + col as f32 * (card_w + theme.card_gap),
            y + row as f32 * (card_h + theme.card_gap),
            card_w,
            card_h,
        );
        let sub = artist
            .subscribers
            .map(format_subscribers)
            .unwrap_or_default();
        let resp = cards::card(
            ui,
            state,
            rect,
            Id::from_str("artist-card").with(&artist.id),
            &artist.avatar,
            &artist.name,
            &sub,
            true,
        );
        if resp.clicked {
            actions.push(Action::OpenArtist(artist.id.clone()));
        }
    }
    grid_height(ui, w, artists.len())
}

// ---------------------------------------------------------------------------
// tabs (search view)
// ---------------------------------------------------------------------------

/// One `.tab` pill. Returns its click response.
pub fn tab(
    ui: &mut Ui,
    x: f32,
    y: f32,
    label: &str,
    active: bool,
    id_key: &str,
) -> (f32, Response) {
    let theme = ui.theme().clone();
    let style = cards::style(
        13.0,
        Weight::SemiBold,
        if active { Color::WHITE } else { theme.text_dim },
    );
    let w = ui.text_width(label, &style) + 32.0 + 2.0;
    let rect = Rect::new(x, y, w, TAB_H);
    let resp = ui.interact(rect, Id::from_str("tab").with(id_key), Sense::CLICK);
    let (bg, border, fg) = if active {
        (theme.accent, theme.accent, Color::WHITE)
    } else {
        let t = resp.hover_t;
        (
            theme.bg_raised,
            theme.border,
            cards::mix(theme.text_dim, theme.text, t),
        )
    };
    ui.bordered_rect(rect, bg, 999.0, 1.0, border);
    ui.text_centered(rect, label, &style.with_color(fg));
    if resp.focus_visible {
        ui.focus_ring(rect, 999.0);
    }
    (w, resp)
}

/// Align helper for the metadata blocks of `page-head` (the old CSS uses
/// `align-items: center`).
pub fn centered_meta_y(head_top: f32, head_h: f32, meta_h: f32) -> f32 {
    head_top + ((head_h - meta_h) * 0.5).max(0.0)
}

/// Draw a round artist avatar (150 px) with the fallback tile. Returns the y
/// unchanged; callers position the meta block.
pub fn artist_avatar(ui: &mut Ui, state: &mut State, x: f32, y: f32, url: &str) {
    let rect = Rect::new(x, y, ARTIST_AVATAR, ARTIST_AVATAR);
    let texture = state.request_cover(url);
    cards::cover_tile(ui, rect, texture, ARTIST_AVATAR * 0.5);
}

/// Draw album art (170 px, radius 8) with the fallback tile.
pub fn album_art(ui: &mut Ui, state: &mut State, x: f32, y: f32, url: &str) {
    let rect = Rect::new(x, y, ALBUM_ART, ALBUM_ART);
    let texture = state.request_cover(url);
    cards::cover_tile(ui, rect, texture, 8.0);
}

/// A collection-meta + head-actions block height (what [`track_list::head_actions`]
/// uses before the rows start).
pub const HEAD_ACTIONS_BLOCK_H: f32 = track_list::HEAD_ACTIONS_H;

/// Position + collection context for [`collection_head`] (bundled so the
/// immediate-mode widget call stays below clippy's argument lint).
#[derive(Clone, Copy, Debug)]
pub struct CollectionHead<'a> {
    /// Left edge of the collection content.
    pub x: f32,
    /// Top of the `.collection-meta` line.
    pub y: f32,
    /// Usable content width (meta line + action row).
    pub w: f32,
    pub tracks: &'a [rift_types::Track],
    /// Collection's queue tag (`"liked"`, `"playlist:<id>"`, ...).
    pub source: Option<&'a str>,
    /// Namespaces the widget ids.
    pub id_key: &'a str,
}

/// Draw the collection metadata + Play/Download row, returning the y where the
/// track list starts.
pub fn collection_head(
    ui: &mut Ui,
    state: &State,
    actions: &mut Vec<Action>,
    head: CollectionHead<'_>,
) -> f32 {
    let y = track_list::collection_meta(ui, head.x, head.y, head.w, head.tracks);
    track_list::head_actions(
        ui,
        state,
        actions,
        track_list::HeadActions {
            x: head.x,
            y,
            tracks: head.tracks,
            source: head.source,
            id_key: head.id_key,
        },
    )
}

/// Height of the [`collection_head`] block (meta line + action row).
pub const COLLECTION_HEAD_H: f32 = track_list::COLLECTION_META_H + track_list::HEAD_ACTIONS_H;

#[cfg(test)]
mod tests {
    use super::*;
    use rift_types::AlbumSummary;

    #[test]
    fn empty_state_is_113_tall() {
        assert_eq!(EMPTY_H, 113.0);
    }

    #[test]
    fn collection_head_height_is_meta_plus_actions() {
        assert_eq!(COLLECTION_HEAD_H, 17.0 + 69.0);
    }

    #[test]
    fn subscribers_format_compacts() {
        assert_eq!(format_subscribers(1), "1 subscriber");
        assert_eq!(format_subscribers(999), "999 subscribers");
        assert_eq!(format_subscribers(12_000), "12K subscribers");
        assert_eq!(format_subscribers(1_200_000), "1.2M subscribers");
        assert_eq!(format_subscribers(1_000_000), "1M subscribers");
    }

    #[test]
    fn album_subtitle_has_type_and_year() {
        let album = AlbumSummary {
            id: "a".into(),
            name: "Album".into(),
            cover: String::new(),
            artist: "Artist".into(),
            artist_id: None,
            year: Some(2024),
            album_type: "Album".into(),
        };
        assert_eq!(album_subtitle(&album), "Album • 2024");
        let no_year = AlbumSummary {
            year: None,
            ..album
        };
        assert_eq!(album_subtitle(&no_year), "Album");
    }

    /// Run one full frame of `pages::page` for `state` and return the paint list.
    fn frame(state: &mut State) -> crate::gfx::paint::PaintList {
        let mut ui = Ui::new();
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let input = crate::ui::InputState {
            viewport,
            mouse_pos: (700.0, 407.0),
            mouse_in_window: true,
            time: 1.0,
            ..Default::default()
        };
        ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        assert!(page(&mut ui, state, &mut actions), "page handled");
        ui.end_frame()
    }

    fn state_for(view: &str) -> State {
        let mut state = crate::preview::preview_state();
        state.sidebar_width = 335.0;
        state.view = match view {
            "search" => View::Search,
            "liked" => View::Liked,
            "settings" => View::Settings,
            "playlist" => View::Playlist("preview-pl-1".into()),
            "artist" => View::Artist("preview-artist-1".into()),
            "artist-songs" => View::ArtistSongs("preview-artist-1".into()),
            "album" => View::Album("preview-album-1".into()),
            other => panic!("unknown view {other}"),
        };
        match view {
            "search" => {
                let (tracks, artists, albums) = crate::preview::preview_search_results();
                state.search.query = "Song Title".into();
                state.search.shown_query = "Song Title".into();
                state.search.songs = tracks;
                state.search.artists = Some(artists);
                state.search.albums = Some(albums);
            }
            "artist" => state.artist_page = Some(crate::preview::preview_artist_page()),
            "artist-songs" => {
                state.artist_page = Some(crate::preview::preview_artist_page());
                state.artist_songs = Some((
                    "preview-artist-1".into(),
                    (1..=20).map(crate::preview::preview_track).collect(),
                ));
            }
            "album" => state.album_page = Some(crate::preview::preview_album_page()),
            _ => {}
        }
        state
    }

    #[test]
    fn every_page_paints_a_non_empty_frame() {
        for view in [
            "search",
            "liked",
            "settings",
            "playlist",
            "artist",
            "artist-songs",
            "album",
        ] {
            let mut state = state_for(view);
            let paint = frame(&mut state);
            assert!(!paint.is_empty(), "{view} painted nothing");
        }
    }

    #[test]
    fn search_artist_and_album_tabs_paint_grid_covers() {
        for tab in [
            crate::state::SearchTab::Artists,
            crate::state::SearchTab::Albums,
        ] {
            let mut state = state_for("search");
            state.search.tab = tab;
            let paint = frame(&mut state);
            let covers = paint
                .layers
                .iter()
                .flat_map(|l| l.prims.iter())
                .filter(
                    |p| matches!(p, crate::gfx::paint::Prim::Image { rect, .. } if rect.w > 100.0),
                )
                .count();
            assert!(covers >= 4, "{tab:?} tab painted {covers} card covers");
        }
    }

    #[test]
    fn artist_page_paints_the_album_and_single_grids() {
        let mut state = state_for("artist");
        let paint = frame(&mut state);
        // Preview artist page has 3 albums + 1 single → 4 card covers of
        // ~158 px at content width 767 (the 150 px round avatar is separate).
        let covers = paint
            .layers
            .iter()
            .flat_map(|l| l.prims.iter())
            .filter(|p| matches!(p, crate::gfx::paint::Prim::Image { rect, .. } if rect.w > 151.0 && rect.w < 170.0))
            .count();
        assert!(covers >= 4, "artist page painted {covers} card covers");
    }

    #[test]
    fn grid_height_matches_the_reference_library() {
        let ui = Ui::new();
        // 4 cards at content 766 = one row of 222.
        assert_eq!(grid_height(&ui, 766.0, 4), 222.0);
        // 5 cards = two rows (4 + 1): 222 + 18 + 222.
        let h = grid_height(&ui, 766.0, 5);
        assert!((h - 462.0).abs() < 0.01);
        assert_eq!(grid_height(&ui, 766.0, 0), 0.0);
    }
}
