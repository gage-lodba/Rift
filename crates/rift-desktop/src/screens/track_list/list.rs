//! The vertical track list: one [`cards::track_row`] per track, with the
//! per-row action mapping and the [`TrackListOpts`] knobs.

use rift_types::Track;

use crate::screens::{cards, menu};
use crate::state::{Action, State};
use crate::ui::{Id, Rect, Ui};

use super::menu::open_track_menu;

// ---------------------------------------------------------------------------
// track list
// ---------------------------------------------------------------------------

/// Options for one [`track_list`] call.
#[derive(Clone, Copy, Debug)]
pub struct TrackListOpts<'a> {
    /// Queue tag when a row click starts the whole collection (`"liked"`,
    /// `"playlist:<id>"`, `"album:<id>"`, `"artist:<id>"`). `None` = start the
    /// list untagged (search results).
    pub source: Option<&'a str>,
    /// A row click plays just that song (`Action::PlayTrack`) instead of starting
    /// the list at that row. Used by Home's Recently played list.
    pub single_click: bool,
    /// Playlist id: rows then offer "Remove from playlist" in their menu.
    pub playlist_id: Option<&'a str>,
    pub show_album: bool,
    pub show_download: bool,
    pub show_like: bool,
    /// Id namespace (must be stable per list across frames).
    pub id: &'a str,
}

impl<'a> Default for TrackListOpts<'a> {
    fn default() -> Self {
        Self {
            source: None,
            single_click: false,
            playlist_id: None,
            show_album: true,
            show_download: true,
            show_like: true,
            id: "track-list",
        }
    }
}

/// Draw `tracks` as a vertical list of 54 px rows. Returns the height used.
///
/// Row clicks either start the collection at that index, play the single track
/// ([`TrackListOpts::single_click`]) or are inert while a menu is open (the menu
/// backdrop owns that frame's click). Nested controls (heart, kebab, album link,
/// download) never play the row.
pub fn track_list(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    rect: Rect,
    tracks: &[Track],
    opts: &TrackListOpts<'_>,
) -> f32 {
    if tracks.is_empty() {
        return 0.0;
    }
    let list_id = Id::from_str(opts.id);
    let liked = state.liked_index();
    let blocked = menu::is_open();

    for (i, track) in tracks.iter().enumerate() {
        let row_rect = Rect::new(
            rect.x,
            rect.y + i as f32 * cards::TROW_HEIGHT,
            rect.w,
            cards::TROW_HEIGHT,
        );
        let downloaded = state.downloads.downloaded.contains(&track.id);
        let unavailable = !state.online && !downloaded;
        let row_opts = cards::TrackRowOpts {
            playing: state.track.as_ref().is_some_and(|t| t.id == track.id),
            liked: liked.contains(track),
            unavailable,
            show_album: opts.show_album,
            show_download: opts.show_download,
            download: state.download_status(track),
            show_like: opts.show_like,
            reveal: false,
        };
        let resp = cards::track_row(
            ui,
            state,
            list_id.child(i as u64),
            row_rect,
            track,
            i,
            row_opts,
        );

        if blocked {
            continue;
        }
        if resp.played && !unavailable {
            let action = if opts.single_click {
                Action::PlayTrack(track.clone())
            } else {
                Action::PlayTracks {
                    tracks: tracks.to_vec(),
                    start: i,
                    source: opts.source.map(str::to_owned),
                }
            };
            actions.push(action);
        }
        if resp.like {
            actions.push(Action::ToggleLike(track.clone()));
        }
        if resp.album {
            if let Some(album_id) = track.album_id.clone() {
                actions.push(Action::OpenAlbum(album_id));
            }
        }
        if let Some(credit) = resp.artist {
            if let Some(artist_id) = track
                .artists
                .get(credit)
                .and_then(|artist| artist.id.clone())
            {
                actions.push(Action::OpenArtist(artist_id));
            }
        }
        if resp.download {
            actions.push(Action::DownloadTracks(vec![track.id.clone()]));
        }
        if resp.menu {
            open_track_menu(ui, state, resp.kebab_rect, track, opts.playlist_id);
        }
    }
    list_height(tracks.len())
}

/// The height a list of `n` tracks uses.
pub fn list_height(n: usize) -> f32 {
    n as f32 * cards::TROW_HEIGHT
}
