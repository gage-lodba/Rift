//! Presentational components for the Rift UI.

use rift_types::{ArtistRef, Library, PlaybackState, Progress, QueueSnapshot, RepeatMode, Track};
use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::api;

#[derive(Clone, PartialEq)]
pub enum View {
    Home,
    Search,
    Liked,
    Playlist(String),
    Artist(String),
    Album(String),
}

// ------------------------------------------------------------------ icons

/// Inline solid (filled) SVG icons, Material-style.
pub fn icon(name: &str) -> Html {
    let d = match name {
        "home" => "M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z",
        "heart" => "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
        "heart-outline" => "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z",
        "music" => "M21 3v12.5c0 1.38-1.12 2.5-2.5 2.5S16 16.88 16 15.5s1.12-2.5 2.5-2.5c.35 0 .69.07 1 .18V8.4L10 9.7v7.8c0 1.38-1.12 2.5-2.5 2.5S5 18.88 5 17.5 6.12 15 7.5 15c.35 0 .69.07 1 .18V6.3L21 3z",
        "plus" => "M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z",
        "search" => "M15.5 14h-.79l-.28-.27C15.41 12.59 16 11.11 16 9.5 16 5.91 13.09 3 9.5 3S3 5.91 3 9.5 5.91 16 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z",
        "play" => "M8 5v14l11-7z",
        "pause" => "M6 19h4V5H6v14zm8-14v14h4V5h-4z",
        "prev" => "M6 6h2v12H6zm3.5 6l8.5 6V6z",
        "next" => "M6 18l8.5-6L6 6v12zm10-12v12h2V6h-2z",
        "shuffle" => "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
        "repeat" => "M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z",
        "repeat-one" => "M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4zm-4-2V9h-1l-2 1v1h1.5v4H13z",
        "volume" => "M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z",
        "x" => "M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z",
        "min" => "M19 13H5v-2h14v2z",
        "max" => "M4 4h16v16H4V4zm2 2v12h12V6H6z",
        "trash" => "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z",
        "edit" => "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34a.9959.9959 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z",
        "download" => "M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z",
        "check" => "M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z",
        "check-circle" => "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z",
        "kebab" => "M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z",
        "queue" => "M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z",
        "person" => "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
        "album" => "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 14.5c-2.49 0-4.5-2.01-4.5-4.5S9.51 7.5 12 7.5s4.5 2.01 4.5 4.5-2.01 4.5-4.5 4.5zm0-5.5c-.55 0-1 .45-1 1s.45 1 1 1 1-.45 1-1-.45-1-1-1z",
        _ => "",
    };
    html! {
        <svg viewBox="0 0 24 24" fill="currentColor"><path d={d.to_string()} /></svg>
    }
}

pub fn cover(url: &str, class: &'static str) -> Html {
    if url.is_empty() {
        html! { <div class={classes!(class, "cover-fallback")}>{ icon("music") }</div> }
    } else {
        html! { <img class={class} src={url.to_string()} loading="lazy" /> }
    }
}

/// Render a track's artist credits, each one a profile link where YouTube
/// provides a channel ID. Falls back to the plain joined string for tracks
/// saved before per-artist credits existed.
pub fn artist_links(artists: &[ArtistRef], fallback: &str, on_open: &Callback<String>) -> Html {
    if artists.is_empty() {
        return html! { { fallback.to_string() } };
    }
    html! {
        <>
            { for artists.iter().enumerate().map(|(i, a)| {
                let name = a.name.clone();
                let link = a.id.clone().map(|id| {
                    let cb = on_open.clone();
                    Callback::from(move |e: MouseEvent| {
                        e.stop_propagation();
                        cb.emit(id.clone());
                    })
                });
                html! {
                    <>
                        if i > 0 { { ", " } }
                        if let Some(link) = link {
                            <span class="link" onclick={link}>{ name }</span>
                        } else {
                            { name }
                        }
                    </>
                }
            }) }
        </>
    }
}

pub fn fmt_secs(total: u32) -> String {
    format!("{}:{:02}", total / 60, total % 60)
}

fn fmt_pos(secs: f64) -> String {
    fmt_secs(secs.max(0.0) as u32)
}

// --------------------------------------------------------------- titlebar

#[derive(Properties, PartialEq)]
pub struct TitlebarProps {
    pub on_search: Callback<String>,
}

#[function_component(Titlebar)]
pub fn titlebar(props: &TitlebarProps) -> Html {
    let input = use_node_ref();

    let submit = {
        let input = input.clone();
        let on_search = props.on_search.clone();
        Callback::from(move |()| {
            if let Some(el) = input.cast::<HtmlInputElement>() {
                let q = el.value();
                if !q.trim().is_empty() {
                    on_search.emit(q.trim().to_string());
                }
            }
        })
    };
    let onkeydown = {
        let submit = submit.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Enter" {
                submit.emit(());
            }
        })
    };
    let onclick = {
        let submit = submit.clone();
        Callback::from(move |_| submit.emit(()))
    };

    html! {
        <header class="titlebar" data-tauri-drag-region="true">
            <div class="logo" data-tauri-drag-region="true">{ "RIFT" }</div>
            <div class="searchbox">
                <input ref={input} type="text" placeholder="Search YouTube Music..." {onkeydown} />
                <button class="search-btn" {onclick}>{ "Search" }</button>
            </div>
            <div class="tb-space" data-tauri-drag-region="true"></div>
            <div class="win-controls">
                <button class="win-btn" title="Minimize"
                    onclick={|_| api::fire("window_minimize", json!({}))}>{ icon("min") }</button>
                <button class="win-btn" title="Maximize"
                    onclick={|_| api::fire("window_toggle_maximize", json!({}))}>{ icon("max") }</button>
                <button class="win-btn win-close" title="Close"
                    onclick={|_| api::fire("window_close", json!({}))}>{ icon("x") }</button>
            </div>
        </header>
    }
}

// ---------------------------------------------------------------- sidebar

#[derive(Properties, PartialEq)]
pub struct SidebarProps {
    pub view: View,
    /// (id, name, track count)
    pub playlists: Vec<(String, String, usize)>,
    pub on_nav: Callback<View>,
    pub on_new_playlist: Callback<String>,
    pub on_rename_playlist: Callback<String>,
    pub on_delete_playlist: Callback<String>,
    /// Source tag of the playing queue ("liked", "playlist:<id>").
    pub playing_source: Option<String>,
    pub is_playing: bool,
}

#[function_component(Sidebar)]
pub fn sidebar(props: &SidebarProps) -> Html {
    let creating = use_state(|| false);
    let input = use_node_ref();

    let nav = |view: View| {
        let on_nav = props.on_nav.clone();
        Callback::from(move |_: MouseEvent| on_nav.emit(view.clone()))
    };

    let start_create = {
        let creating = creating.clone();
        Callback::from(move |_: MouseEvent| creating.set(true))
    };
    let onkeydown = {
        let creating = creating.clone();
        let input = input.clone();
        let on_new = props.on_new_playlist.clone();
        Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
            "Enter" => {
                if let Some(el) = input.cast::<HtmlInputElement>() {
                    let name = el.value();
                    if !name.trim().is_empty() {
                        on_new.emit(name.trim().to_string());
                    }
                }
                creating.set(false);
            }
            "Escape" => creating.set(false),
            _ => {}
        })
    };

    let item_class = |active: bool| classes!("side-item", active.then_some("active"));

    let eq_markup = || {
        html! {
            <span class={classes!("eq", (!props.is_playing).then_some("paused"))}>
                <span></span><span></span><span></span>
            </span>
        }
    };
    // Leading icon for a collection: the animated equalizer while it plays,
    // otherwise the given icon.
    let lead = |source: &str, icon_name: &str| {
        if props.playing_source.as_deref() == Some(source) {
            eq_markup()
        } else {
            icon(icon_name)
        }
    };

    html! {
        <nav class="sidebar">
            <div class={item_class(props.view == View::Home)} onclick={nav(View::Home)}>
                { icon("home") }<span>{ "Home" }</span>
            </div>
            <div class={item_class(props.view == View::Liked)} onclick={nav(View::Liked)}>
                { lead("liked", "heart") }<span class="side-name">{ "Liked Songs" }</span>
            </div>

            <div class="side-head">{ "PLAYLISTS" }</div>
            { for props.playlists.iter().map(|(id, name, count)| html! {
                <SidebarPlaylist
                    id={id.clone()}
                    name={name.clone()}
                    count={*count}
                    active={props.view == View::Playlist(id.clone())}
                    playing={props.playing_source.as_deref() == Some(format!("playlist:{id}").as_str())}
                    is_playing={props.is_playing}
                    on_nav={props.on_nav.clone()}
                    on_rename={props.on_rename_playlist.clone()}
                    on_delete={props.on_delete_playlist.clone()}
                />
            }) }

            if *creating {
                <input class="side-new-input" ref={input} type="text"
                       placeholder="Playlist name" autofocus=true {onkeydown}
                       onblur={ let c = creating.clone(); Callback::from(move |_| c.set(false)) } />
            } else {
                <div class="side-item side-new" onclick={start_create}>
                    { icon("plus") }<span>{ "New playlist" }</span>
                </div>
            }
        </nav>
    }
}

#[derive(Properties, PartialEq)]
struct SidebarPlaylistProps {
    id: String,
    name: String,
    count: usize,
    active: bool,
    /// This playlist is the source of the playing queue.
    playing: bool,
    /// Playback is active (vs paused), for the equalizer animation.
    is_playing: bool,
    on_nav: Callback<View>,
    on_rename: Callback<String>,
    on_delete: Callback<String>,
}

#[function_component(SidebarPlaylist)]
fn sidebar_playlist(props: &SidebarPlaylistProps) -> Html {
    let menu_open = use_state(|| false);

    let nav = {
        let cb = props.on_nav.clone();
        let id = props.id.clone();
        Callback::from(move |_: MouseEvent| cb.emit(View::Playlist(id.clone())))
    };
    let context = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.prevent_default();
            menu_open.set(true);
        })
    };
    let toggle = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            menu_open.set(!*menu_open);
        })
    };
    let close = {
        let menu_open = menu_open.clone();
        Callback::from(move |_| menu_open.set(false))
    };

    let lead = if props.playing {
        html! {
            <span class={classes!("eq", (!props.is_playing).then_some("paused"))}>
                <span></span><span></span><span></span>
            </span>
        }
    } else {
        icon("music")
    };

    let actions = vec![
        MenuAction::Item {
            icon: "edit",
            label: "Rename".into(),
            danger: false,
            cb: {
                let cb = props.on_rename.clone();
                let id = props.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        },
        MenuAction::Item {
            icon: "trash",
            label: "Delete".into(),
            danger: true,
            cb: {
                let cb = props.on_delete.clone();
                let id = props.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        },
    ];

    html! {
        <div class={classes!("side-item", props.active.then_some("active"), (*menu_open).then_some("menu-open"))}
             onclick={nav} oncontextmenu={context}>
            { lead }
            <span class="side-name">{ &props.name }</span>
            <span class="side-count">{ props.count }</span>
            <div class="menu-anchor side-kebab">
                <button class="ibtn" title="More" onclick={toggle}>{ icon("kebab") }</button>
                <Menu open={*menu_open} on_close={close} actions={actions} align_right=true />
            </div>
        </div>
    }
}

// -------------------------------------------------------------- home view

#[derive(Properties, PartialEq)]
pub struct HomeProps {
    pub library: Library,
    pub on_nav: Callback<View>,
    /// Play a single track (starts radio).
    pub on_play: Callback<Track>,
}

#[function_component(HomeView)]
pub fn home_view(props: &HomeProps) -> Html {
    let lib = &props.library;
    let card = |view: View, cover_url: &str, name: &str, count: usize| {
        let on_nav = props.on_nav.clone();
        let v = view.clone();
        html! {
            <div class="card" onclick={Callback::from(move |_| on_nav.emit(v.clone()))}>
                { cover(cover_url, "card-cover") }
                <div class="card-name">{ name }</div>
                <div class="card-sub">{ format!("{count} songs") }</div>
            </div>
        }
    };

    html! {
        <>
            <h2>{ "Your library" }</h2>
            <div class="card-grid">
                { card(View::Liked,
                       lib.liked.first().map(|t| t.cover.as_str()).unwrap_or(""),
                       "Liked Songs", lib.liked.len()) }
                { for lib.playlists.iter().map(|p| card(
                    View::Playlist(p.id.clone()),
                    p.tracks.first().map(|t| t.cover.as_str()).unwrap_or(""),
                    &p.name, p.tracks.len())) }
            </div>

            <h2>{ "Recently played" }</h2>
            if lib.recently_played.is_empty() {
                <div class="empty">{ "Nothing played yet. Search for a song to get started." }</div>
            } else {
                <div class="card-grid">
                    { for lib.recently_played.iter().take(12).map(|t| {
                        let on_play = props.on_play.clone();
                        let track = t.clone();
                        html! {
                            <div class="card" onclick={Callback::from(move |_| on_play.emit(track.clone()))}>
                                { cover(&t.cover, "card-cover") }
                                <div class="card-name">{ &t.title }</div>
                                <div class="card-sub">{ &t.artist }</div>
                            </div>
                        }
                    }) }
                </div>
            }
        </>
    }
}

// ------------------------------------------------------------- track list

// ------------------------------------------------------------ context menu

/// One entry in a kebab / right-click menu.
#[derive(Clone, PartialEq)]
pub enum MenuAction {
    /// A plain action item.
    Item {
        icon: &'static str,
        label: String,
        danger: bool,
        cb: Callback<()>,
    },
    /// An item that expands into a list of options (e.g. "Add to playlist").
    /// `cb` receives the chosen option's id.
    Sub {
        icon: &'static str,
        label: String,
        options: Vec<(String, String)>,
        cb: Callback<String>,
    },
    Separator,
}

#[derive(Properties, PartialEq)]
pub struct MenuProps {
    pub open: bool,
    pub on_close: Callback<()>,
    pub actions: Vec<MenuAction>,
    /// Align the popup to the right edge of its anchor.
    #[prop_or_default]
    pub align_right: bool,
}

/// A controlled popup menu: render it inside a `.menu-anchor` and drive `open`
/// from the parent (so both a kebab button and a right-click can open it).
#[function_component(Menu)]
pub fn menu(props: &MenuProps) -> Html {
    let expanded = use_state(|| None::<usize>);
    {
        // Collapse any expanded submenu whenever the menu closes.
        let expanded = expanded.clone();
        use_effect_with(props.open, move |open| {
            if !*open {
                expanded.set(None);
            }
            || ()
        });
    }
    if !props.open {
        return html! {};
    }
    let close = props.on_close.clone();
    let backdrop_close = close.clone();

    html! {
        <>
            <div class="menu-backdrop"
                 onclick={Callback::from(move |_| backdrop_close.emit(()))}
                 oncontextmenu={let c = close.clone(); Callback::from(move |e: MouseEvent| { e.prevent_default(); c.emit(()); })}>
            </div>
            <div class={classes!("menu", props.align_right.then_some("menu-right"))}
                 onclick={|e: MouseEvent| e.stop_propagation()}>
                { for props.actions.iter().enumerate().map(|(i, a)| {
                    match a {
                        MenuAction::Separator => html! { <div class="menu-sep"></div> },
                        MenuAction::Item { icon: ic, label, danger, cb } => {
                            let cb = cb.clone();
                            let close = close.clone();
                            let onclick = Callback::from(move |_: MouseEvent| { cb.emit(()); close.emit(()); });
                            html! {
                                <div class={classes!("menu-item", danger.then_some("danger"))} onclick={onclick}>
                                    { icon(ic) }<span>{ label }</span>
                                </div>
                            }
                        }
                        MenuAction::Sub { icon: ic, label, options, cb } => {
                            let is_open = *expanded == Some(i);
                            let toggle = {
                                let expanded = expanded.clone();
                                Callback::from(move |_: MouseEvent| {
                                    expanded.set(if is_open { None } else { Some(i) });
                                })
                            };
                            html! {
                                <>
                                    <div class="menu-item" onclick={toggle}>
                                        { icon(ic) }<span>{ label }</span>
                                        <span class="menu-caret">{ if is_open { "▾" } else { "▸" } }</span>
                                    </div>
                                    if is_open {
                                        <div class="menu-sub-list">
                                            { for options.iter().map(|(id, name)| {
                                                let cb = cb.clone();
                                                let close = close.clone();
                                                let id = id.clone();
                                                let onclick = Callback::from(move |_: MouseEvent| {
                                                    cb.emit(id.clone());
                                                    close.emit(());
                                                });
                                                html! { <div class="menu-item menu-sub-option" onclick={onclick}>{ name }</div> }
                                            }) }
                                        </div>
                                    }
                                </>
                            }
                        }
                    }
                }) }
            </div>
        </>
    }
}

/// A kebab (⋮) button that owns its open state and shows a [`Menu`].
#[derive(Properties, PartialEq)]
pub struct MenuButtonProps {
    pub actions: Vec<MenuAction>,
    #[prop_or_default]
    pub align_right: bool,
    /// Extra classes for the trigger button.
    #[prop_or_default]
    pub btn_class: Classes,
}

#[function_component(MenuButton)]
pub fn menu_button(props: &MenuButtonProps) -> Html {
    let open = use_state(|| false);
    let toggle = {
        let open = open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            open.set(!*open);
        })
    };
    let on_close = {
        let open = open.clone();
        Callback::from(move |_| open.set(false))
    };
    html! {
        <div class="menu-anchor">
            <button class={classes!("ibtn", props.btn_class.clone())} title="More" onclick={toggle}>
                { icon("kebab") }
            </button>
            <Menu open={*open} on_close={on_close} actions={props.actions.clone()} align_right={props.align_right} />
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct TrackListProps {
    pub tracks: Vec<Track>,
    pub liked_ids: Vec<String>,
    /// ID of the track currently playing, highlighted in the list.
    #[prop_or_default]
    pub playing_id: Option<String>,
    /// IDs available offline, marked with an indicator.
    #[prop_or_default]
    pub downloaded_ids: Vec<String>,
    /// (id, name) of user playlists, for the add-to-playlist dropdown.
    pub playlists: Vec<(String, String)>,
    /// Play the track at this index (within this list).
    pub on_play: Callback<usize>,
    pub on_like: Callback<Track>,
    pub on_queue: Callback<Track>,
    pub on_add_to_playlist: Callback<(String, Track)>,
    pub on_open_artist: Callback<String>,
    pub on_open_album: Callback<String>,
    /// When set, rows get a remove button (used inside playlists).
    #[prop_or_default]
    pub on_remove: Option<Callback<usize>>,
}

#[function_component(TrackList)]
pub fn track_list(props: &TrackListProps) -> Html {
    html! {
        <div class="tracklist">
            { for props.tracks.iter().enumerate().map(|(i, t)| html! {
                <TrackRow
                    track={t.clone()}
                    index={i}
                    liked={props.liked_ids.contains(&t.id)}
                    downloaded={props.downloaded_ids.contains(&t.id)}
                    playing={props.playing_id.as_deref() == Some(t.id.as_str())}
                    playlists={props.playlists.clone()}
                    on_play={props.on_play.clone()}
                    on_like={props.on_like.clone()}
                    on_queue={props.on_queue.clone()}
                    on_add_to_playlist={props.on_add_to_playlist.clone()}
                    on_open_artist={props.on_open_artist.clone()}
                    on_open_album={props.on_open_album.clone()}
                    on_remove={props.on_remove.clone()}
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
    playing: bool,
    playlists: Vec<(String, String)>,
    on_play: Callback<usize>,
    on_like: Callback<Track>,
    on_queue: Callback<Track>,
    on_add_to_playlist: Callback<(String, Track)>,
    on_open_artist: Callback<String>,
    on_open_album: Callback<String>,
    on_remove: Option<Callback<usize>>,
}

#[function_component(TrackRow)]
fn track_row(props: &TrackRowProps) -> Html {
    let menu_open = use_state(|| false);
    let t = &props.track;
    let i = props.index;

    let play = {
        let cb = props.on_play.clone();
        Callback::from(move |_: MouseEvent| cb.emit(i))
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
    let mut actions: Vec<MenuAction> = vec![MenuAction::Item {
        icon: "queue",
        label: "Add to queue".into(),
        danger: false,
        cb: {
            let cb = props.on_queue.clone();
            let track = t.clone();
            Callback::from(move |_| cb.emit(track.clone()))
        },
    }];
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

    html! {
        <div class={classes!("trow", props.playing.then_some("playing"), (*menu_open).then_some("menu-open"))}
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
            if props.downloaded {
                <span class="trow-dl" title="Available offline">{ icon("check-circle") }</span>
            }
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

// ------------------------------------------------------------ queue panel

#[derive(Properties, PartialEq)]
pub struct QueuePanelProps {
    pub queue: QueueSnapshot,
    pub on_jump: Callback<usize>,
    pub on_remove: Callback<usize>,
    pub on_clear: Callback<()>,
}

#[function_component(QueuePanel)]
pub fn queue_panel(props: &QueuePanelProps) -> Html {
    let collapsed = use_state(|| false);
    if props.queue.tracks.is_empty() {
        return html! {};
    }
    let toggle = {
        let collapsed = collapsed.clone();
        Callback::from(move |_: MouseEvent| collapsed.set(!*collapsed))
    };
    let clear = {
        let cb = props.on_clear.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(());
        })
    };

    html! {
        <section class="queue-panel">
            <div class="queue-head" onclick={toggle}>
                <span>{ format!("UP NEXT ({})", props.queue.tracks.len()) }</span>
                <button class="ibtn" title="Clear queue" onclick={clear}>{ icon("trash") }</button>
            </div>
            if !*collapsed {
                <div class="queue-list">
                    { for props.queue.tracks.iter().enumerate().map(|(i, t)| {
                        let jump = { let cb = props.on_jump.clone(); Callback::from(move |_| cb.emit(i)) };
                        let remove = {
                            let cb = props.on_remove.clone();
                            Callback::from(move |e: MouseEvent| { e.stop_propagation(); cb.emit(i); })
                        };
                        let current = props.queue.current == Some(i);
                        html! {
                            <div class={classes!("qrow", current.then_some("current"))} onclick={jump}>
                                { cover(&t.cover, "qrow-cover") }
                                <div class="trow-meta">
                                    <div class="trow-title">{ &t.title }</div>
                                    <div class="trow-artist">{ &t.artist }</div>
                                </div>
                                <button class="ibtn" title="Remove from queue" onclick={remove}>{ icon("x") }</button>
                            </div>
                        }
                    }) }
                </div>
            }
        </section>
    }
}

// ------------------------------------------------------------- player bar

#[derive(Properties, PartialEq)]
pub struct PlayerBarProps {
    pub track: Option<Track>,
    pub state: PlaybackState,
    pub progress: Progress,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
    pub on_open_artist: Callback<String>,
    pub on_volume: Callback<f32>,
}

#[function_component(PlayerBar)]
pub fn player_bar(props: &PlayerBarProps) -> Html {
    // While the user drags the seek bar this holds the in-progress position,
    // so live progress ticks from the backend don't fight the drag.
    let scrubbing = use_state(|| None::<f64>);

    let seek_input = {
        let scrubbing = scrubbing.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(pos) = el.value().parse::<f64>() {
                scrubbing.set(Some(pos));
            }
        })
    };
    let seek_commit = {
        let scrubbing = scrubbing.clone();
        Callback::from(move |e: Event| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(pos) = el.value().parse::<f64>() {
                api::fire("seek", json!({ "position": pos }));
            }
            scrubbing.set(None);
        })
    };
    let set_volume = {
        let on_volume = props.on_volume.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(v) = el.value().parse::<f32>() {
                on_volume.emit(v / 100.0);
            }
        })
    };
    // Persist only when the drag ends, so we don't write to disk on every tick.
    let save_volume = Callback::from(move |e: Event| {
        let el: HtmlInputElement = e.target_unchecked_into();
        if let Ok(v) = el.value().parse::<f32>() {
            api::fire("save_volume", json!({ "volume": v / 100.0 }));
        }
    });
    let toggle_like = {
        let track = props.track.clone();
        Callback::from(move |_| {
            if let Some(t) = &track {
                let t = t.clone();
                spawn_local(async move {
                    let _ = api::invoke::<serde_json::Value>("toggle_like", &json!({ "track": t }))
                        .await;
                });
            }
        })
    };

    let loading = props.state == PlaybackState::Loading;
    let playing = props.state == PlaybackState::Playing;
    let duration = props.progress.duration.max(1.0);
    // Show the dragged position while scrubbing, otherwise the live position.
    let seek_pos = scrubbing.unwrap_or(props.progress.position);

    html! {
        <footer class="player">
            <div class="seek-row">
                <span class="time">{ fmt_pos(seek_pos) }</span>
                <input class="seek" type="range" min="0" step="1"
                       max={format!("{}", duration as u64)}
                       value={format!("{}", seek_pos as u64)}
                       oninput={seek_input} onchange={seek_commit} />
                <span class="time">{ fmt_pos(props.progress.duration) }</span>
            </div>
            <div class="player-row">
                <div class="now-playing">
                    if let Some(t) = &props.track {
                        { cover(&t.cover, "np-cover") }
                        <div class="trow-meta">
                            <div class="trow-title">{ &t.title }</div>
                            <div class="trow-artist">
                                { artist_links(&t.artists, &t.artist, &props.on_open_artist) }
                            </div>
                        </div>
                        <button class={classes!("ibtn", "np-like", props.liked.then_some("liked"))}
                                title="Like" onclick={toggle_like}>
                            { icon(if props.liked { "heart" } else { "heart-outline" }) }
                        </button>
                    }
                </div>
                <div class="controls">
                    <button class={classes!("ibtn", props.shuffle.then_some("accent"))} title="Shuffle"
                            onclick={|_| api::fire("toggle_shuffle", json!({}))}>{ icon("shuffle") }</button>
                    <button class="ibtn" title="Previous"
                            onclick={|_| api::fire("prev_track", json!({}))}>{ icon("prev") }</button>
                    <button class={classes!("play-btn", loading.then_some("loading"))} title="Play/Pause"
                            onclick={|_| api::fire("toggle_play", json!({}))}>
                        { icon(if playing || loading { "pause" } else { "play" }) }
                    </button>
                    <button class="ibtn" title="Next"
                            onclick={|_| api::fire("next_track", json!({}))}>{ icon("next") }</button>
                    <button class={classes!("ibtn", (props.repeat != RepeatMode::Off).then_some("accent"))}
                            title="Repeat" onclick={|_| api::fire("cycle_repeat", json!({}))}>
                        { icon(if props.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }) }
                    </button>
                </div>
                <div class="volume">
                    { icon("volume") }
                    <input class="vol-slider" type="range" min="0" max="100"
                           value={format!("{}", (props.volume * 100.0) as u32)}
                           oninput={set_volume} onchange={save_volume} />
                </div>
            </div>
        </footer>
    }
}
