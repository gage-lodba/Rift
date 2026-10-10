//! Home view: library cards grid and the "Recently played" list.

use rift_types::Library;
use yew::prelude::*;

use super::icons::cover;
use super::menu::{MenuAction, MenuButton};
use super::View;

#[derive(Properties, PartialEq)]
pub struct HomeProps {
    /// Shared via Rc so the app can pass it down without a deep clone per render.
    pub library: std::rc::Rc<Library>,
    pub on_nav: Callback<View>,
    pub on_rename_playlist: Callback<String>,
    pub on_delete_playlist: Callback<String>,
    /// Pre-rendered "Recently played" track list (the app owns the callbacks
    /// a `TrackList` needs, so it builds the rows).
    pub recent: Html,
}

#[function_component(HomeView)]
pub fn home_view(props: &HomeProps) -> Html {
    let lib = &props.library;
    let card = |view: View, cover_url: &str, name: &str, count: usize| {
        let on_nav = props.on_nav.clone();
        let v = view.clone();
        html! {
            <button class="card" onclick={Callback::from(move |_| on_nav.emit(v.clone()))}>
                { cover(cover_url, "card-cover") }
                <div class="card-name">{ name }</div>
                <div class="card-sub">{ format!("{count} songs") }</div>
            </button>
        }
    };

    html! {
        <>
            <h2>{ "Your library" }</h2>
            <div class="card-grid">
                { card(View::Liked,
                       lib.liked.first().map(|t| t.cover.as_str()).unwrap_or(""),
                       "Liked Songs", lib.liked.len()) }
                { for lib.playlists.iter().map(|p| {
                    let on_nav = props.on_nav.clone();
                    let v = View::Playlist(p.id.clone());
                    let cover_url = p.tracks.first().map(|t| t.cover.as_str()).unwrap_or("");
                    let actions = vec![
                        MenuAction::Item {
                            icon: "edit",
                            label: "Rename".into(),
                            danger: false,
                            cb: {
                                let cb = props.on_rename_playlist.clone();
                                let id = p.id.clone();
                                Callback::from(move |_| cb.emit(id.clone()))
                            },
                        },
                        MenuAction::Item {
                            icon: "trash",
                            label: "Delete".into(),
                            danger: true,
                            cb: {
                                let cb = props.on_delete_playlist.clone();
                                let id = p.id.clone();
                                Callback::from(move |_| cb.emit(id.clone()))
                            },
                        },
                    ];
                    html! {
                        // A `div`, not a `<button>`: this card holds the kebab
                        // below, and a button inside a button is not defined by
                        // any engine. The primary action is the transparent
                        // `.card-action` overlay instead, so the kebab is a
                        // sibling. See the CSS for the stacking.
                        <div class="card">
                            { cover(cover_url, "card-cover") }
                            <div class="card-foot">
                                <div class="card-text">
                                    <div class="card-name">{ &p.name }</div>
                                    <div class="card-sub">{ format!("{} songs", p.tracks.len()) }</div>
                                </div>
                                // No `stop_propagation` here any more: the kebab
                                // is no longer a descendant of a click handler,
                                // and `MenuButton`'s own toggle stops the click.
                                <div class="card-menu">
                                    <MenuButton actions={actions} align_right=true />
                                </div>
                            </div>
                            // Covers the whole card so pointer and keyboard reach
                            // the same target, and it is a real button, so Enter
                            // and Space work without a role/tabindex trio.
                            <button class="card-action" aria-label={format!("Open playlist {}", p.name)}
                                    onclick={Callback::from(move |_| on_nav.emit(v.clone()))} />
                        </div>
                    }
                }) }
            </div>

            <h2>{ "Recently played" }</h2>
            if lib.recently_played.is_empty() {
                <div class="empty">{ "Nothing played yet. Search for a song to get started." }</div>
            } else {
                { props.recent.clone() }
            }
        </>
    }
}
