//! Home view: library cards grid and the "Recently played" list.

use rift_types::Library;
use yew::prelude::*;

use super::icons::cover;
use super::menu::{MenuAction, MenuButton};
use super::View;

#[derive(Properties, PartialEq)]
pub struct HomeProps {
    pub library: Library,
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
                        <div class="card" onclick={Callback::from(move |_| on_nav.emit(v.clone()))}>
                            { cover(cover_url, "card-cover") }
                            <div class="card-foot">
                                <div class="card-text">
                                    <div class="card-name">{ &p.name }</div>
                                    <div class="card-sub">{ format!("{} songs", p.tracks.len()) }</div>
                                </div>
                                <div class="card-menu">
                                    <MenuButton actions={actions} align_right=true />
                                </div>
                            </div>
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
