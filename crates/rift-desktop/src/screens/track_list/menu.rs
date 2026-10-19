//! The track-row context-menu entries, in the old UI's order, and the helper
//! that opens the menu under a row's kebab (or the row itself for a right-click).

use rift_types::Track;

use crate::screens::menu;
use crate::state::{Action, State};
use crate::ui::{Icon, Id, Rect, Ui};

// ---------------------------------------------------------------------------
// track context menu
// ---------------------------------------------------------------------------

/// Build the entries for a track row's context menu, in the old UI's order
/// (`ui-spec.md` §6.8): Play next · Add to queue · Add to playlist ▸ · offline
/// download action/status · — · Go to artist · Go to album · — · Remove from
/// playlist (only inside a playlist).
pub fn track_menu_entries(
    state: &State,
    track: &Track,
    playlist_id: Option<&str>,
) -> Vec<menu::Entry> {
    let mut out = vec![
        menu::Entry::item(
            menu::Item::new("Play next", Some(Icon::SkipNext)),
            Action::QueuePlayNext(track.clone()),
        ),
        menu::Entry::item(
            menu::Item::new("Add to queue", Some(Icon::Queue)),
            Action::QueueAdd(track.clone()),
        ),
    ];
    let options: Vec<(String, Action)> = state
        .playlist_options()
        .into_iter()
        .map(|(id, name)| {
            (
                name,
                Action::AddToPlaylist {
                    playlist_id: id,
                    track: track.clone(),
                },
            )
        })
        .collect();
    if !options.is_empty() {
        out.push(menu::Entry::Sub {
            icon: Some(Icon::Add),
            label: "Add to playlist".into(),
            enabled: true,
            options,
        });
    }

    // Offline download state: one status/action entry, exactly like the old row
    // menu (`check_circle` / spinner / alert variants).
    let downloaded = state.downloads.downloaded.contains(&track.id);
    let downloading = state.downloads.downloading.contains(&track.id);
    let failed = state.downloads.failed.contains(&track.id);
    let pinned = downloaded && state.pinned_ids().contains(&track.id);
    if downloaded && pinned {
        out.push(menu::Entry::inert(menu::Item::new(
            "Kept offline by a playlist",
            Some(Icon::CheckCircle),
        )));
    } else if downloaded {
        out.push(menu::Entry::item(
            menu::Item::new("Remove download", Some(Icon::CheckCircle)),
            Action::RemoveDownloads(vec![track.id.clone()]),
        ));
    } else if downloading {
        out.push(menu::Entry::inert(menu::Item::new(
            "Downloading…",
            Some(Icon::Download),
        )));
    } else if failed {
        out.push(menu::Entry::item(
            menu::Item::new("Retry download", Some(Icon::Warning)),
            Action::DownloadTracks(vec![track.id.clone()]),
        ));
    } else {
        out.push(menu::Entry::item(
            menu::Item::new("Download", Some(Icon::Download)),
            Action::DownloadTracks(vec![track.id.clone()]),
        ));
    }

    let artist_id = track.artists.iter().find_map(|a| a.id.clone());
    if artist_id.is_some() || track.album_id.is_some() {
        out.push(menu::Entry::Separator);
    }
    if let Some(artist_id) = artist_id {
        out.push(menu::Entry::item(
            menu::Item::new("Go to artist", Some(Icon::Person)),
            Action::OpenArtist(artist_id),
        ));
    }
    if let Some(album_id) = track.album_id.clone() {
        out.push(menu::Entry::item(
            menu::Item::new("Go to album", Some(Icon::Album)),
            Action::OpenAlbum(album_id),
        ));
    }
    if let Some(playlist_id) = playlist_id {
        out.push(menu::Entry::Separator);
        out.push(menu::Entry::item(
            menu::Item::new("Remove from playlist", Some(Icon::Delete)).danger(),
            Action::RemoveFromPlaylist {
                playlist_id: playlist_id.to_string(),
                track_id: track.id.clone(),
            },
        ));
    }
    out
}

/// Open the track context menu under `anchor` (a row's kebab, or the row itself
/// for a right-click). The menu itself is painted by [`menu::layer`].
pub fn open_track_menu(
    ui: &mut Ui,
    state: &State,
    anchor: Rect,
    track: &Track,
    playlist_id: Option<&str>,
) {
    let entries = track_menu_entries(state, track, playlist_id);
    let id = Id::from_str("track-menu").with(&track.id);
    menu::open(ui, id, anchor, entries);
}
