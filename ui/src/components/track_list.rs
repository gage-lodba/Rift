//! Track list and its per-row rendering (cover, actions, offline state).

use std::collections::HashSet;

use rift_types::{DownloadState, Track};
use yew::prelude::*;

use super::icons::{artist_links, cover, fmt_secs, icon};
use super::menu::{Menu, MenuAction};

/// Per-render context shared by every track list on screen: the id sets that
/// decorate rows and the track-level callbacks that are the same for all
/// lists. Bundled so each call site passes one value instead of re-plumbing a
/// dozen identical props.
#[derive(Clone, PartialEq)]
pub struct TrackListCtx {
    /// IDs of liked tracks.
    pub liked_ids: HashSet<String>,
    /// ID of the track currently playing, highlighted in the list.
    pub playing_id: Option<String>,
    /// Offline download status (downloaded / in flight / given up).
    pub downloads: DownloadState,
    /// IDs kept offline by a fully-downloaded playlist; their downloads can't
    /// be removed from the row, only by un-downloading the playlist.
    pub pinned_ids: HashSet<String>,
    /// Whether the network is reachable. When false, un-downloaded tracks are
    /// greyed out and can't be played.
    pub online: bool,
    /// (id, name) of user playlists, for the add-to-playlist dropdown.
    pub playlists: Vec<(String, String)>,
    pub on_like: Callback<Track>,
    pub on_queue: Callback<Track>,
    pub on_add_to_playlist: Callback<(String, Track)>,
    pub on_open_artist: Callback<String>,
    pub on_open_album: Callback<String>,
    /// Download a single track for offline listening.
    pub on_download: Callback<Track>,
    /// Remove a single track's offline copy (by id).
    pub on_remove_download: Callback<String>,
    /// Insert a single track right after the current one.
    pub on_play_next: Callback<Track>,
}

#[derive(Properties, PartialEq)]
pub struct TrackListProps {
    pub ctx: TrackListCtx,
    pub tracks: Vec<Track>,
    /// Play the track at this index (within this list).
    pub on_play: Callback<usize>,
    /// When set, rows get a remove button (used inside playlists).
    #[prop_or_default]
    pub on_remove: Option<Callback<usize>>,
}

#[function_component(TrackList)]
pub fn track_list(props: &TrackListProps) -> Html {
    let c = &props.ctx;
    html! {
        <div class="tracklist">
            { for props.tracks.iter().enumerate().map(|(i, t)| html! {
                <TrackRow
                    track={t.clone()}
                    index={i}
                    liked={c.liked_ids.contains(&t.id)}
                    downloaded={c.downloads.downloaded.contains(&t.id)}
                    downloading={c.downloads.downloading.contains(&t.id)}
                    pinned={c.pinned_ids.contains(&t.id)}
                    failed={c.downloads.failed.contains(&t.id)}
                    online={c.online}
                    playing={c.playing_id.as_deref() == Some(t.id.as_str())}
                    playlists={c.playlists.clone()}
                    on_play={props.on_play.clone()}
                    on_like={c.on_like.clone()}
                    on_queue={c.on_queue.clone()}
                    on_add_to_playlist={c.on_add_to_playlist.clone()}
                    on_open_artist={c.on_open_artist.clone()}
                    on_open_album={c.on_open_album.clone()}
                    on_remove={props.on_remove.clone()}
                    on_download={c.on_download.clone()}
                    on_remove_download={c.on_remove_download.clone()}
                    on_play_next={c.on_play_next.clone()}
                />
            }) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct TrackRowProps {
    track: Track,
    index: usize,
    liked: bool,
    downloaded: bool,
    #[prop_or(true)]
    online: bool,
    playing: bool,
    playlists: Vec<(String, String)>,
    on_play: Callback<usize>,
    on_like: Callback<Track>,
    on_queue: Callback<Track>,
    on_add_to_playlist: Callback<(String, Track)>,
    on_open_artist: Callback<String>,
    on_open_album: Callback<String>,
    on_remove: Option<Callback<usize>>,
    /// Whether a download for this track is currently in flight.
    #[prop_or_default]
    downloading: bool,
    /// Kept offline by a fully-downloaded playlist: hide the remove affordances.
    #[prop_or_default]
    pinned: bool,
    /// Download was given up on after repeated failures: show a retry affordance.
    #[prop_or_default]
    failed: bool,
    /// Download this track for offline listening.
    on_download: Callback<Track>,
    /// Remove this track's offline copy (by id).
    on_remove_download: Callback<String>,
    /// Insert this track right after the current one.
    on_play_next: Callback<Track>,
}

#[function_component(TrackRow)]
fn track_row(props: &TrackRowProps) -> Html {
    let menu_open = use_state(|| false);
    let t = &props.track;
    let i = props.index;
    // Offline, a track that isn't downloaded can't be streamed: grey it out and
    // make the row inert for playback.
    let available = props.online || props.downloaded;

    let play = {
        let cb = props.on_play.clone();
        Callback::from(move |_: MouseEvent| {
            if available {
                cb.emit(i);
            }
        })
    };
    let context = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.prevent_default();
            menu_open.set(true);
        })
    };
    let like = {
        let cb = props.on_like.clone();
        let track = t.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(track.clone());
        })
    };
    let toggle_menu = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            menu_open.set(!*menu_open);
        })
    };
    let close_menu = {
        let menu_open = menu_open.clone();
        Callback::from(move |_| menu_open.set(false))
    };
    let open_album = t.album_id.clone().map(|id| {
        let cb = props.on_open_album.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(id.clone());
        })
    });

    // Build the kebab / right-click menu.
    let mut actions: Vec<MenuAction> = vec![
        MenuAction::Item {
            icon: "next",
            label: "Play next".into(),
            danger: false,
            cb: {
                let cb = props.on_play_next.clone();
                let track = t.clone();
                Callback::from(move |_| cb.emit(track.clone()))
            },
        },
        MenuAction::Item {
            icon: "queue",
            label: "Add to queue".into(),
            danger: false,
            cb: {
                let cb = props.on_queue.clone();
                let track = t.clone();
                Callback::from(move |_| cb.emit(track.clone()))
            },
        },
    ];
    if !props.playlists.is_empty() {
        actions.push(MenuAction::Sub {
            icon: "plus",
            label: "Add to playlist".into(),
            options: props.playlists.clone(),
            cb: {
                let cb = props.on_add_to_playlist.clone();
                let track = t.clone();
                Callback::from(move |id: String| cb.emit((id, track.clone())))
            },
        });
    }
    // Per-song offline download: download, or remove the offline copy. A copy
    // pinned by a fully-downloaded playlist can't be removed individually, so
    // show a non-actionable status instead.
    if props.downloaded && props.pinned {
        actions.push(MenuAction::Item {
            icon: "check-circle",
            label: "Kept offline by a playlist".into(),
            danger: false,
            cb: Callback::noop(),
        });
    } else if props.downloaded {
        actions.push(MenuAction::Item {
            icon: "check-circle",
            label: "Remove download".into(),
            danger: false,
            cb: {
                let cb = props.on_remove_download.clone();
                let id = t.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        });
    } else if props.downloading {
        // In flight: a non-actionable status entry.
        actions.push(MenuAction::Item {
            icon: "download",
            label: "Downloading…".into(),
            danger: false,
            cb: Callback::noop(),
        });
    } else {
        actions.push(MenuAction::Item {
            icon: if props.failed { "alert" } else { "download" },
            label: if props.failed {
                "Retry download".into()
            } else {
                "Download".into()
            },
            danger: false,
            cb: {
                let cb = props.on_download.clone();
                let track = t.clone();
                Callback::from(move |_| cb.emit(track.clone()))
            },
        });
    }

    let artist_id = t.artists.iter().find_map(|a| a.id.clone());
    if artist_id.is_some() || t.album_id.is_some() {
        actions.push(MenuAction::Separator);
    }
    if let Some(aid) = artist_id {
        actions.push(MenuAction::Item {
            icon: "person",
            label: "Go to artist".into(),
            danger: false,
            cb: {
                let cb = props.on_open_artist.clone();
                Callback::from(move |_| cb.emit(aid.clone()))
            },
        });
    }
    if let Some(alid) = t.album_id.clone() {
        actions.push(MenuAction::Item {
            icon: "album",
            label: "Go to album".into(),
            danger: false,
            cb: {
                let cb = props.on_open_album.clone();
                Callback::from(move |_| cb.emit(alid.clone()))
            },
        });
    }
    if let Some(remove) = props.on_remove.clone() {
        actions.push(MenuAction::Separator);
        actions.push(MenuAction::Item {
            icon: "trash",
            label: "Remove from playlist".into(),
            danger: true,
            cb: Callback::from(move |_| remove.emit(i)),
        });
    }

    // Inline download / remove-download from the row itself (the kebab menu
    // entries stay for right-click users).
    let download = {
        let cb = props.on_download.clone();
        let track = t.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(track.clone());
        })
    };
    let remove_download = {
        let cb = props.on_remove_download.clone();
        let id = t.id.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(id.clone());
        })
    };

    html! {
        <div class={classes!("trow", props.playing.then_some("playing"), (*menu_open).then_some("menu-open"), (!available).then_some("unavailable"))}
             title={(!available).then_some("Unavailable offline — not downloaded")}
             onclick={play} oncontextmenu={context}>
            { cover(&t.cover, "trow-cover") }
            <div class="trow-meta">
                <div class="trow-title">{ &t.title }</div>
                <div class="trow-artist">
                    { artist_links(&t.artists, &t.artist, &props.on_open_artist) }
                </div>
            </div>
            <div class="trow-album">
                if let (Some(album), Some(open_album)) = (t.album.clone(), open_album) {
                    <span class="link" onclick={open_album}>{ album }</span>
                } else {
                    { t.album.clone().unwrap_or_default() }
                }
            </div>
            <div class="trow-spacer"></div>
            // Always rendered so the slot's width is constant and the album
            // column doesn't shift between downloaded and undownloaded rows.
            <span class="trow-dl">
                if props.downloaded && props.pinned {
                    // A downloaded playlist pins this copy: indicator only.
                    <span title="Kept offline by a downloaded playlist">
                        { icon("check-circle") }
                    </span>
                } else if props.downloaded {
                    <button class="ibtn accent" title="Available offline — click to remove download"
                            onclick={remove_download}>
                        { icon("check-circle") }
                    </button>
                } else if props.downloading {
                    <span class="spinner" title="Downloading…"></span>
                } else if props.failed {
                    <button class="ibtn error" title="Download failed — click to retry"
                            onclick={download}>
                        { icon("alert") }
                    </button>
                } else if props.online {
                    <button class="ibtn" title="Download" onclick={download}>
                        { icon("download") }
                    </button>
                }
            </span>
            <div class="trow-dur">{ t.duration.map(fmt_secs).unwrap_or_default() }</div>
            <div class="trow-actions" onclick={|e: MouseEvent| e.stop_propagation()}>
                <button class={classes!("ibtn", props.liked.then_some("liked"))}
                        title={ if props.liked { "Unlike" } else { "Like" } }
                        onclick={like}>{ icon(if props.liked { "heart" } else { "heart-outline" }) }</button>
                <div class="menu-anchor">
                    <button class="ibtn" title="More" onclick={toggle_menu}>{ icon("kebab") }</button>
                    <Menu open={*menu_open} on_close={close_menu} actions={actions} align_right=true />
                </div>
            </div>
        </div>
    }
}
