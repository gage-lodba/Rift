//! Root component: owns all state, subscribes to backend events and routes
//! between views.

use rift_types::{
    events, AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Progress, QueueSnapshot, Track,
};
use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
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

fn format_subs(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M subscribers", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K subscribers", n as f64 / 1e3)
    } else {
        format!("{n} subscribers")
    }
}

fn album_subtitle(a: &AlbumSummary) -> String {
    match a.year {
        Some(y) => format!("{} • {y}", a.album_type),
        None => a.album_type.clone(),
    }
}

fn album_grid(albums: &[AlbumSummary], on_open: Callback<String>) -> Html {
    html! {
        <div class="card-grid">
            { for albums.iter().map(|a| {
                let cb = on_open.clone();
                let id = a.id.clone();
                html! {
                    <div class="card" onclick={Callback::from(move |_| cb.emit(id.clone()))}>
                        { cover(&a.cover, "card-cover") }
                        <div class="card-name">{ &a.name }</div>
                        <div class="card-sub">{ album_subtitle(a) }</div>
                    </div>
                }
            }) }
        </div>
    }
}

fn artist_grid(artists: &[ArtistSummary], on_open: Callback<String>) -> Html {
    html! {
        <div class="card-grid">
            { for artists.iter().map(|a| {
                let cb = on_open.clone();
                let id = a.id.clone();
                html! {
                    <div class="card card-artist" onclick={Callback::from(move |_| cb.emit(id.clone()))}>
                        { cover(&a.avatar, "card-cover round") }
                        <div class="card-name">{ &a.name }</div>
                        <div class="card-sub">{ a.subscribers.map(format_subs).unwrap_or_default() }</div>
                    </div>
                }
            }) }
        </div>
    }
}

#[function_component(App)]
pub fn app() -> Html {
    let library = use_state(Library::default);
    let queue = use_state(QueueSnapshot::default);
    let pstate = use_state(PlaybackState::default);
    let track = use_state(|| None::<Track>);
    let progress = use_state(Progress::default);
    let volume = use_state(|| 1.0f32);
    let view = use_state(|| View::Home);
    let search = use_state(Search::default);
    let search_tab = use_state(SearchTab::default);
    let artist_results = use_state(|| None::<Vec<ArtistSummary>>);
    let album_results = use_state(|| None::<Vec<AlbumSummary>>);
    let artist_page = use_state(|| None::<ArtistPage>);
    let album_page = use_state(|| None::<AlbumPage>);
    let downloads = use_state(DownloadState::default);
    // (playlist id, draft name) while a playlist is being renamed.
    let renaming = use_state(|| None::<(String, String)>);
    let toast = use_state(|| None::<String>);

    // Subscribe to backend events and load the initial snapshot, once.
    {
        let library = library.clone();
        let queue = queue.clone();
        let pstate = pstate.clone();
        let track = track.clone();
        let progress = progress.clone();
        let volume = volume.clone();
        let downloads = downloads.clone();
        let toast = toast.clone();
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
            {
                let progress = progress.clone();
                api::listen_event::<Progress>(
                    events::PROGRESS,
                    Callback::from(move |v| progress.set(v)),
                );
            }
            {
                let downloads = downloads.clone();
                api::listen_event::<DownloadState>(
                    events::DOWNLOADS,
                    Callback::from(move |v| downloads.set(v)),
                );
            }
            {
                let toast = toast.clone();
                api::listen_event::<String>(
                    events::ERROR,
                    Callback::from(move |msg: String| {
                        toast.set(Some(msg));
                        let toast = toast.clone();
                        spawn_local(async move {
                            gloo_timers::future::TimeoutFuture::new(6000).await;
                            toast.set(None);
                        });
                    }),
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
                        downloads.set(b.downloads);
                    }
                    Err(e) => web_sys::console::error_1(&format!("bootstrap: {e}").into()),
                }
            });
            || {}
        });
    }

    // Load artist/album pages whenever the view navigates to one.
    {
        let artist_page = artist_page.clone();
        let album_page = album_page.clone();
        let toast = toast.clone();
        use_effect_with((*view).clone(), move |v| {
            match v {
                View::Artist(id) => {
                    artist_page.set(None);
                    let id = id.clone();
                    spawn_local(async move {
                        match api::invoke::<ArtistPage>("get_artist", &json!({ "id": id })).await {
                            Ok(p) => artist_page.set(Some(p)),
                            Err(e) => toast.set(Some(e)),
                        }
                    });
                }
                View::Album(id) => {
                    album_page.set(None);
                    let id = id.clone();
                    spawn_local(async move {
                        match api::invoke::<AlbumPage>("get_album", &json!({ "id": id })).await {
                            Ok(p) => album_page.set(Some(p)),
                            Err(e) => toast.set(Some(e)),
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
            let search = search.clone();
            spawn_local(async move {
                match api::invoke::<Vec<Track>>("search", &json!({ "query": query.clone() })).await
                {
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

    // Lazily fetch artist/album results when their tab is first opened.
    let on_tab = {
        let search_tab = search_tab.clone();
        let artist_results = artist_results.clone();
        let album_results = album_results.clone();
        let query = search.query.clone();
        let toast = toast.clone();
        Callback::from(move |t: SearchTab| {
            search_tab.set(t);
            match t {
                SearchTab::Artists if artist_results.is_none() => {
                    let results = artist_results.clone();
                    let query = query.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        match api::invoke::<Vec<ArtistSummary>>(
                            "search_artists",
                            &json!({ "query": query }),
                        )
                        .await
                        {
                            Ok(v) => results.set(Some(v)),
                            Err(e) => {
                                toast.set(Some(e));
                                results.set(Some(vec![]));
                            }
                        }
                    });
                }
                SearchTab::Albums if album_results.is_none() => {
                    let results = album_results.clone();
                    let query = query.clone();
                    let toast = toast.clone();
                    spawn_local(async move {
                        match api::invoke::<Vec<AlbumSummary>>(
                            "search_albums",
                            &json!({ "query": query }),
                        )
                        .await
                        {
                            Ok(v) => results.set(Some(v)),
                            Err(e) => {
                                toast.set(Some(e));
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
        Callback::from(move |start: usize| {
            api::fire(
                "play_tracks",
                json!({ "tracks": tracks, "start": start, "source": source }),
            );
        })
    };

    let on_like = Callback::from(|t: Track| api::fire("toggle_like", json!({ "track": t })));
    let on_queue_add = Callback::from(|t: Track| api::fire("queue_add", json!({ "track": t })));
    let on_add_to_playlist = Callback::from(|(id, t): (String, Track)| {
        api::fire("add_to_playlist", json!({ "id": id, "track": t }));
    });
    let on_new_playlist =
        Callback::from(|name: String| api::fire("create_playlist", json!({ "name": name })));
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

    let on_queue_jump = Callback::from(|i: usize| api::fire("queue_jump", json!({ "index": i })));
    let on_queue_remove =
        Callback::from(|i: usize| api::fire("queue_remove", json!({ "index": i })));
    let on_queue_clear = Callback::from(|()| api::fire("queue_clear", json!({})));
    let on_volume = {
        let volume = volume.clone();
        Callback::from(move |v: f32| {
            volume.set(v);
            api::fire("set_volume", json!({ "volume": v }));
        })
    };

    // -------------------------------------------------------------- views

    let liked_ids: Vec<String> = library.liked.iter().map(|t| t.id.clone()).collect();
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

    let list = |tracks: Vec<Track>, source: Option<String>, removable: Option<Callback<usize>>| {
        html! {
            <TrackList
                tracks={tracks.clone()}
                liked_ids={liked_ids.clone()}
                playing_id={track.as_ref().map(|t| t.id.clone())}
                downloaded_ids={downloads.downloaded.clone()}
                playlists={playlist_opts.clone()}
                on_play={play_list(tracks, source)}
                on_like={on_like.clone()}
                on_queue={on_queue_add.clone()}
                on_add_to_playlist={on_add_to_playlist.clone()}
                on_open_artist={on_open_artist.clone()}
                on_open_album={on_open_album.clone()}
                on_remove={removable}
            />
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
            .filter(|id| downloads.downloaded.contains(id))
            .count();
        let n_active = ids
            .iter()
            .filter(|id| downloads.downloading.contains(id))
            .count();

        let play = {
            let tracks = tracks.clone();
            let source = source.clone();
            Callback::from(move |_: MouseEvent| {
                api::fire(
                    "play_tracks",
                    json!({ "tracks": tracks, "start": 0usize, "source": source }),
                );
            })
        };

        let download = if n_active > 0 {
            html! {
                <button class="head-btn" disabled=true>
                    <span class="spinner"></span>
                    { format!("Downloading {}/{}", n_done, total) }
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
            let label = if n_done > 0 {
                "Download rest"
            } else {
                "Download"
            };
            html! {
                <button class="head-btn" onclick={download}>
                    { icon("download") }<span>{ label }</span>
                </button>
            }
        };

        html! {
            <div class="head-actions">
                <button class="play-all" onclick={play}>{ icon("play") }<span>{ "Play" }</span></button>
                { download }
            </div>
        }
    };

    let main = match &*view {
        View::Home => html! {
            <HomeView library={(*library).clone()} on_nav={on_nav.clone()} on_play={on_play_single.clone()} />
        },
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
                SearchTab::Artists => match &*artist_results {
                    None => html! { <div class="empty">{ "Searching..." }</div> },
                    Some(v) if v.is_empty() => html! { <div class="empty">{ "No results." }</div> },
                    Some(v) => artist_grid(v, on_open_artist.clone()),
                },
                SearchTab::Albums => match &*album_results {
                    None => html! { <div class="empty">{ "Searching..." }</div> },
                    Some(v) if v.is_empty() => html! { <div class="empty">{ "No results." }</div> },
                    Some(v) => album_grid(v, on_open_album.clone()),
                },
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
                <h2>{ "Liked Songs" }</h2>
                if library.liked.is_empty() {
                    <div class="empty">{ "No liked songs yet. Click the heart on any track." }</div>
                } else {
                    { actions(library.liked.clone(), Some("liked".to_string())) }
                    { list(library.liked.clone(), Some("liked".to_string()), None) }
                }
            </>
        },
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
                    let id = p.id.clone();
                    let view = view.clone();
                    Callback::from(move |_: ()| {
                        api::fire("delete_playlist", json!({ "id": id.clone() }));
                        view.set(View::Home);
                    })
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
                            { actions(p.tracks.clone(), Some(format!("playlist:{}", p.id))) }
                            { list(p.tracks.clone(), Some(format!("playlist:{}", p.id)), Some(remove)) }
                        }
                    </>
                }
            }
        },
        View::Artist(_) => match &*artist_page {
            None => html! { <div class="empty">{ "Loading artist..." }</div> },
            Some(p) => html! {
                <>
                    <div class="page-head">
                        { cover(&p.image, "artist-avatar") }
                        <div class="page-head-meta">
                            <h1 class="page-title">{ &p.name }</h1>
                            if let Some(n) = p.subscribers {
                                <div class="page-sub">{ format_subs(n) }</div>
                            }
                            if let Some(d) = &p.description {
                                <p class="page-desc">{ d }</p>
                            }
                        </div>
                    </div>
                    <h2>{ "Top songs" }</h2>
                    { list(p.tracks.clone(), None, None) }
                    if !p.albums.is_empty() {
                        <h2>{ "Albums" }</h2>
                        { album_grid(&p.albums, on_open_album.clone()) }
                    }
                </>
            },
        },
        View::Album(_) => match &*album_page {
            None => html! { <div class="empty">{ "Loading album..." }</div> },
            Some(p) => {
                let play_all = {
                    let tracks = p.tracks.clone();
                    Callback::from(move |_: MouseEvent| {
                        api::fire("play_tracks", json!({ "tracks": tracks, "start": 0usize }));
                    })
                };
                let kind = match p.year {
                    Some(y) => format!("{} • {y}", p.album_type),
                    None => p.album_type.clone(),
                };
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
                                    { format!(" • {} songs", p.tracks.len()) }
                                </div>
                                <button class="play-all" onclick={play_all}>
                                    { icon("play") }<span>{ "Play" }</span>
                                </button>
                            </div>
                        </div>
                        { list(p.tracks.clone(), None, None) }
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
    let playing_source = match *pstate {
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading => {
            queue.source.clone()
        }
        PlaybackState::Stopped => None,
    };
    let is_playing = *pstate == PlaybackState::Playing;

    html! {
        <div class="app">
            <Titlebar on_search={on_search} />
            <div class="body">
                <Sidebar view={(*view).clone()} playlists={sidebar_playlists}
                         on_nav={on_nav} on_new_playlist={on_new_playlist}
                         on_rename_playlist={on_rename_playlist} on_delete_playlist={on_delete_playlist}
                         playing_source={playing_source} is_playing={is_playing} />
                <div class="content">
                    <main class="main">{ main }</main>
                    <QueuePanel queue={(*queue).clone()}
                                on_jump={on_queue_jump}
                                on_remove={on_queue_remove}
                                on_clear={on_queue_clear} />
                </div>
            </div>
            <PlayerBar track={(*track).clone()} state={*pstate} progress={*progress}
                       volume={*volume} shuffle={queue.shuffle} repeat={queue.repeat}
                       liked={liked_current} on_open_artist={on_open_artist.clone()}
                       on_volume={on_volume} />
            if let Some(msg) = &*toast {
                <div class="toast">{ msg }</div>
            }
        </div>
    }
}
