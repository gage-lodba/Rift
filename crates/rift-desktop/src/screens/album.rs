//! Album view (`View::Album(id)`).
//!
//! Spec: `ui-spec.md` §7.7.
//!
//! Loading → `.empty` "Loading album...". Otherwise `.page-head` with the 170 px
//! cover (radius 8) and, vertically centred beside it:
//!
//! * `.page-kind` = `"{album_type} • {year}"` (uppercase, 11/700, tracking 1.5),
//! * `.page-title` = album name (30/800),
//! * `.page-sub` = artist as a link + `" • N songs • duration"`,
//! * the Play/Download action row (source `"album:<id>"`) *inside* the meta block.
//!
//! Then the album track list.

use crate::screens::{cards, pages, track_list};
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Id, Rect, Sense, Ui};

/// Content height (no main padding) for [`view`].
pub fn height(_ui: &mut Ui, state: &State, _width: f32, id: &str) -> f32 {
    let Some(page) = state.album_page.as_ref().filter(|p| p.id == id) else {
        return pages::EMPTY_H;
    };
    meta_height(&page.tracks).max(pages::ALBUM_ART)
        + pages::PAGE_HEAD_MARGIN_BOTTOM
        + track_list::list_height(page.tracks.len())
}

/// Height of the meta block inside the page head.
fn meta_height(tracks: &[rift_types::Track]) -> f32 {
    let mut h = pages::PAGE_KIND_H + pages::PAGE_TITLE_LINE + pages::PAGE_SUB_H;
    if !tracks.is_empty() {
        h += track_list::HEAD_ACTIONS_H;
    }
    h
}

/// Draw the album page at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect, id: &str) {
    // Move the page cache out of `State` for the frame: the cover request and
    // the rows need `&mut State`, so borrowing `state.album_page` directly would
    // conflict — and cloning the page (with every track) per frame would
    // allocate on the hot path. It goes back before returning.
    let page = state.album_page.take();
    let Some(album) = page.as_ref().filter(|page| page.id == id) else {
        state.album_page = page;
        pages::empty(ui, area.x, area.y, area.w, "Loading album...");
        return;
    };
    let theme = ui.theme().clone();

    // ---- page head: cover + centred meta ----
    pages::album_art(ui, state, area.x, area.y, &album.cover);
    let meta_x = area.x + pages::ALBUM_ART + pages::PAGE_HEAD_GAP;
    let meta_w = (area.w - pages::ALBUM_ART - pages::PAGE_HEAD_GAP).max(0.0);
    let kind = match album.year {
        Some(year) => format!("{} • {year}", album.album_type),
        None => album.album_type.clone(),
    };
    let meta_h = meta_height(&album.tracks);
    let head_h = meta_h.max(pages::ALBUM_ART);
    let mut y = pages::centered_meta_y(area.y, head_h, meta_h);

    y = pages::page_kind(ui, meta_x, y, meta_w, &kind);
    y = pages::page_title(ui, meta_x, y, meta_w, &album.name);

    // sub: artist link + " • N songs • duration"
    let sub_y = y + 6.0;
    let total: u32 = album.tracks.iter().filter_map(|t| t.duration).sum();
    let rest = format!(
        " • {} songs • {}",
        album.tracks.len(),
        crate::state::fmt_total_duration(total)
    );
    let artist_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, 17.0);
    let link_w;
    if let Some(artist_id) = album.artist_id.as_deref() {
        link_w = ui.text_width(&album.artist, &artist_style);
        let rect = Rect::new(meta_x, sub_y, link_w + 4.0, 17.0);
        let resp = ui.interact(rect, Id::from_str("album/artist-link"), Sense::CLICK);
        let color = if resp.hovered {
            theme.text
        } else {
            theme.text_dim
        };
        ui.text(rect, &album.artist, &artist_style.with_color(color));
        if resp.hovered {
            ui.hline(rect.x, rect.y + 15.0, link_w, 1.0, theme.text);
        }
        if resp.focus_visible {
            ui.focus_ring(rect, 4.0);
        }
        if resp.clicked {
            actions.push(Action::OpenArtist(artist_id.to_string()));
        }
    } else {
        ui.text(
            Rect::new(meta_x, sub_y, meta_w, 17.0),
            &album.artist,
            &artist_style,
        );
        link_w = ui.text_width(&album.artist, &artist_style);
    }
    ui.text(
        Rect::new(meta_x + link_w, sub_y, (meta_w - link_w).max(0.0), 17.0),
        &rest,
        &artist_style,
    );
    y += pages::PAGE_SUB_H;

    // actions row inside the meta block
    let source = format!("album:{}", album.id);
    if !album.tracks.is_empty() {
        y = track_list::head_actions(
            ui,
            state,
            actions,
            track_list::HeadActions {
                x: meta_x,
                y,
                tracks: &album.tracks,
                source: Some(source.as_str()),
                id_key: &source,
            },
        );
    }
    let _ = y;

    // ---- track list ----
    let list_y = area.y + head_h + pages::PAGE_HEAD_MARGIN_BOTTOM;
    if !album.tracks.is_empty() {
        let opts = track_list::TrackListOpts {
            source: Some(source.as_str()),
            id: "album-list",
            ..track_list::TrackListOpts::default()
        };
        track_list::track_list(
            ui,
            state,
            actions,
            Rect::new(
                area.x,
                list_y,
                area.w,
                track_list::list_height(album.tracks.len()),
            ),
            &album.tracks,
            &opts,
        );
    }
    state.album_page = page;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview;
    use crate::ui::Ui;

    #[test]
    fn loading_album_is_just_the_empty_state() {
        let mut ui = Ui::new();
        let state = State::default();
        assert_eq!(height(&mut ui, &state, 766.0, "x"), pages::EMPTY_H);
    }

    #[test]
    fn album_height_is_page_head_plus_rows() {
        let mut ui = Ui::new();
        let state = State {
            album_page: Some(preview::preview_album_page()),
            ..State::default()
        };
        let n = state.album_page.as_ref().unwrap().tracks.len();
        // meta (19+40+23+69=151) < cover 170, + 24 margin + rows.
        assert_eq!(
            height(&mut ui, &state, 766.0, "preview-album-1"),
            194.0 + n as f32 * 54.0
        );
    }

    /// The page cache is restored after drawing (it is moved out for the frame
    /// so the row painters can mutate `State` for cover requests).
    #[test]
    fn view_restores_the_album_page() {
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
        let mut state = State {
            album_page: Some(preview::preview_album_page()),
            ..State::default()
        };
        let mut actions = Vec::new();
        view(
            &mut ui,
            &mut state,
            &mut actions,
            Rect::new(359.0, 66.0, 767.0, 600.0),
            "preview-album-1",
        );
        let _ = ui.end_frame();
        assert_eq!(
            state.album_page.as_ref().map(|page| page.id.as_str()),
            Some("preview-album-1"),
            "album page restored"
        );
    }
}
