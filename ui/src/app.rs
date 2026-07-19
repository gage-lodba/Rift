//! Root component: owns all state, subscribes to backend events and routes
//! between views.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use rift_types::{
    events, AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Playlist, Progress, QueueSnapshot, Track, UpdateStatus,
};
use serde_json::json;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, KeyboardEvent};
use yew::prelude::*;

use crate::api;
use crate::components::*;

#[derive(Clone, PartialEq, Default)]
struct Search {
    query: String,
    results: Vec<Track>,
    busy: bool,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum SearchTab {
    #[default]
    Songs,
    Artists,
    Albums,
}

/// Human-readable total duration for a collection subtitle, e.g. "1 hr 23 min".
fn fmt_total_duration(secs: u32) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h} hr {m} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{secs} sec")
    }
}

/// Subtitle line for a collection: song count and total duration.
fn collection_meta(tracks: &[Track]) -> Html {
    let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
    let n = tracks.len();
    html! {
        <div class="collection-meta">
            { format!("{n} song{} • {}", if n == 1 { "" } else { "s" }, fmt_total_duration(total)) }
        </div>
    }
}

/// Every track of `p` is downloaded (and it has any): the playlist pins its
/// tracks' offline copies, which then can't be removed individually. Shared by
/// the pinned-row computation and the remove-download guard (its mirror).
fn playlist_fully_downloaded(p: &Playlist, downloaded: &HashSet<String>) -> bool {
    !p.tracks.is_empty() && p.tracks.iter().all(|t| downloaded.contains(&t.id))
}

/// Fire the `play_tracks` command for a collection, tagged with where it came
/// from so the UI can mark the playing collection.
fn fire_play_tracks(tracks: &[Track], source: &Option<String>, start: usize) {
    api::fire(
        "play_tracks",
        json!({ "tracks": tracks, "start": start, "source": source }),
    );
}

/// Show a toast that auto-dismisses after `ms`, unless a newer toast supersedes
/// it first. `gen` is a shared counter bumped on every toast so overlapping
/// toasts don't clear each other's messages early.
fn show_toast(toast: UseStateHandle<Option<String>>, gen: Rc<RefCell<u64>>, msg: String, ms: u32) {
    let token = {
        let mut g = gen.borrow_mut();
        *g += 1;
        *g
    };
    toast.set(Some(msg));
    spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(ms).await;
        if *gen.borrow() == token {
            toast.set(None);
        }
    });
}

/// Show a toast that stays until replaced (no auto-dismiss). Still bumps `gen`
/// so a pending dismiss from an earlier toast can't clear it.
fn sticky_toast(toast: &UseStateHandle<Option<String>>, gen: &Rc<RefCell<u64>>, msg: String) {
    *gen.borrow_mut() += 1;
    toast.set(Some(msg));
}

#[function_component(App)]
pub fn app() -> Html {
    let library = use_state(Library::default);
    let queue = use_state(QueueSnapshot::default);
    let pstate = use_state(PlaybackState::default);
    let track = use_state(|| None::<Track>);
    let progress = use_state(Progress::default);
    let volume = use_state(|| 1.0f32);
    let discord_rpc = use_state(|| true);
    let crossfade = use_state(|| 0.0f32);
    let yt_dlp_path = use_state(|| None::<String>);
    let update_notifications = use_state(|| true);
    // Set when a launch-time update check finds a newer release; drives the
    // dismissible update banner.
    let update_banner = use_state(|| None::<UpdateStatus>);
    // Cached result of the GitHub release check, shared by the launch check and
    // the Settings view so repeated Settings visits don't re-hit the API.
    // `None` = not checked yet / a check is in flight.
    let update_status = use_state(|| None::<UpdateStatus>);
    let view = use_state(|| View::Home);
    let search = use_state(Search::default);
    let search_tab = use_state(SearchTab::default);
    let artist_results = use_state(|| None::<Vec<ArtistSummary>>);
    let album_results = use_state(|| None::<Vec<AlbumSummary>>);
    let artist_page = use_state(|| None::<ArtistPage>);
    // Full song catalog for the "Show all songs" view, loaded on demand and
    // cached by artist id (it's the heaviest fetch in the app) so re-opening the
    // same artist is instant. `None` means nothing loaded yet.
    let artist_songs = use_state(|| None::<(String, Vec<Track>)>);
    let album_page = use_state(|| None::<AlbumPage>);
    let downloads = use_state(DownloadState::default);
    // Preview mode (dev builds launched with RIFT_PREVIEW=1): render neutral
    // placeholder data instead of the personal library, for repo screenshots.
    // The flag arrives via bootstrap; release builds never set it.
    let preview = use_state(|| false);
    // Network reachability (from the webview); offline greys out un-downloaded
    // tracks since they can't be streamed.
    let online = use_state(|| {
        web_sys::window()
            .map(|w| w.navigator().on_line())
            .unwrap_or(true)
    });
    // (playlist id, draft name) while a playlist is being renamed.
    let renaming = use_state(|| None::<(String, String)>);
    let toast = use_state(|| None::<String>);
    // Bumped on every toast so overlapping toasts don't clear each other early.
    let toast_gen = use_mut_ref(|| 0u64);
    // Bumped on every song search so a slow, stale response can't overwrite the
    // results of a newer one (the invokes race; last *arrival* would win).
    let search_gen = use_mut_ref(|| 0u64);

    // Subscribe to backend events and load the initial snapshot, once.
    {
        let library = library.clone();
        let queue = queue.clone();
        let pstate = pstate.clone();
        let track = track.clone();
        let progress = progress.clone();
        let volume = volume.clone();
        let discord_rpc = discord_rpc.clone();
        let crossfade = crossfade.clone();
        let yt_dlp_path = yt_dlp_path.clone();
        let update_notifications = update_notifications.clone();
        let update_banner = update_banner.clone();
        let update_status = update_status.clone();
        let downloads = downloads.clone();
        let preview = preview.clone();
        let toast = toast.clone();
        let toast_gen = toast_gen.clone();
        let view = view.clone();
        use_effect_with((), move |_| {
            {
                let library = library.clone();
                api::listen_event::<Library>(
                    events::LIBRARY,
                    Callback::from(move |v| library.set(v)),
                );
            }
            {
                let queue = queue.clone();
                api::listen_event::<QueueSnapshot>(
                    events::QUEUE,
                    Callback::from(move |v| queue.set(v)),
                );
            }
            {
                let pstate = pstate.clone();
                api::listen_event::<PlaybackState>(
                    events::STATE,
                    Callback::from(move |v| pstate.set(v)),
                );
            }
            {
                let track = track.clone();
                api::listen_event::<Track>(
                    events::TRACK,
                    Callback::from(move |v| track.set(Some(v))),
                );
            }
            // NB: the PROGRESS event is intentionally *not* subscribed here.
            // PlayerBar owns live progress and subscribes itself, so ~4 Hz ticks
            // re-render only the bar, not the whole app. `progress` below is
            // seeded once from bootstrap and handed to PlayerBar as its seed.
            {
                let downloads = downloads.clone();
                api::listen_event::<DownloadState>(
                    events::DOWNLOADS,
                    Callback::from(move |v| downloads.set(v)),
                );
            }
            {
                let toast = toast.clone();
                let gen = toast_gen.clone();
                api::listen_event::<String>(
                    events::ERROR,
                    Callback::from(move |msg: String| {
                        show_toast(toast.clone(), gen.clone(), msg, 6000);
                    }),
                );
            }
            {
                // Informational toasts (e.g. export/import results).
                let toast = toast.clone();
                let gen = toast_gen.clone();
                api::listen_event::<String>(
                    events::NOTICE,
                    Callback::from(move |msg: String| {
                        show_toast(toast.clone(), gen.clone(), msg, 5000);
                    }),
                );
            }
            {
                // Backend-driven navigation (e.g. after a file import).
                let view = view.clone();
                api::listen_event::<String>(
                    events::OPEN_PLAYLIST,
                    Callback::from(move |id: String| view.set(View::Playlist(id))),
                );
            }
            spawn_local(async move {
                match api::invoke::<Bootstrap>("bootstrap", &json!({})).await {
                    Ok(b) => {
                        library.set(b.library);
                        queue.set(b.queue);
                        pstate.set(b.state);
                        track.set(b.track);
                        progress.set(b.progress);
                        volume.set(b.volume);
                        discord_rpc.set(b.discord_rpc);
                        crossfade.set(b.crossfade);
                        yt_dlp_path.set(b.yt_dlp_path);
                        update_notifications.set(b.update_notifications);
                        downloads.set(b.downloads);
                        preview.set(b.preview);

                        // Launch-time update check (only if the user hasn't
                        // silenced it). Surfaces a dismissible banner if a newer
                        // release exists, and seeds the cache Settings reads.
                        if b.update_notifications {
                            let update_banner = update_banner.clone();
                            let update_status = update_status.clone();
                            spawn_local(async move {
                                if let Ok(status) =
                                    api::invoke::<UpdateStatus>("check_update", &json!({})).await
                                {
                                    if status.update_available {
                                        update_banner.set(Some(status.clone()));
                                    }
                                    update_status.set(Some(status));
                                }
                            });
                        }
                    }
                    Err(e) => web_sys::console::error_1(&format!("bootstrap: {e}").into()),
                }
            });
            || {}
        });
    }

    // Global keyboard shortcuts: Space = play/pause, Ctrl/Cmd+Arrows = prev/next.
    // Ignored while typing in a text field so search/rename keep working.
    use_effect_with((), move |_| {
        let document = web_sys::window().unwrap().document().unwrap();
        let handler = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let typing = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.active_element())
                .map(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA"))
                .unwrap_or(false);
            if typing {
                return;
            }
            let cmd = e.ctrl_key() || e.meta_key();
            match e.key().as_str() {
                " " => {
                    e.prevent_default();
                    api::fire("toggle_play", json!({}));
                }
                "ArrowRight" if cmd => {
                    e.prevent_default();
                    api::fire("next_track", json!({}));
                }
                "ArrowLeft" if cmd => {
                    e.prevent_default();
                    api::fire("prev_track", json!({}));
                }
                _ => {}
            }
        });
        document
            .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
            .ok();
        // Lives for the lifetime of the app, like the event subscriptions.
        handler.forget();
        || {}
    });

    // Track network reachability via the webview's online/offline events.
    {
        let online = online.clone();
        use_effect_with((), move |_| {
            let window = web_sys::window().unwrap();
            let make = |value: bool, online: UseStateHandle<bool>| {
                Closure::<dyn FnMut()>::new(move || online.set(value))
            };
            let on = make(true, online.clone());
            let off = make(false, online.clone());
            let _ = window.add_event_listener_with_callback("online", on.as_ref().unchecked_ref());
            let _ =
                window.add_event_listener_with_callback("offline", off.as_ref().unchecked_ref());
            on.forget();
            off.forget();
            || {}
        });
    }

    // Load artist/album pages whenever the view navigates to one.
    {
        let artist_page = artist_page.clone();
        let artist_songs = artist_songs.clone();
        let album_page = album_page.clone();
        let toast = toast.clone();
        let toast_gen = toast_gen.clone();
        use_effect_with((*view).clone(), move |v| {
            match v {
                View::Artist(id) => {
                    artist_page.set(None);
                    let id = id.clone();
                    spawn_local(async move {
                        match api::invoke::<ArtistPage>("get_artist", &json!({ "id": id })).await {
                            Ok(p) => artist_page.set(Some(p)),
                            Err(e) => show_toast(toast, toast_gen, e, 6000),
                        }
                    });
                }
                View::ArtistSongs(id) => {
                    // Cache hit for this artist: keep the loaded list (instant).
                    let cached = matches!(&*artist_songs, Some((cached_id, _)) if cached_id == id);
                    if !cached {
                        artist_songs.set(None);
                        let id = id.clone();
                        let artist_songs = artist_songs.clone();
                        let toast = toast.clone();
                        let toast_gen = toast_gen.clone();
                        spawn_local(async move {
                            match api::invoke::<Vec<Track>>(
                                "get_artist_songs",
                                &json!({ "id": id.clone() }),
                            )
                            .await
                            {
                                Ok(t) => artist_songs.set(Some((id, t))),
                                Err(e) => show_toast(toast, toast_gen, e, 6000),
                            }
                        });
                    }
                }
                View::Album(id) => {
                    album_page.set(None);
                    let id = id.clone();
                    spawn_local(async move {
                        match api::invoke::<AlbumPage>("get_album", &json!({ "id": id })).await {
                            Ok(p) => album_page.set(Some(p)),
                            Err(e) => show_toast(toast, toast_gen, e, 6000),
                        }
                    });
                }
                _ => {}
            }
            || {}
        });
    }

    // ------------------------------------------------------------ actions

    let on_search = {
        let search = search.clone();
        let view = view.clone();
        let search_tab = search_tab.clone();
        let artist_results = artist_results.clone();
        let album_results = album_results.clone();
        let search_gen = search_gen.clone();
        Callback::from(move |query: String| {
            view.set(View::Search);
            search_tab.set(SearchTab::Songs);
            artist_results.set(None);
            album_results.set(None);
            search.set(Search {
                query: query.clone(),
                results: vec![],
                busy: true,
            });
            let token = {
                let mut g = search_gen.borrow_mut();
                *g += 1;
                *g
            };
            let search = search.clone();
            let search_gen = search_gen.clone();
            spawn_local(async move {
                let result =
                    api::invoke::<Vec<Track>>("search", &json!({ "query": query.clone() })).await;
                if *search_gen.borrow() != token {
                    return; // superseded by a newer search
                }
                match result {
                    Ok(results) => search.set(Search {
                        query,
                        results,
                        busy: false,
                    }),
                    Err(e) => {
                        web_sys::console::error_1(&format!("search: {e}").into());
                        search.set(Search {
                            query,
                            results: vec![],
                            busy: false,
                        });
                    }
                }
            });
        })
    };

    let on_nav = {
        let view = view.clone();
        Callback::from(move |v: View| view.set(v))
    };
    let on_open_artist = {
        let view = view.clone();
        Callback::from(move |id: String| view.set(View::Artist(id)))
    };
    let on_open_album = {
        let view = view.clone();
        Callback::from(move |id: String| view.set(View::Album(id)))
    };
    let on_open_artist_songs = {
        let view = view.clone();
        Callback::from(move |id: String| view.set(View::ArtistSongs(id)))
    };

    // Lazily fetch artist/album results when their tab is first opened.
    let on_tab = {
        let search_tab = search_tab.clone();
        let artist_results = artist_results.clone();
        let album_results = album_results.clone();
        let query = search.query.clone();
        let toast = toast.clone();
        let toast_gen = toast_gen.clone();
        Callback::from(move |t: SearchTab| {
            search_tab.set(t);
            match t {
                SearchTab::Artists if artist_results.is_none() => {
                    let results = artist_results.clone();
                    let query = query.clone();
                    let toast = toast.clone();
                    let toast_gen = toast_gen.clone();
                    spawn_local(async move {
                        match api::invoke::<Vec<ArtistSummary>>(
                            "search_artists",
                            &json!({ "query": query }),
                        )
                        .await
                        {
                            Ok(v) => results.set(Some(v)),
                            Err(e) => {
                                show_toast(toast, toast_gen, e, 6000);
                                results.set(Some(vec![]));
                            }
                        }
                    });
                }
                SearchTab::Albums if album_results.is_none() => {
                    let results = album_results.clone();
                    let query = query.clone();
                    let toast = toast.clone();
                    let toast_gen = toast_gen.clone();
                    spawn_local(async move {
                        match api::invoke::<Vec<AlbumSummary>>(
                            "search_albums",
                            &json!({ "query": query }),
                        )
                        .await
                        {
                            Ok(v) => results.set(Some(v)),
                            Err(e) => {
                                show_toast(toast, toast_gen, e, 6000);
                                results.set(Some(vec![]));
                            }
                        }
                    });
                }
                _ => {}
            }
        })
    };

    // Play one track by itself (no radio fill).
    let on_play_single = Callback::from(|t: Track| {
        api::fire("play_track", json!({ "track": t, "radio": false }));
    });

    // Play a full list starting at an index, tagged with where it came from.
    let play_list = |tracks: Vec<Track>, source: Option<String>| {
        Callback::from(move |start: usize| fire_play_tracks(&tracks, &source, start))
    };

    let on_update_notifications = {
        let update_notifications = update_notifications.clone();
        Callback::from(move |enabled: bool| {
            update_notifications.set(enabled);
            api::fire("set_update_notifications", json!({ "enabled": enabled }));
        })
    };

    // Re-run the GitHub release check (Settings' "Check now", or its mount when
    // no launch check has populated the cache yet).
    let on_check_update = {
        let update_status = update_status.clone();
        Callback::from(move |_: ()| {
            let update_status = update_status.clone();
            update_status.set(None);
            spawn_local(async move {
                let status = api::invoke::<UpdateStatus>("check_update", &json!({}))
                    .await
                    .unwrap_or_default();
                update_status.set(Some(status));
            });
        })
    };

    let on_like = Callback::from(|t: Track| api::fire("toggle_like", json!({ "track": t })));
    let on_download =
        Callback::from(|t: Track| api::fire("download_tracks", json!({ "tracks": [t] })));
    let on_remove_download = {
        let library = library.clone();
        let downloads = downloads.clone();
        let toast = toast.clone();
        let toast_gen = toast_gen.clone();
        Callback::from(move |id: String| {
            // Playlists kept fully offline (every track downloaded) that contain
            // this song: removing its download would break their offline copy, so
            // block it and tell the user to remove it from the playlist first.
            let blocking: Vec<String> = library
                .playlists
                .iter()
                .filter(|p| {
                    p.tracks.iter().any(|t| t.id == id)
                        && playlist_fully_downloaded(p, &downloads.downloaded)
                })
                .map(|p| format!("\u{201c}{}\u{201d}", p.name))
                .collect();
            if blocking.is_empty() {
                api::fire("remove_downloads", json!({ "ids": [id] }));
            } else {
                let noun = if blocking.len() == 1 {
                    "that playlist"
                } else {
                    "those playlists"
                };
                show_toast(
                    toast.clone(),
                    toast_gen.clone(),
                    format!(
                        "This song is kept offline in {}. Remove it from {noun} before deleting the download.",
                        blocking.join(", ")
                    ),
                    6000,
                );
            }
        })
    };
    let on_queue_add = Callback::from(|t: Track| api::fire("queue_add", json!({ "track": t })));
    let on_play_next =
        Callback::from(|t: Track| api::fire("queue_play_next", json!({ "tracks": [t] })));
    let on_add_to_playlist = Callback::from(|(id, t): (String, Track)| {
        api::fire("add_to_playlist", json!({ "id": id, "track": t }));
    });
    let on_new_playlist =
        Callback::from(|name: String| api::fire("create_playlist", json!({ "name": name })));
    let on_import_playlist = {
        let toast = toast.clone();
        let toast_gen = toast_gen.clone();
        let view = view.clone();
        Callback::from(move |url: String| {
            let toast = toast.clone();
            let toast_gen = toast_gen.clone();
            let view = view.clone();
            spawn_local(async move {
                // Persist "Importing…" until the request resolves.
                sticky_toast(&toast, &toast_gen, "Importing playlist…".to_string());
                let msg =
                    match api::invoke::<Playlist>("import_yt_playlist", &json!({ "url": url }))
                        .await
                    {
                        Ok(p) => {
                            let n = p.tracks.len();
                            view.set(View::Playlist(p.id));
                            format!(
                                "Imported \u{201c}{}\u{201d} ({n} song{})",
                                p.name,
                                if n == 1 { "" } else { "s" }
                            )
                        }
                        Err(e) => e,
                    };
                show_toast(toast, toast_gen, msg, 5000);
            });
        })
    };
    let on_rename_playlist = {
        let renaming = renaming.clone();
        let view = view.clone();
        let library = library.clone();
        Callback::from(move |id: String| {
            if let Some(p) = library.playlists.iter().find(|p| p.id == id) {
                renaming.set(Some((id.clone(), p.name.clone())));
                view.set(View::Playlist(id));
            }
        })
    };
    let on_delete_playlist = {
        let view = view.clone();
        Callback::from(move |id: String| {
            api::fire("delete_playlist", json!({ "id": id.clone() }));
            if *view == View::Playlist(id) {
                view.set(View::Home);
            }
        })
    };
    let on_export_playlist =
        Callback::from(|id: String| api::fire("export_playlist", json!({ "id": id })));
    let on_import_file = Callback::from(|()| api::fire("import_playlist", json!({})));

    let on_queue_jump = Callback::from(|i: usize| api::fire("queue_jump", json!({ "index": i })));
    let on_queue_remove =
        Callback::from(|i: usize| api::fire("queue_remove", json!({ "index": i })));
    let on_queue_move = Callback::from(|(from, to): (usize, usize)| {
        api::fire("queue_move", json!({ "from": from, "to": to }));
    });
    // Sidebar reorder: resolve the dragged index to a playlist id at emit time
    // (the backend moves by id, so a stale index can't move the wrong list).
    let on_move_playlist = {
        let library = library.clone();
        Callback::from(move |(from, to): (usize, usize)| {
            if let Some(p) = library.playlists.get(from) {
                api::fire("move_playlist", json!({ "id": p.id, "to": to }));
            }
        })
    };
    let on_queue_clear = Callback::from(|()| api::fire("queue_clear", json!({})));
    let on_volume = {
        let volume = volume.clone();
        Callback::from(move |v: f32| {
            volume.set(v);
            api::fire("set_volume", json!({ "volume": v }));
        })
    };

    // -------------------------------------------------------------- views

    // Preview mode (dev builds only) swaps everything rendered below for
    // neutral placeholders; the action callbacks above still hold the real
    // state handles, so the app keeps running underneath. Release builds
    // compile the placeholder path out entirely.
    //
    // `library` is borrowed, not cloned: App re-renders on every progress tick
    // (~4x/s while playing) and a deep Library clone each time is pure churn.
    // (track/queue were already cloned per render; progress/pstate are Copy.)
    #[cfg(debug_assertions)]
    let preview_lib = (*preview).then(preview_library);
    #[cfg(not(debug_assertions))]
    let preview_lib: Option<Library> = None;
    let library: &Library = preview_lib.as_ref().unwrap_or(&library);

    #[cfg(debug_assertions)]
    let (track, queue, progress, pstate) = if *preview {
        (
            Some(preview_track(1)),
            preview_queue(),
            Progress {
                position: 83.0,
                duration: 214.0,
            },
            PlaybackState::Playing,
        )
    } else {
        ((*track).clone(), (*queue).clone(), *progress, *pstate)
    };
    #[cfg(not(debug_assertions))]
    let (track, queue, progress, pstate) = ((*track).clone(), (*queue).clone(), *progress, *pstate);

    let liked_ids: HashSet<String> = library.liked.iter().map(|t| t.id.clone()).collect();
    let playlist_opts: Vec<(String, String)> = library
        .playlists
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    let sidebar_playlists: Vec<(String, String, usize)> = library
        .playlists
        .iter()
        .map(|p| (p.id.clone(), p.name.clone(), p.tracks.len()))
        .collect();
    // Tracks whose offline copy is pinned by a fully-downloaded playlist.
    // on_remove_download refuses these, so rows show a plain indicator instead
    // of a remove button (the mirror of that guard).
    let pinned_ids: HashSet<String> = library
        .playlists
        .iter()
        .filter(|p| playlist_fully_downloaded(p, &downloads.downloaded))
        .flat_map(|p| p.tracks.iter().map(|t| t.id.clone()))
        .collect();

    // Everything a TrackList needs that doesn't vary per list.
    let ctx = TrackListCtx {
        liked_ids: liked_ids.clone(),
        playing_id: track.as_ref().map(|t| t.id.clone()),
        downloads: (*downloads).clone(),
        pinned_ids,
        online: *online,
        playlists: playlist_opts,
        on_like: on_like.clone(),
        on_queue: on_queue_add.clone(),
        on_add_to_playlist: on_add_to_playlist.clone(),
        on_open_artist: on_open_artist.clone(),
        on_open_album: on_open_album.clone(),
        on_download: on_download.clone(),
        on_remove_download: on_remove_download.clone(),
        on_play_next: on_play_next.clone(),
    };

    let list = {
        let ctx = ctx.clone();
        move |tracks: Vec<Track>, source: Option<String>, removable: Option<Callback<usize>>| {
            html! {
                <TrackList ctx={ctx.clone()} tracks={tracks.clone()}
                           on_play={play_list(tracks, source)} on_remove={removable} />
            }
        }
    };

    // Play + Download buttons shown atop a collection (Liked / playlist / album).
    let actions = |tracks: Vec<Track>, source: Option<String>| -> Html {
        if tracks.is_empty() {
            return html! {};
        }
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let total = ids.len();
        let n_done = ids
            .iter()
            .filter(|id| downloads.downloaded.contains(*id))
            .count();
        let n_active = ids
            .iter()
            .filter(|id| downloads.downloading.contains(*id))
            .count();

        // Whether this collection is the one currently loaded in the player, so
        // the Play button can reflect (and toggle) its playback state instead of
        // always offering to start it over.
        let is_current = source.is_some()
            && source == queue.source
            && matches!(
                pstate,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading
            );
        let show_pause =
            is_current && matches!(pstate, PlaybackState::Playing | PlaybackState::Loading);

        let play = {
            let tracks = tracks.clone();
            let source = source.clone();
            // With shuffle on, start the collection on a random track instead of
            // always the first. Clicking an individual row still plays that row.
            let shuffle = queue.shuffle;
            Callback::from(move |_: MouseEvent| {
                if is_current {
                    // Already this collection — just toggle play/pause.
                    api::fire("toggle_play", json!({}));
                    return;
                }
                let start = if shuffle && tracks.len() > 1 {
                    (js_sys::Math::random() * tracks.len() as f64) as usize
                } else {
                    0
                };
                fire_play_tracks(&tracks, &source, start);
            })
        };

        let download = if n_active > 0 {
            html! {
                <button class="head-btn" disabled=true>
                    <span class="spinner"></span>
                    // 1-indexed ordinal of the item in flight, so the counter
                    // reads "1/10" while the first track downloads rather than
                    // sitting at "0/10" until it lands.
                    { format!("Downloading {}/{}", (n_done + 1).min(total), total) }
                </button>
            }
        } else if n_done == total {
            let remove = {
                let ids = ids.clone();
                Callback::from(move |_: MouseEvent| {
                    api::fire("remove_downloads", json!({ "ids": ids.clone() }));
                })
            };
            html! {
                <button class="head-btn downloaded" title="Remove downloads" onclick={remove}>
                    { icon("check-circle") }<span>{ "Downloaded" }</span>
                </button>
            }
        } else {
            let download = {
                let tracks = tracks.clone();
                Callback::from(move |_: MouseEvent| {
                    api::fire("download_tracks", json!({ "tracks": tracks.clone() }));
                })
            };
            html! {
                <button class="head-btn" onclick={download}>
                    { icon("download") }<span>{ "Download" }</span>
                </button>
            }
        };

        html! {
            <div class="head-actions">
                <button class="play-all" onclick={play}>
                    { icon(if show_pause { "pause" } else { "play" }) }
                    <span>{ if show_pause { "Pause" } else { "Play" } }</span>
                </button>
                { download }
            </div>
        }
    };

    let main = match &*view {
        View::Home => {
            let recent: Vec<Track> = library.recently_played.iter().take(12).cloned().collect();
            // Clicking a row plays just that song (with radio fill), like the
            // old cards did — not the whole recently-played list.
            let play_recent = {
                let tracks = recent.clone();
                let on_play_single = on_play_single.clone();
                Callback::from(move |i: usize| {
                    if let Some(t) = tracks.get(i) {
                        on_play_single.emit(t.clone());
                    }
                })
            };
            let recent_list = html! {
                <TrackList ctx={ctx.clone()} tracks={recent} on_play={play_recent} />
            };
            html! {
                <HomeView library={(*library).clone()} on_nav={on_nav.clone()}
                          on_rename_playlist={on_rename_playlist.clone()}
                          on_delete_playlist={on_delete_playlist.clone()}
                          recent={recent_list} />
            }
        }
        View::Search => {
            let tab_btn = |t: SearchTab, label: &str| {
                let on_tab = on_tab.clone();
                let active = *search_tab == t;
                html! {
                    <button class={classes!("tab", active.then_some("active"))}
                            onclick={Callback::from(move |_| on_tab.emit(t))}>
                        { label }
                    </button>
                }
            };
            let content = match *search_tab {
                SearchTab::Songs => {
                    if search.busy {
                        html! { <div class="empty">{ "Searching..." }</div> }
                    } else if search.results.is_empty() {
                        html! { <div class="empty">{ "No results." }</div> }
                    } else {
                        list(search.results.clone(), None, None)
                    }
                }
                SearchTab::Artists => {
                    results_view(&artist_results, |v| artist_grid(v, on_open_artist.clone()))
                }
                SearchTab::Albums => {
                    results_view(&album_results, |v| album_grid(v, on_open_album.clone()))
                }
            };
            html! {
                <>
                    <h2>{ format!("Results for \u{201c}{}\u{201d}", search.query) }</h2>
                    <div class="tabs">
                        { tab_btn(SearchTab::Songs, "Songs") }
                        { tab_btn(SearchTab::Artists, "Artists") }
                        { tab_btn(SearchTab::Albums, "Albums") }
                    </div>
                    { content }
                </>
            }
        }
        View::Liked => html! {
            <>
                <div class="view-head"><h2>{ "Liked Songs" }</h2></div>
                if library.liked.is_empty() {
                    <div class="empty">{ "No liked songs yet. Click the heart on any track." }</div>
                } else {
                    { collection_meta(&library.liked) }
                    { actions(library.liked.clone(), Some("liked".to_string())) }
                    { list(library.liked.clone(), Some("liked".to_string()), None) }
                }
            </>
        },
        View::Settings => {
            let on_discord_rpc = {
                let discord_rpc = discord_rpc.clone();
                Callback::from(move |enabled: bool| {
                    discord_rpc.set(enabled);
                    api::fire("set_discord_rpc", json!({ "enabled": enabled }));
                })
            };
            let on_crossfade = {
                let crossfade = crossfade.clone();
                Callback::from(move |secs: f32| {
                    crossfade.set(secs);
                    api::fire("set_crossfade", json!({ "secs": secs }));
                })
            };
            html! {
                <SettingsView
                    discord_rpc={*discord_rpc}
                    on_discord_rpc={on_discord_rpc}
                    crossfade={*crossfade}
                    on_crossfade={on_crossfade}
                    yt_dlp_path={(*yt_dlp_path).clone()}
                    update_notifications={*update_notifications}
                    on_update_notifications={on_update_notifications.clone()}
                    update={(*update_status).clone()}
                    on_check_update={on_check_update.clone()}
                />
            }
        }
        View::Playlist(id) => match library.playlists.iter().find(|p| &p.id == id) {
            None => html! { <div class="empty">{ "Playlist not found." }</div> },
            Some(p) => {
                let remove = {
                    let id = p.id.clone();
                    let tracks = p.tracks.clone();
                    Callback::from(move |i: usize| {
                        if let Some(t) = tracks.get(i) {
                            api::fire(
                                "remove_from_playlist",
                                json!({ "id": id.clone(), "track_id": t.id.clone() }),
                            );
                        }
                    })
                };
                let editing = renaming
                    .as_ref()
                    .filter(|(rid, _)| rid == &p.id)
                    .map(|(_, draft)| draft.clone());
                let start_rename = {
                    let renaming = renaming.clone();
                    let id = p.id.clone();
                    let name = p.name.clone();
                    Callback::from(move |_: ()| renaming.set(Some((id.clone(), name.clone()))))
                };
                let delete_action = {
                    let cb = on_delete_playlist.clone();
                    let id = p.id.clone();
                    Callback::from(move |_: ()| cb.emit(id.clone()))
                };
                let header_menu = vec![
                    MenuAction::Item {
                        icon: "edit",
                        label: "Rename".into(),
                        danger: false,
                        cb: start_rename,
                    },
                    MenuAction::Item {
                        icon: "trash",
                        label: "Delete".into(),
                        danger: true,
                        cb: delete_action,
                    },
                ];
                let rename_input = {
                    let renaming = renaming.clone();
                    let id = p.id.clone();
                    Callback::from(move |e: InputEvent| {
                        let el: HtmlInputElement = e.target_unchecked_into();
                        renaming.set(Some((id.clone(), el.value())));
                    })
                };
                let rename_key = {
                    let renaming = renaming.clone();
                    let id = p.id.clone();
                    Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
                        "Enter" => {
                            let el: HtmlInputElement = e.target_unchecked_into();
                            let name = el.value();
                            if !name.trim().is_empty() {
                                api::fire(
                                    "rename_playlist",
                                    json!({ "id": id.clone(), "name": name.trim() }),
                                );
                            }
                            renaming.set(None);
                        }
                        "Escape" => renaming.set(None),
                        _ => {}
                    })
                };
                let rename_blur = {
                    let renaming = renaming.clone();
                    Callback::from(move |_: FocusEvent| renaming.set(None))
                };
                html! {
                    <>
                        <div class="view-head">
                            if let Some(draft) = editing {
                                <input class="rename-input" type="text" value={draft}
                                       autofocus=true oninput={rename_input}
                                       onkeydown={rename_key} onblur={rename_blur} />
                            } else {
                                <h2>{ &p.name }</h2>
                                <MenuButton actions={header_menu} align_right=false />
                            }
                        </div>
                        if p.tracks.is_empty() {
                            <div class="empty">{ "Empty playlist. Use ♪+ on any track to add it." }</div>
                        } else {
                            { collection_meta(&p.tracks) }
                            { actions(p.tracks.clone(), Some(format!("playlist:{}", p.id))) }
                            { list(p.tracks.clone(), Some(format!("playlist:{}", p.id)), Some(remove)) }
                        }
                    </>
                }
            }
        },
        View::Artist(_) => match &*artist_page {
            None => html! { <div class="empty">{ "Loading artist..." }</div> },
            Some(p) => {
                // Split standalone singles into their own section; albums and EPs
                // stay together under "Albums".
                let singles: Vec<AlbumSummary> = p
                    .albums
                    .iter()
                    .filter(|a| a.album_type == "Single")
                    .cloned()
                    .collect();
                let albums: Vec<AlbumSummary> = p
                    .albums
                    .iter()
                    .filter(|a| a.album_type != "Single")
                    .cloned()
                    .collect();
                html! {
                    <>
                        <div class="page-head">
                            { cover(&p.image, "artist-avatar") }
                            <div class="page-head-meta">
                                <h1 class="page-title">{ &p.name }</h1>
                            </div>
                        </div>
                        <div class="view-head">
                            <h2>{ "Top songs" }</h2>
                            if p.tracks_playlist_id.is_some() {
                                <button class="link-btn" onclick={{
                                    let cb = on_open_artist_songs.clone();
                                    let id = p.id.clone();
                                    Callback::from(move |_| cb.emit(id.clone()))
                                }}>{ "Show all" }</button>
                            }
                        </div>
                        { list(p.tracks.clone(), None, None) }
                        if !albums.is_empty() {
                            <h2>{ "Albums" }</h2>
                            { album_grid(&albums, on_open_album.clone()) }
                        }
                        if !singles.is_empty() {
                            <h2>{ "Singles" }</h2>
                            { album_grid(&singles, on_open_album.clone()) }
                        }
                    </>
                }
            }
        },
        View::ArtistSongs(id) => {
            // Reuse the artist page's name/avatar (kept in state) so this view
            // matches the artist profile's header. The source tag keeps the
            // playing-collection highlight working like other collections.
            let artist = artist_page.as_ref().filter(|p| &p.id == id);
            let source = Some(format!("artist:{id}"));
            // Only treat the cache as loaded if it's for the artist in view (a
            // different artist's list is mid-reload, shown as "Loading").
            let songs = (*artist_songs)
                .as_ref()
                .filter(|(cached_id, _)| cached_id == id)
                .map(|(_, t)| t);
            let open_artist = {
                let cb = on_open_artist.clone();
                let id = id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            };
            html! {
                <>
                    <div class="page-head">
                        { cover(artist.map(|p| p.image.as_str()).unwrap_or_default(), "artist-avatar") }
                        <div class="page-head-meta">
                            <div class="page-kind">{ "All songs" }</div>
                            <h1 class="page-title">
                                if let Some(p) = artist {
                                    <span class="link" onclick={open_artist}>{ &p.name }</span>
                                }
                            </h1>
                            if let Some(t) = songs.filter(|t| !t.is_empty()) {
                                <div class="page-sub">
                                    { format!(
                                        "{} songs • {}",
                                        t.len(),
                                        fmt_total_duration(t.iter().filter_map(|t| t.duration).sum())
                                    ) }
                                </div>
                                { actions(t.clone(), source.clone()) }
                            }
                        </div>
                    </div>
                    { match songs {
                        None => html! { <div class="empty">{ "Loading songs..." }</div> },
                        Some(t) if t.is_empty() => html! { <div class="empty">{ "No songs found." }</div> },
                        Some(t) => list(t.clone(), source.clone(), None),
                    } }
                </>
            }
        }
        View::Album(_) => match &*album_page {
            None => html! { <div class="empty">{ "Loading album..." }</div> },
            Some(p) => {
                let kind = match p.year {
                    Some(y) => format!("{} • {y}", p.album_type),
                    None => p.album_type.clone(),
                };
                let source = Some(format!("album:{}", p.id));
                html! {
                    <>
                        <div class="page-head">
                            { cover(&p.cover, "album-art") }
                            <div class="page-head-meta">
                                <div class="page-kind">{ kind }</div>
                                <h1 class="page-title">{ &p.name }</h1>
                                <div class="page-sub">
                                    if let Some(id) = p.artist_id.clone() {
                                        <span class="link" onclick={{
                                            let cb = on_open_artist.clone();
                                            Callback::from(move |_| cb.emit(id.clone()))
                                        }}>{ &p.artist }</span>
                                    } else {
                                        { &p.artist }
                                    }
                                    { format!(
                                        " • {} songs • {}",
                                        p.tracks.len(),
                                        fmt_total_duration(
                                            p.tracks.iter().filter_map(|t| t.duration).sum()
                                        )
                                    ) }
                                </div>
                                { actions(p.tracks.clone(), source.clone()) }
                            </div>
                        </div>
                        { list(p.tracks.clone(), source, None) }
                    </>
                }
            }
        },
    };

    let liked_current = track
        .as_ref()
        .map(|t| liked_ids.contains(&t.id))
        .unwrap_or(false);

    // Show the equalizer only while a track is actually loaded.
    let playing_source = match pstate {
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading => {
            queue.source.clone()
        }
        PlaybackState::Stopped => None,
    };
    let is_playing = pstate == PlaybackState::Playing;

    html! {
        <div class="app">
            <Titlebar on_search={on_search} />
            <div class="body">
                <Sidebar view={(*view).clone()} playlists={sidebar_playlists}
                         on_nav={on_nav} on_new_playlist={on_new_playlist}
                         on_import_playlist={on_import_playlist} on_import_file={on_import_file}
                         on_rename_playlist={on_rename_playlist} on_delete_playlist={on_delete_playlist}
                         on_export_playlist={on_export_playlist}
                         on_move_playlist={on_move_playlist}
                         playing_source={playing_source} is_playing={is_playing} />
                <div class="content">
                    <main class="main">{ main }</main>
                    if *view != View::Settings {
                        <QueuePanel queue={queue.clone()}
                                    on_jump={on_queue_jump}
                                    on_remove={on_queue_remove}
                                    on_move={on_queue_move}
                                    on_clear={on_queue_clear} />
                    }
                </div>
            </div>
            <PlayerBar track={track.clone()} state={pstate} progress={progress}
                       volume={*volume} shuffle={queue.shuffle} repeat={queue.repeat}
                       liked={liked_current} on_open_artist={on_open_artist.clone()}
                       on_volume={on_volume} />
            if let Some(msg) = &*toast {
                <div class="toast">{ msg }</div>
            }
            if let Some(u) = &*update_banner {
                <UpdateBanner
                    status={u.clone()}
                    on_dismiss={
                        let update_banner = update_banner.clone();
                        Callback::from(move |()| update_banner.set(None))
                    }
                    // "Don't ask again" silences future launch checks and clears
                    // the banner; the same switch lives in Settings.
                    on_silence={
                        let on_update_notifications = on_update_notifications.clone();
                        let update_banner = update_banner.clone();
                        Callback::from(move |()| {
                            on_update_notifications.emit(false);
                            update_banner.set(None);
                        })
                    }
                />
            }
        </div>
    }
}

// ------------------------------------------------------------ preview mode

/// A placeholder track for preview mode (empty cover renders the fallback
/// tile, so no real artwork appears).
#[cfg(debug_assertions)]
fn preview_track(n: usize) -> Track {
    Track {
        id: format!("preview-{n}"),
        title: format!("Song Title {n}"),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(150 + (n as u32 * 37) % 120),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

/// Neutral library rendered in preview mode, so repo screenshots show generic
/// placeholders instead of a personal library.
#[cfg(debug_assertions)]
fn preview_library() -> Library {
    let playlist = |n: usize, len: usize, start: usize| Playlist {
        id: format!("preview-pl-{n}"),
        name: format!("Playlist {n}"),
        tracks: (start..start + len).map(preview_track).collect(),
    };
    Library {
        liked: (1..=8).map(preview_track).collect(),
        playlists: vec![playlist(1, 12, 1), playlist(2, 9, 13), playlist(3, 16, 22)],
        recently_played: (1..=12).map(preview_track).collect(),
    }
}

/// Placeholder queue: a handful of upcoming tracks with the first playing.
#[cfg(debug_assertions)]
fn preview_queue() -> QueueSnapshot {
    QueueSnapshot {
        tracks: (1..=6).map(preview_track).collect(),
        current: Some(0),
        shuffle: false,
        repeat: rift_types::RepeatMode::Off,
        source: None,
    }
}
